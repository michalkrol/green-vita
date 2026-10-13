//! RTX SSRC Rewrite Interceptor
//!
//! The rtc crate's endpoint handler drops inbound RTX packets because it only
//! routes by primary SSRC. This interceptor rewrites the SSRC of inbound RTX
//! packets from the RTX SSRC to the primary SSRC so they reach our receive path.
//!
//! The interceptor learns the RTX->primary SSRC mapping from `bind_remote_stream`
//! calls (which are made for both primary and RTX streams with their respective
//! payload types) and consults the negotiated RTX payload type map to know
//! which payload types correspond to RTX.

use crate::api::streaming::rtc::media::{is_negotiated_rtx_pt, rtx_apt_for};
use crate::streaming::video::metrics::METRICS;
use rtc_interceptor::{Interceptor, NoopInterceptor, Packet, StreamInfo, TaggedPacket};
use rtc::sansio::Protocol;
use rtc::shared::error::{Error, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Global primary video SSRC known to the app (set by VideoReceiver::open).
/// Used as a last-resort fallback when RTX binding payload type is 0.
static PRIMARY_VIDEO_SSRC: AtomicU32 = AtomicU32::new(0);

/// Sets the primary video SSRC from the app (VideoReceiver::open).
/// Called once when the primary video track is opened.
pub(crate) fn set_primary_video_ssrc(ssrc: u32) {
    PRIMARY_VIDEO_SSRC.store(ssrc, Ordering::Release);
}

/// Gets the primary video SSRC for fallback use.
fn get_primary_video_ssrc() -> Option<u32> {
    let ssrc = PRIMARY_VIDEO_SSRC.load(Ordering::Acquire);
    if ssrc != 0 { Some(ssrc) } else { None }
}

pub struct RtxSsrcRewrite<P: Interceptor> {
    next: P,
    /// Maps RTX SSRC -> primary SSRC. Populated lazily at bind_remote_stream time.
    rtx_to_primary: HashMap<u32, u32>,
    /// Maps primary payload type -> primary SSRC. Used to identify the primary
    /// stream when we see the RTX stream's bind_remote_stream call.
    primary_by_pt: HashMap<u8, u32>,
    /// Count of rewrites performed (for logging first few).
    rewrite_count: AtomicU32,
    /// Whether we've logged the mapping learning.
    logged_mapping: AtomicU32,
    /// Debug log line counter (capped at ~200 lines).
    debug_log_lines: AtomicU64,
}

impl<P: Interceptor> RtxSsrcRewrite<P> {
    pub fn new(next: P) -> Self {
        Self {
            next,
            rtx_to_primary: HashMap::new(),
            primary_by_pt: HashMap::new(),
            rewrite_count: AtomicU32::new(0),
            logged_mapping: AtomicU32::new(0),
            debug_log_lines: AtomicU64::new(0),
        }
    }

    /// Writes a line to the capped debug log at ux0:data/xcloud-rust/rtx-debug.log.
    /// Stops writing after ~200 lines. Never panics or blocks on IO errors.
    fn debug_log(&self, line: &str) {
        // Cap at ~200 lines total
        let current = self.debug_log_lines.fetch_add(1, Ordering::Relaxed);
        if current >= 200 {
            return;
        }
        // Use the same pattern as SDP dumps - write via fs_utils
        let _ = crate::fs_utils::write_file_truncating(
            "ux0:data/xcloud-rust/rtx-debug.log",
            format!("{line}\n"),
        );
    }
}

impl<P: Interceptor> Protocol<TaggedPacket, TaggedPacket, ()> for RtxSsrcRewrite<P> {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<()> {
        if let Packet::Rtp(pkt) = &mut msg.message {
            let ssrc = pkt.header.ssrc;
            let pt = pkt.header.payload_type;

            // Check if this is an RTX packet we need to rewrite
            if is_negotiated_rtx_pt(pt) {
                METRICS.rtx_read_rtx_pt.fetch_add(1, Ordering::Relaxed);
                
                // Resolve rewrite target in priority order:
                // 1. Explicit rtx_ssrc -> primary_ssrc mapping from bindings
                // 2. Primary SSRC for rtx_apt_for(pkt_pt) (works even when RTX binding had PT 0)
                // 3. Single known primary video SSRC (if exactly one)
                // 4. App-known primary SSRC (from VideoReceiver::open)
                // 5. Give up
                let primary_ssrc: Option<u32> = if let Some(&primary_ssrc) = self.rtx_to_primary.get(&ssrc) {
                    // Source 1: explicit mapping learned from bindings
                    Some(primary_ssrc)
                } else if let Some(primary_pt) = rtx_apt_for(pt)
                    && let Some(&primary_ssrc) = self.primary_by_pt.get(&primary_pt)
                {
                    // Source 2: primary SSRC for the RTX PT's associated primary PT
                    Some(primary_ssrc)
                } else if self.primary_by_pt.len() == 1 {
                    // Source 3: exactly one primary video SSRC known
                    self.primary_by_pt.values().next().copied()
                } else if let Some(primary_ssrc) = get_primary_video_ssrc() {
                    // Source 3b: app-known primary SSRC (last resort)
                    Some(primary_ssrc)
                } else {
                    // Source 4: give up
                    None
                };

                if let Some(primary_ssrc) = primary_ssrc {
                    if primary_ssrc != ssrc {
                        let count = self.rewrite_count.fetch_add(1, Ordering::Relaxed);
                        if count < 20 {
                            self.debug_log(&format!(
                                "REWRITE #{}: SSRC {} -> {} (PT={})",
                                count + 1, ssrc, primary_ssrc, pt
                            ));
                        }
                        METRICS.rtx_read_rewritten.fetch_add(1, Ordering::Relaxed);
                        pkt.header.ssrc = primary_ssrc;
                    }
                } else {
                    // No usable mapping
                    METRICS.rtx_read_unmapped.fetch_add(1, Ordering::Relaxed);
                    let warn_count = self.rewrite_count.load(Ordering::Relaxed);
                    if warn_count < 100 || warn_count % 100 == 0 {
                        eprintln!(
                            "RtxSsrcRewrite: WARNING - RTX packet for unmapped SSRC={} PT={} (no bind_remote_stream mapping learned yet)",
                            ssrc, pt
                        );
                    }
                    if METRICS.rtx_read_unmapped.load(Ordering::Relaxed) <= 20 {
                        self.debug_log(&format!(
                            "UNMAPPED: SSRC={} PT={} (no mapping source worked)",
                            ssrc, pt
                        ));
                    }
                }
            }
        }
        self.next.handle_read(msg)
    }

    fn poll_read(&mut self) -> Option<Self::Rout> {
        self.next.poll_read()
    }

    fn handle_write(&mut self, msg: TaggedPacket) -> Result<()> {
        self.next.handle_write(msg)
    }

    fn poll_write(&mut self) -> Option<Self::Wout> {
        self.next.poll_write()
    }

    fn handle_event(&mut self, evt: ()) -> Result<()> {
        self.next.handle_event(evt)
    }

    fn poll_event(&mut self) -> Option<Self::Eout> {
        self.next.poll_event()
    }

    fn handle_timeout(&mut self, now: Instant) -> Result<()> {
        self.next.handle_timeout(now)
    }

    fn poll_timeout(&mut self) -> Option<Instant> {
        self.next.poll_timeout()
    }

    fn close(&mut self) -> Result<()> {
        self.next.close()
    }
}

impl<P: Interceptor> Interceptor for RtxSsrcRewrite<P> {
    fn bind_local_stream(&mut self, info: &StreamInfo) {
        self.next.bind_local_stream(info);
    }

    fn unbind_local_stream(&mut self, info: &StreamInfo) {
        self.next.unbind_local_stream(info);
    }

    fn bind_remote_stream(&mut self, info: &StreamInfo) {
        let pt = info.payload_type;
        let ssrc = info.ssrc;

        METRICS.rtx_bind_total.fetch_add(1, Ordering::Relaxed);

        // Debug log every bind_remote_stream (capped)
        if METRICS.rtx_bind_total.load(Ordering::Relaxed) <= 20 {
            let bind_type = if pt == 0 {
                "unknown_pt(0)"
            } else if is_negotiated_rtx_pt(pt) {
                "rtx"
            } else {
                "primary"
            };
            self.debug_log(&format!(
                "BIND #{}: SSRC={} PT={} type={}",
                METRICS.rtx_bind_total.load(Ordering::Relaxed),
                ssrc, pt, bind_type
            ));
        }

        // Check if this payload type is a negotiated RTX type
        if is_negotiated_rtx_pt(pt) {
            METRICS.rtx_bind_rtx.fetch_add(1, Ordering::Relaxed);
            // This is an RTX stream binding. Look up its primary payload type.
            if let Some(primary_pt) = rtx_apt_for(pt) {
                // Find the primary SSRC we recorded for this primary PT
                if let Some(&primary_ssrc) = self.primary_by_pt.get(&primary_pt) {
                    // Learn the mapping: RTX SSRC -> primary SSRC
                    self.rtx_to_primary.insert(ssrc, primary_ssrc);
                    METRICS.rtx_map_learned.fetch_add(1, Ordering::Relaxed);
                    eprintln!(
                        "RtxSsrcRewrite: learned mapping RTX_SSRC={} -> PRIMARY_SSRC={} (RTX_PT={}, PRIMARY_PT={})",
                        ssrc, primary_ssrc, pt, primary_pt
                    );
                } else {
                    // Primary not seen yet - this can happen if RTX binds before primary.
                    // We'll catch it on the next bind_remote_stream for the primary.
                    // But this shouldn't happen in practice since primary binds first.
                    eprintln!(
                        "RtxSsrcRewrite: WARNING - RTX stream bound before primary! RTX_SSRC={} RTX_PT={} PRIMARY_PT={} (no primary SSRC known yet)",
                        ssrc, pt, primary_pt
                    );
                }
            } else {
                eprintln!(
                    "RtxSsrcRewrite: WARNING - RTX PT {} has no negotiated APT mapping! SSRC={}",
                    pt, ssrc
                );
            }
        } else if pt != 0 {
            // This is a primary stream binding (PT != 0). Record its SSRC by payload type.
            // PT=0 is not a valid primary PT - record only as unknown to avoid corrupting primary_by_pt.
            METRICS.rtx_bind_primary.fetch_add(1, Ordering::Relaxed);
            self.primary_by_pt.insert(pt, ssrc);
            eprintln!(
                "RtxSsrcRewrite: recorded primary SSRC={} for PT={}",
                ssrc, pt
            );
        } else {
            // PT == 0 and not a negotiated RTX type - unknown binding
            METRICS.rtx_bind_unknown_pt.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "RtxSsrcRewrite: bind_remote_stream with PT=0 (unknown), SSRC={} - ignored for primary_by_pt",
                ssrc
            );
        }

        self.next.bind_remote_stream(info);
    }

    fn unbind_remote_stream(&mut self, info: &StreamInfo) {
        self.next.unbind_remote_stream(info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::streaming::rtc::media::{set_negotiated_rtx_map};
    use rtc_interceptor::{NoopInterceptor, StreamInfo, TaggedPacket, Packet};
    use rtc::rtp::Packet as RtpPacket;
    use rtc::rtp::header::Header;
    use bytes::Bytes;
    use std::collections::HashMap;
    use std::time::Instant;

    fn setup_rtx_map() {
        let mut map = HashMap::new();
        map.insert(112, 102); // RTX PT 112 -> primary PT 102
        set_negotiated_rtx_map(map);
    }

    #[test]
    fn test_rtx_rewrite_basic() {
        setup_rtx_map();
        let mut chain = RtxSsrcRewrite::new(NoopInterceptor::new());

        // Bind primary stream first (PT 102)
        let primary_info = StreamInfo {
            ssrc: 1000,
            payload_type: 102,
            mime_type: "video/H264".to_string(),
            clock_rate: 90000,
            ..Default::default()
        };
        chain.bind_remote_stream(&primary_info);

        // Bind RTX stream (PT 112, apt=102)
        let rtx_info = StreamInfo {
            ssrc: 2000,
            payload_type: 112,
            mime_type: "video/rtx".to_string(),
            clock_rate: 90000,
            sdp_fmtp_line: "apt=102".to_string(),
            ..Default::default()
        };
        chain.bind_remote_stream(&rtx_info);

        // Now test handle_read with an RTX packet
        let rtp_pkt = RtpPacket {
            header: Header {
                ssrc: 2000,
                payload_type: 112,
                sequence_number: 1,
                timestamp: 90000,
                ..Default::default()
            },
            payload: Bytes::from_static(b"\x00\x01\x00\x00\x00\x00"), // OSN=1 + payload
            ..Default::default()
        };

        let tagged = TaggedPacket {
            now: Instant::now(),
            transport: Default::default(),
            message: Packet::Rtp(rtp_pkt),
        };

        chain.handle_read(tagged).unwrap();
        let result = chain.poll_read();

        assert!(result.is_some());
        if let Some(TaggedPacket { message: Packet::Rtp(pkt), .. }) = result {
            assert_eq!(pkt.header.ssrc, 1000, "SSRC should be rewritten to primary");
            assert_eq!(pkt.header.payload_type, 112, "PT should remain RTX PT");
        } else {
            panic!("Expected RTP packet");
        }
    }

    #[test]
    fn test_rtx_rewrite_pt0_fallback() {
        // Test the fallback when RTX binding has PT=0 (the main bug we're fixing)
        setup_rtx_map();
        let mut chain = RtxSsrcRewrite::new(NoopInterceptor::new());

        // Bind primary stream first (PT 102)
        let primary_info = StreamInfo {
            ssrc: 1000,
            payload_type: 102,
            mime_type: "video/H264".to_string(),
            clock_rate: 90000,
            ..Default::default()
        };
        chain.bind_remote_stream(&primary_info);

        // Bind RTX stream with PT=0 (simulating the crate bug where RTX binding gets PT 0)
        let rtx_info = StreamInfo {
            ssrc: 2000,
            payload_type: 0, // PT=0 - the bug case
            mime_type: "video/rtx".to_string(),
            clock_rate: 90000,
            sdp_fmtp_line: "apt=102".to_string(),
            ..Default::default()
        };
        chain.bind_remote_stream(&rtx_info);

        // The PT=0 RTX binding should NOT be recorded in primary_by_pt
        // Now send an RTX packet with the actual negotiated RTX PT (112)
        let rtp_pkt = RtpPacket {
            header: Header {
                ssrc: 2000,
                payload_type: 112, // Actual RTX PT on the wire
                sequence_number: 1,
                timestamp: 90000,
                ..Default::default()
            },
            payload: Bytes::from_static(b"\x00\x01\x00\x00\x00\x00"),
            ..Default::default()
        };

        let tagged = TaggedPacket {
            now: Instant::now(),
            transport: Default::default(),
            message: Packet::Rtp(rtp_pkt),
        };

        chain.handle_read(tagged).unwrap();
        let result = chain.poll_read();

        // Should still rewrite using fallback source 2 (primary_by_pt[rtx_apt_for(112)])
        assert!(result.is_some());
        if let Some(TaggedPacket { message: Packet::Rtp(pkt), .. }) = result {
            assert_eq!(pkt.header.ssrc, 1000, "SSRC should be rewritten to primary via fallback");
            assert_eq!(pkt.header.payload_type, 112, "PT should remain RTX PT");
        } else {
            panic!("Expected RTP packet");
        }
    }

    #[test]
    fn test_rtx_rewrite_single_primary_fallback() {
        // Test fallback when only one primary SSRC is known (source 3)
        setup_rtx_map();
        let mut chain = RtxSsrcRewrite::new(NoopInterceptor::new());

        // Bind primary stream (PT 102)
        let primary_info = StreamInfo {
            ssrc: 1000,
            payload_type: 102,
            mime_type: "video/H264".to_string(),
            clock_rate: 90000,
            ..Default::default()
        };
        chain.bind_remote_stream(&primary_info);

        // RTX stream binds with PT=0 (so no mapping learned)
        let rtx_info = StreamInfo {
            ssrc: 2000,
            payload_type: 0,
            mime_type: "video/rtx".to_string(),
            clock_rate: 90000,
            sdp_fmtp_line: "apt=102".to_string(),
            ..Default::default()
        };
        chain.bind_remote_stream(&rtx_info);

        // But RTX packet comes in with negotiated RTX PT 112
        // Should fallback to single known primary (source 3)
        let rtp_pkt = RtpPacket {
            header: Header {
                ssrc: 2000,
                payload_type: 112,
                sequence_number: 1,
                timestamp: 90000,
                ..Default::default()
            },
            payload: Bytes::from_static(b"\x00\x01\x00\x00\x00\x00"),
            ..Default::default()
        };

        let tagged = TaggedPacket {
            now: Instant::now(),
            transport: Default::default(),
            message: Packet::Rtp(rtp_pkt),
        };

        chain.handle_read(tagged).unwrap();
        let result = chain.poll_read();

        assert!(result.is_some());
        if let Some(TaggedPacket { message: Packet::Rtp(pkt), .. }) = result {
            assert_eq!(pkt.header.ssrc, 1000, "SSRC should be rewritten via single-primary fallback");
        } else {
            panic!("Expected RTP packet");
        }
    }
}