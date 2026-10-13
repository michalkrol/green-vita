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
use rtc_interceptor::{Interceptor, NoopInterceptor, Packet, StreamInfo, TaggedPacket};
use rtc::sansio::Protocol;
use rtc::shared::error::{Error, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

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
}

impl<P: Interceptor> RtxSsrcRewrite<P> {
    pub fn new(next: P) -> Self {
        Self {
            next,
            rtx_to_primary: HashMap::new(),
            primary_by_pt: HashMap::new(),
            rewrite_count: AtomicU32::new(0),
            logged_mapping: AtomicU32::new(0),
        }
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
                if let Some(&primary_ssrc) = self.rtx_to_primary.get(&ssrc) {
                    if primary_ssrc != ssrc {
                        let count = self.rewrite_count.fetch_add(1, Ordering::Relaxed);
                        if count < 5 {
                            eprintln!(
                                "RtxSsrcRewrite: rewrote SSRC {} -> {} (PT={}, rewrite #{})",
                                ssrc, primary_ssrc, pt, count + 1
                            );
                        }
                        pkt.header.ssrc = primary_ssrc;
                    }
                } else {
                    // RTX packet for unknown SSRC - log once per SSRC
                    let warn_count = self.rewrite_count.load(Ordering::Relaxed);
                    if warn_count < 100 || warn_count % 100 == 0 {
                        eprintln!(
                            "RtxSsrcRewrite: WARNING - RTX packet for unmapped SSRC={} PT={} (no bind_remote_stream mapping learned yet)",
                            ssrc, pt
                        );
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

        // Check if this payload type is a negotiated RTX type
        if is_negotiated_rtx_pt(pt) {
            // This is an RTX stream binding. Look up its primary payload type.
            if let Some(primary_pt) = rtx_apt_for(pt) {
                // Find the primary SSRC we recorded for this primary PT
                if let Some(&primary_ssrc) = self.primary_by_pt.get(&primary_pt) {
                    // Learn the mapping: RTX SSRC -> primary SSRC
                    self.rtx_to_primary.insert(ssrc, primary_ssrc);
                    if self.logged_mapping.fetch_or(1, Ordering::Relaxed) == 0 {
                        eprintln!(
                            "RtxSsrcRewrite: learned mapping RTX_SSRC={} -> PRIMARY_SSRC={} (RTX_PT={}, PRIMARY_PT={})",
                            ssrc, primary_ssrc, pt, primary_pt
                        );
                    } else {
                        eprintln!(
                            "RtxSsrcRewrite: learned mapping RTX_SSRC={} -> PRIMARY_SSRC={} (RTX_PT={}, PRIMARY_PT={})",
                            ssrc, primary_ssrc, pt, primary_pt
                        );
                    }
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
        } else {
            // This is a primary stream binding. Record its SSRC by payload type.
            self.primary_by_pt.insert(pt, ssrc);
            eprintln!(
                "RtxSsrcRewrite: recorded primary SSRC={} for PT={}",
                ssrc, pt
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
    use rtc_interceptor::{NoopInterceptor, StreamInfo, TaggedPacket, Packet};
    use rtc::rtp::Packet as RtpPacket;
    use rtc::rtp::header::Header;
    use bytes::Bytes;
    use std::time::Instant;

    #[test]
    fn test_rtx_rewrite_basic() {
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
}