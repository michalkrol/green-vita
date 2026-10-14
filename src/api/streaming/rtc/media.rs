use crate::api::streaming::rtc::rtp;
use crate::streaming::video::{DecodedFrame, DecoderConfig, DirectVideoOutput, VideoDecodeWorker, MetricsLog};
use anyhow::Result;
use bytes::Bytes;
use rtc::media_stream::MediaStreamTrackId;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use rtc::rtp::Packet;
use rtc::rtp_transceiver::RTCRtpReceiverId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const STREAM_STATS_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_REMB_INTERVAL: Duration = Duration::from_millis(500);
const TWCC_INTERVAL: Duration = Duration::from_millis(15);
/// Send TWCC immediately when this many packets have accumulated since last send,
/// preventing large I-frame bursts from looking like bufferbloat to the rate controller.
const TWCC_PACKET_THRESHOLD: usize = 8;
/// Shared REMB bitrate knob accessible from main thread.
pub(crate) static REMB_BPS: AtomicU32 = AtomicU32::new(5_000_000);
/// REMB override: when set (>0), sent instead of dynamic REMB to drain console queue.
pub(crate) static REMB_OVERRIDE_BPS: AtomicU32 = AtomicU32::new(0);
/// REMB shock: brief deep drop in requested bitrate to drain the console's send/encode queue.
pub(crate) const REMB_SHOCK_BPS: u32 = 150_000;
/// Window during which the shock is active.
const REMB_SHOCK_WINDOW: Duration = Duration::from_secs(2);
/// Shock state: AtomicU64 storing milliseconds since PROCESS_START (0 = off).
static SHOCK_BPS: AtomicU64 = AtomicU64::new(0);
/// Arbitrary stable local RTCP sender SSRC (we transmit no RTP of our own).
const LOCAL_RTCP_SENDER_SSRC: u32 = 1;

/// When true, NACK feedback packets are generated and sent to request
/// retransmissions of missing RTP packets. Set to false to trade visual artefacts
/// for latency on congested links, per an explicit product decision.
pub(crate) const NACK_ENABLED: bool = true;

/// Process start reference for monotonic timestamps used by RTCP SR anchoring
/// and capture-to-arrival latency. Initialized once at first use.
pub(crate) static PROCESS_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Negotiated RTX payload type -> primary H.264 payload type mapping from the
/// answer SDP. Replaced on every answer (re-negotiation).
static NEGOTIATED_RTX_MAP: LazyLock<RwLock<HashMap<u8, u8>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Gets the negotiated RTX payload type mapping.
pub(crate) fn negotiated_rtx_map() -> HashMap<u8, u8> {
    NEGOTIATED_RTX_MAP.read().unwrap().clone()
}

/// Sets the negotiated RTX payload type mapping from the answer SDP.
/// Overwrites any existing mapping (re-negotiation support).
pub(crate) fn set_negotiated_rtx_map(map: HashMap<u8, u8>) {
    *NEGOTIATED_RTX_MAP.write().unwrap() = map;
}

/// Checks if a payload type is a negotiated RTX type.
pub(crate) fn is_negotiated_rtx_pt(payload_type: u8) -> bool {
    negotiated_rtx_map().contains_key(&payload_type)
}

/// Arms the REMB shock for REMB_SHOCK_WINDOW from now.
/// Idempotent: re-arming extends the shock window.
pub(crate) fn request_remb_shock() {
    let now_ms = PROCESS_START.elapsed().as_millis() as u64;
    SHOCK_BPS.store(now_ms + REMB_SHOCK_WINDOW.as_millis() as u64, Ordering::Relaxed);
}

/// Checks whether the REMB shock is currently armed.
/// The shock stores an absolute expiry (ms since process start); it is active
/// while the current elapsed time is still within that window.
pub(crate) fn is_remb_shock_armed() -> bool {
    let elapsed = PROCESS_START.elapsed().as_millis() as u64;
    let shock_expiry = SHOCK_BPS.load(Ordering::Relaxed);
    shock_expiry > 0 && elapsed <= shock_expiry
}

/// Gets the primary H.264 payload type for a negotiated RTX payload type.
pub(crate) fn rtx_apt_for(payload_type: u8) -> Option<u8> {
    negotiated_rtx_map().get(&payload_type).copied()
}

#[derive(Default)]
struct VideoStats {
    dropped: u64,
    decode_errors: u64,
    last_sample_duration_us: Option<u64>,
    encoded_resolution: Option<(u32, u32)>,
}

pub(crate) struct VideoReceiver {
    track_id: Option<MediaStreamTrackId>,
    receiver_id: Option<RTCRtpReceiverId>,
    ssrc: Option<u32>,
    rtp: rtp::VideoRtp,
    pub(crate) decoder: VideoDecodeWorker,
    pub(crate) latest_frame: Option<(u64, DecodedFrame)>,
    next_frame_id: u64,
    pub(crate) received_packet: bool,
    last_stats_report: Instant,
    last_bw_bytes: u64,
    last_remb_at: Option<Instant>,
    last_twcc_at: Option<Instant>,
    twcc_fb_pkt_count: u8,
    stats: VideoStats,
    /// Configurable REMB interval (ms).
    remb_interval_ms: u32,
    /// Whether TWCC delay injection is enabled (throttle on overshoot).
    twcc_delay_injection: bool,
    /// Maximum TWCC delay ramp multiplier.
    twcc_delay_ramp_max: u8,
    /// Base REMB ceiling from user settings (hard_bandwidth_cap * bitrate or 15Mbps).
    remb_base_cap_bps: u32,
    /// Once-per-second CSV log of stream metrics; `None` off-device or after a
    /// write failure.  See `streaming/video/metrics_log.rs`.
    metrics_log: Option<MetricsLog>,
}

impl VideoReceiver {
    pub(crate) fn new(
        config: DecoderConfig,
        direct_output: Arc<DirectVideoOutput>,
        _video_fps: u32,
        remb_interval_ms: u32,
        twcc_delay_injection: bool,
        twcc_delay_ramp_max: u8,
        remb_base_cap_bps: u32,
    ) -> Result<Self> {
        let decoder = VideoDecodeWorker::spawn(config, direct_output)?;
        Ok(Self::init(
            decoder,
            remb_interval_ms,
            twcc_delay_injection,
            twcc_delay_ramp_max,
            remb_base_cap_bps,
        ))
    }

    fn init(
        decoder: VideoDecodeWorker,
        remb_interval_ms: u32,
        twcc_delay_injection: bool,
        twcc_delay_ramp_max: u8,
        remb_base_cap_bps: u32,
    ) -> Self {
        Self {
            track_id: None,
            receiver_id: None,
            ssrc: None,
            rtp: rtp::VideoRtp::new(),
            decoder,
            latest_frame: None,
            next_frame_id: 0,
            received_packet: false,
            last_stats_report: Instant::now(),
            last_bw_bytes: 0,
            last_remb_at: None,
            last_twcc_at: None,
            twcc_fb_pkt_count: 0,
            stats: VideoStats::default(),
            remb_interval_ms,
            twcc_delay_injection,
            twcc_delay_ramp_max,
            remb_base_cap_bps,
            metrics_log: MetricsLog::open(),
        }
    }

    pub(crate) fn open(
        &mut self,
        track_id: MediaStreamTrackId,
        receiver_id: RTCRtpReceiverId,
        ssrc: u32,
    ) {
        // Guard against clobbering: if already opened for a different SSRC,
        // log loudly and ignore. The primary video track should only open once.
        if let Some(existing_ssrc) = self.ssrc {
            if existing_ssrc != ssrc {
                eprintln!(
                    "VideoReceiver::open() called with different SSRC! existing={} new={} - IGNORING",
                    existing_ssrc, ssrc
                );
                return;
            }
            // Same SSRC - allow re-open (idempotent), but log if track_id/receiver_id differ
            if self.track_id.as_ref() != Some(&track_id) {
                eprintln!(
                    "VideoReceiver::open() track_id changed for same SSRC {}: {:?} -> {:?}",
                    ssrc, self.track_id, track_id
                );
            }
            if self.receiver_id != Some(receiver_id) {
                eprintln!(
                    "VideoReceiver::open() receiver_id changed for same SSRC {}: {:?} -> {:?}",
                    ssrc, self.receiver_id, receiver_id
                );
            }
            return;
        }
        self.track_id = Some(track_id);
        self.receiver_id = Some(receiver_id);
        self.ssrc = Some(ssrc);
        // Publish primary video SSRC for RTX interceptor fallback (source 3b)
        crate::api::streaming::rtc::rtx_interceptor::set_primary_video_ssrc(ssrc);
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(
        &mut self,
        _peer: &mut RTCPeerConnection,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) {
        self.received_packet = true;
        let sample_stats = self.rtp.receive(&self.decoder, packet, keyframe_requested);
        if !sample_stats.nack_requests.is_empty() {
            crate::streaming::video::metrics::METRICS
                .rtp_gaps
                .fetch_add(sample_stats.nack_requests.len() as u64, Ordering::Relaxed);
            // Note: rtcp_nack_sent is now incremented in send_nacks() when actual
            // NACK feedback packets are emitted, not from local gap detection.
        }
        self.stats.dropped = self
            .stats
            .dropped
            .saturating_add(sample_stats.dropped as u64);
        if sample_stats.source_frame_duration_us.is_some() {
            self.stats.last_sample_duration_us = sample_stats.source_frame_duration_us;
        }
        if sample_stats.encoded_resolution.is_some() {
            self.stats.encoded_resolution = sample_stats.encoded_resolution;
        }
    }

    pub(crate) fn drain_decoder(&mut self, keyframe_requested: &mut bool) {
        // Expire grace-held packets on the pump tick so a burst followed by silence
        // (static screen) cannot strand them waiting for a newer packet to arrive.
        {
            let mut expired_stats = rtp_stats_scratch();
            self.rtp
                .flush_expired(&self.decoder, keyframe_requested, &mut expired_stats);
            self.stats.dropped = self
                .stats
                .dropped
                .saturating_add(expired_stats.dropped as u64);
        }
        let mut decode_errors = 0u64;
        while let Some(result) = self
            .decoder
            .latest_result
            .lock()
            .ok()
            .and_then(|mut latest| latest.take())
        {
            match result {
                Ok(frame) => {
                    self.next_frame_id = self.next_frame_id.wrapping_add(1);
                    self.latest_frame = Some((self.next_frame_id, frame));
                }
                Err(error) => {
                    eprintln!("Failed to decode H264 video frame: {error}");
                    decode_errors = decode_errors.saturating_add(1);
                    *keyframe_requested = true;
                }
            }
        }

        if decode_errors > 0 {
            self.stats.decode_errors = self.stats.decode_errors.saturating_add(decode_errors);
            self.decoder.reset_decoder();
            self.rtp.wait_for_keyframe();
        }
    }

    /// Re-baseline the glass-lag measurement (e.g. after a backlog flush) without
    /// touching decoder or keyframe state.
    pub(crate) fn reset_lag_anchor(&mut self) {
        self.rtp.reset_lag_anchor();
    }

    /// Reset the slip anchor and associated metrics to session start state.
    pub(crate) fn reset_slip_anchor(&mut self) {
        self.rtp.reset_slip_anchor();
    }

    pub(crate) fn request_keyframe(&self, peer: &mut RTCPeerConnection) {
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id)
        {
            // PLI (PT=206, FMT=1) — requests a fresh keyframe from the Xbox encoder.
            // Use LOCAL_RTCP_SENDER_SSRC to match the SSRC used by other RTCP
            // feedback (REMB, TWCC) so the console recognizes the feedback source.
            let pli = rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                media_ssrc: ssrc,
            };
            let _ = receiver.write_rtcp(vec![Box::new(pli)]);
        }
    }

    /// Generates and sends NACK feedback for missing RTP sequences detected during
    /// access-unit assembly. Called once per pump tick.
    pub(crate) fn send_nacks(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !NACK_ENABLED {
            // NACKs disabled: trade visual artefacts for latency on congested links,
            // per an explicit product decision.  Inbound RTX packets are still handled
            // normally if they arrive.
            return;
        }
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        let Some(mut receiver) = peer.rtp_receiver(receiver_id) else {
            return;
        };
        let nack_packets = self.rtp.generate_nack_packets(ssrc, now);
        if nack_packets.is_empty() {
            return;
        }
        // Count actual NACK feedback packets emitted (each may carry multiple sequence numbers).
        let nack_count = nack_packets.len() as u64;
        crate::streaming::video::metrics::METRICS
            .rtcp_nack_sent
            .fetch_add(nack_count, Ordering::Relaxed);
        let rtcp_packets: Vec<Box<dyn rtcp::Packet>> = nack_packets
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn rtcp::Packet>)
            .collect();
        let _ = receiver.write_rtcp(rtcp_packets);
    }

    /// Sends Receiver Estimated Max Bitrate to keep the console's estimate fresh.
    pub(crate) fn send_remb(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };

        // If REMB override is set, send that value instead of dynamic REMB.
        // This is used by the drain governor to force bitrate down and drain the
        // console's send/encode buffer.
        let override_bps = REMB_OVERRIDE_BPS.load(Ordering::Relaxed);
        if override_bps > 0 {
            // Respect the remb_interval_ms rate limit
            let remb_interval = Duration::from_millis(self.remb_interval_ms as u64);
            if self
                .last_remb_at
                .is_some_and(|sent_at| now.duration_since(sent_at) < remb_interval)
            {
                return; // Not due yet, don't send override yet
            }
            self.last_remb_at = Some(now);
            // Log what we actually asked for
            crate::streaming::video::metrics::METRICS
                .remb_bps
                .store(override_bps as u64, Ordering::Relaxed);
            crate::streaming::video::metrics::METRICS
                .remb_sent
                .fetch_add(1, Ordering::Relaxed);
            if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
                let remb =
                    rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate {
                        sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                        bitrate: override_bps as f32,
                        ssrcs: vec![ssrc],
                    };
                let _ = receiver.write_rtcp(vec![Box::new(remb)]);
            }
            return;
        }

        // Check if shock window has expired
        let shock_expiry = SHOCK_BPS.load(Ordering::Relaxed);
        if shock_expiry > 0 {
            let elapsed = PROCESS_START.elapsed().as_millis() as u64;
            if elapsed > shock_expiry {
                // Window expired: clear shock, restore base cap, fall through to normal path
                SHOCK_BPS.store(0, Ordering::Relaxed);
                REMB_BPS.store(self.remb_base_cap_bps, Ordering::Relaxed);
            }
        }

        // Check if shock is armed (still within window)
        if is_remb_shock_armed() {
            // While shock is armed, send REMB_SHOCK_BPS respecting the remb_interval rate limit
            let remb_interval = Duration::from_millis(self.remb_interval_ms as u64);
            if self
                .last_remb_at
                .is_some_and(|sent_at| now.duration_since(sent_at) < remb_interval)
            {
                return; // Not due yet, return without clearing the shock
            }
            self.last_remb_at = Some(now);
            // Log what we actually asked for, so a session CSV shows the feedback loop.
            crate::streaming::video::metrics::METRICS
                .remb_bps
                .store(REMB_SHOCK_BPS as u64, Ordering::Relaxed);
            crate::streaming::video::metrics::METRICS
                .remb_sent
                .fetch_add(1, Ordering::Relaxed);
            if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
                let remb =
                    rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate {
                        sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                        bitrate: REMB_SHOCK_BPS as f32,
                        ssrcs: vec![ssrc],
                    };
                let _ = receiver.write_rtcp(vec![Box::new(remb)]);
            }
            // Do NOT clear SHOCK_BPS — stay armed for the full 2 s window
            return;
        }

        // Normal dynamic path
        self.adjust_remb_dynamically(now);

        let bitrate = REMB_BPS.load(Ordering::Relaxed) as f32;
        let remb_interval = Duration::from_millis(self.remb_interval_ms as u64);
        if self
            .last_remb_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < remb_interval)
        {
            return;
        }
        self.last_remb_at = Some(now);
        // Log what we actually asked for, so a session CSV shows the feedback loop.
        crate::streaming::video::metrics::METRICS
            .remb_bps
            .store(bitrate as u64, Ordering::Relaxed);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            let remb =
                rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate {
                    sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                    bitrate,
                    ssrcs: vec![ssrc],
                };
            let _ = receiver.write_rtcp(vec![Box::new(remb)]);
        }
    }

    /// Dynamically adjusts REMB bitrate based on RTCP feedback and network conditions.
    /// Called before each REMB send to adapt to changing conditions.
    fn adjust_remb_dynamically(&mut self, now: Instant) {
        use crate::streaming::video::metrics::METRICS;
        use std::sync::atomic::Ordering;

        let base_cap = self.remb_base_cap_bps;
        let mut target = base_cap;

        // Read RTCP Receiver Report metrics.
        // The RR handler stores an integer percent (5% -> 5).
        // NOTE: this value comes from the Xbox's RTCP Receiver Report about the
        // client's non-existent outbound stream, and is observed at 0.0% in
        // practice, so the loss ladder is advisory rather than currently load-bearing.
        let loss_pct = METRICS.rtcp_rr_loss_fraction.load(Ordering::Relaxed) as f32;
        let jitter = METRICS.rtcp_rr_jitter.load(Ordering::Relaxed) as f32;
        let stream_lag_us = METRICS.stream_lag_us.load(Ordering::Relaxed);

        // 1. Loss-based reduction: >5% loss → -30%, >2% → -15%, >1% → -5%
        if loss_pct > 5.0 {
            target = target.saturating_mul(7) / 10;  // -30%
        } else if loss_pct > 2.0 {
            target = target.saturating_mul(85) / 100;  // -15%
        } else if loss_pct > 1.0 {
            target = target.saturating_mul(95) / 100;  // -5%
        }

        // 2. Jitter-based reduction: high jitter indicates congestion
        // Jitter in RTP timestamp units (90kHz clock). >1000 = ~11ms
        if jitter > 2000.0 {
            target = target.saturating_mul(8) / 10;  // -20%
        } else if jitter > 1000.0 {
            target = target.saturating_mul(9) / 10;  // -10%
        }

        // 3. Lag-based reduction: sustained lag growth above the recent best
        // (the metric is drift-free; see rtp::lag_sample) → console queue backing up
        if stream_lag_us > 100_000 {  // >100ms
            target = target.saturating_mul(8) / 10;  // -20%
        } else if stream_lag_us > 50_000 {  // >50ms
            target = target.saturating_mul(9) / 10;  // -10%
        }

        // 4. Recovery: slow ramp up when conditions are good
        // Only recover if: low loss (<1%), low jitter (<500), low lag (<30ms)
        let conditions_good = loss_pct < 1.0 && jitter < 500.0 && stream_lag_us < 30_000;
        if conditions_good {
            let current = REMB_BPS.load(Ordering::Relaxed);
            if current < base_cap {
                // Ramp up by 5% per adjustment, capped at base_cap
                let increased = current.saturating_add(current / 20).min(base_cap);
                target = target.max(increased);
            }
        }

        // 5. Hard floor: never go below 500kbps (minimum viable stream)
        const MIN_REMB_BPS: u32 = 500_000;
        target = target.max(MIN_REMB_BPS);

        // Apply the new target
        REMB_BPS.store(target, Ordering::Release);

        // Debug log when significant change
        let current = REMB_BPS.load(Ordering::Relaxed);
        if (current as i32 - target as i32).abs() > (base_cap as i32 / 20) {
            eprintln!("REMB adjusted: {} -> {} kbps (loss={:.1}% jitter={:.0} lag={}ms)",
                current / 1000, target / 1000, loss_pct, jitter, stream_lag_us / 1000);
        }
    }

    /// Manual transport-cc feedback: per-packet arrival deltas for the console's
    /// annotated RTP. The pcap of a smooth Safari session showed ~17 of these per
    /// second; the console's sender pacing depends on them.
    pub(crate) fn send_twcc(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        // Hybrid trigger: send every 15ms minimum OR immediately when N packets
        // accumulate (catches I-frame bursts before the console's rate controller
        // interprets delayed feedback as bufferbloat).
        let enough_packets = self.rtp.twcc_annotation_count() >= TWCC_PACKET_THRESHOLD;
        let time_elapsed = self
            .last_twcc_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < TWCC_INTERVAL);
        if !enough_packets && time_elapsed {
            return;
        }
        let current_bps = self.rtp.current_bitrate_bps();
        // +10% hysteresis: delay injection only triggers when measured bitrate
        // exceeds the REMB target by 10%, preventing GCC oscillation from noise.
        let ceiling_bps = REMB_BPS.load(Ordering::Relaxed).saturating_mul(11) / 10;
        let Some(report) = self
            .rtp
            .take_twcc_report(ssrc, current_bps, ceiling_bps, self.twcc_delay_injection, self.twcc_delay_ramp_max)
        else {
            return;
        };
        self.last_twcc_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            use rtcp::transport_feedbacks::transport_layer_cc::TransportLayerCc;
            let twcc = TransportLayerCc {
                sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                media_ssrc: report.media_ssrc,
                base_sequence_number: report.base_sequence_number,
                packet_status_count: report.packet_status_count,
                reference_time: report.reference_time,
                fb_pkt_count: self.twcc_fb_pkt_count,
                packet_chunks: report.chunks,
                recv_deltas: report.recv_deltas,
            };
            self.twcc_fb_pkt_count = self.twcc_fb_pkt_count.wrapping_add(1);
            let result = receiver.write_rtcp(vec![Box::new(twcc)]);
            if let Err(error) = result {
                eprintln!("twcc write_rtcp failed: {error}");
            }
        }
    }

    pub(crate) fn total_drops(&self) -> u64 {
        self.stats.dropped
    }

    pub(crate) fn status(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_stats_report) < STREAM_STATS_INTERVAL {
            return None;
        }
        let stats_window = now.duration_since(self.last_stats_report);
        self.last_stats_report = now;

        // Publish TWCC stride forensics (median sequence gap between annotated
        // packets) into the summary line rendered below.
        crate::streaming::video::metrics::METRICS
            .twcc_stride
            .store(self.rtp.twcc_median_stride() as u64, Ordering::Relaxed);

        // Publish inbound video bandwidth (kbps over the elapsed window) for the
        // Vita-link-headroom investigation.
        {
            let total = self.rtp.total_bytes();
            let kbps = if stats_window.as_secs_f64() > 0.0 {
                ((total.saturating_sub(self.last_bw_bytes)) as f64 * 8.0
                    / stats_window.as_secs_f64()
                    / 1000.0) as u64
            } else {
                0
            };
            crate::streaming::video::metrics::METRICS
                .video_bandwidth_kbps
                .store(kbps, Ordering::Relaxed);
            self.last_bw_bytes = total;
        }

        // Compute SR-anchored absolute latency metrics once per second.
        // Requires: SR anchor exists, NTP > 0, and newest received RTP timestamp available.
        {
            use crate::streaming::video::metrics::METRICS;
            let sr_anchor_ntp = METRICS.sr_anchor_ntp.load(Ordering::Relaxed);
            let sr_anchor_rtp_ts = METRICS.sr_anchor_rtp_ts.load(Ordering::Relaxed);
            let sr_anchor_local_us = METRICS.sr_anchor_local_us.load(Ordering::Relaxed);
            let sr_anchor_local_wall_us = METRICS.sr_anchor_local_wall_us.load(Ordering::Relaxed);
            let newest_received_ts = self.rtp.newest_received_ts;

            if sr_anchor_ntp > 0 && sr_anchor_local_us > 0 && sr_anchor_local_wall_us > 0
                && newest_received_ts.is_some()
            {
                let sr_anchor_ntp_ms = sr_anchor_ntp as u64;
                // Age of the SR anchor in milliseconds
                let sr_age_ms = ((PROCESS_START.elapsed().as_micros() as u64 - sr_anchor_local_us) / 1000) as i64;
                // Console unix ms minus client wall ms: alignment offset
                let sr_clock_offset_ms = (sr_anchor_ntp_ms - 2_208_988_800_000) as i64
                    - (sr_anchor_local_wall_us / 1000) as i64;
                // RTP timestamp delta from SR anchor (90 kHz clock)
                let newest_rtp_ts = newest_received_ts.unwrap() as u32;
                let delta_ts = newest_rtp_ts.wrapping_sub(sr_anchor_rtp_ts as u32);
                // Guard against stale anchor or wrap: >10s delta is invalid
                if delta_ts <= 900_000 {
                    // Console media time in microseconds:
                    //   (NTP unix ms) * 1000 + (RTP delta * 1_000_000 / 90_000)
                    let console_media_unix_us = (sr_anchor_ntp_ms as i64 - 2_208_988_800_000) * 1000
                        + delta_ts as i64 * 1_000_000 / 90_000;
                    // Now wall at "now", extrapolated from SR arrival
                    let now_wall_us = sr_anchor_local_wall_us
                        + (PROCESS_START.elapsed().as_micros() as u64 - sr_anchor_local_us);
                    let capture_to_arrival_us = now_wall_us as i64 - console_media_unix_us;

                    // Store per-second metrics
                    METRICS.capture_to_arrival_us.store(capture_to_arrival_us, Ordering::Relaxed);
                    METRICS.capture_to_arrival_max_us.store(capture_to_arrival_us, Ordering::Relaxed);
                    METRICS.sr_age_ms.store(sr_age_ms, Ordering::Relaxed);
                    METRICS.sr_clock_offset_ms.store(sr_clock_offset_ms, Ordering::Relaxed);
                }
            }
        }

        let snapshot = crate::streaming::video::drain_metrics();
        let performance = crate::streaming::video::format_summary(&snapshot);
        let source_fps = self
            .stats
            .last_sample_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(0);
        let encoded_resolution = self
            .stats
            .encoded_resolution
            .map(|(width, height)| format!("{width}x{height}"))
            .unwrap_or_else(|| "?".to_owned());
        
        if let Some(log) = self.metrics_log.as_mut() {
            let (enc_width, enc_height) = self.stats.encoded_resolution.unwrap_or((0, 0));
            log.write_row(
                &snapshot,
                &crate::streaming::video::RowExtras {
                    src_fps: source_fps,
                    enc_width,
                    enc_height,
                    waiting_keyframe: self.rtp.waiting_for_keyframe(),
                    drops: self.stats.dropped,
                    decode_errors: self.stats.decode_errors,
                },
            );
        }
        
        Some(format!(
            "enc:{encoded_resolution} srcfps:{source_fps} {performance} wait:{} drop:{} err:{}",
            u8::from(self.rtp.waiting_for_keyframe()),
            self.stats.dropped,
            self.stats.decode_errors,
        ))
    }

    /// Process incoming RTCP packets from the console (SenderReport, ReceiverReport, etc.)
    /// Updates internal metrics for dynamic REMB adjustment.
    pub(crate) fn handle_rtcp(&mut self, rtcp_packets: &[Box<dyn rtcp::Packet>]) {
        use rtcp::receiver_report::ReceiverReport;
        use rtcp::sender_report::SenderReport;
        use rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack;
        use rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
        use rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
        use crate::streaming::video::metrics::METRICS;
        use std::time::Instant;

        // Use process start reference for monotonic time, not Instant::now().elapsed() which is ~0
        let sr_arrival_us = PROCESS_START.elapsed().as_micros() as u64;

        for packet in rtcp_packets {
            // Sender Report from Xbox: contains NTP timestamp for RTT calculation
            if let Some(sr) = packet.as_any().downcast_ref::<SenderReport>() {
                // Skip SRs with zero NTP time (invalid/unspecified)
                if sr.ntp_time == 0 {
                    eprintln!("RTCP SR skipped: ntp_time is zero");
                    continue;
                }
                // sr.ntp_time = wall clock when report sent (ms since 1900)
                // sr.rtp_time = corresponding RTP timestamp (90 kHz clock)
                // Capture all three values as an anchor for capture-to-arrival latency
                eprintln!("RTCP SR from Xbox: ssrc={} ntp={} rtp_ts={} pkt_cnt={} oct_cnt={}",
                    sr.ssrc, sr.ntp_time, sr.rtp_time, sr.packet_count, sr.octet_count);

                // Store the SR anchor: NTP timestamp, RTP timestamp, and local arrival instant
                METRICS.sr_anchor_ntp.store(sr.ntp_time, Ordering::Relaxed);
                METRICS.sr_anchor_rtp_ts.store(sr.rtp_time as u64, Ordering::Relaxed);
                METRICS.sr_anchor_local_us.store(sr_arrival_us, Ordering::Relaxed);
                // Also store the client wall clock at SR arrival (µs since UNIX epoch)
                let now_wall_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;
                METRICS.sr_anchor_local_wall_us.store(now_wall_us, Ordering::Relaxed);
                // Also keep the legacy single-field metric for compatibility
                METRICS.rtcp_sr_ntp_timestamp.store(sr.ntp_time, Ordering::Relaxed);
            }

            // Receiver Report from Xbox: how Xbox is receiving our (non-existent) stream
            // Actually Xbox sends RR for the stream IT sends (our inbound)
            if let Some(rr) = packet.as_any().downcast_ref::<ReceiverReport>() {
                for report in &rr.reports {
                    let loss_frac = report.fraction_lost as f32 / 256.0;
                    let jitter = report.jitter as f32;
                    let last_seq = report.last_sequence_number;
                    let total_lost = report.total_lost;

                    eprintln!("RTCP RR from Xbox: ssrc={} loss={:.1}% jitter={} last_seq={} lost={}",
                        report.ssrc, loss_frac * 100.0, jitter, last_seq, total_lost);

                    // Store for dynamic REMB
                    METRICS.rtcp_rr_loss_fraction.store((loss_frac * 100.0) as u64, Ordering::Relaxed);
                    METRICS.rtcp_rr_jitter.store(jitter as u64, Ordering::Relaxed);
                }
            }

            // NACK from Xbox: wants retransmission of our (non-existent) outbound stream
            if let Some(nack) = packet.as_any().downcast_ref::<TransportLayerNack>() {
                eprintln!("RTCP NACK from Xbox: ssrc={} nacks={:?}", nack.media_ssrc, nack.nacks);
            }

            // PLI from Xbox: wants keyframe from our (non-existent) outbound stream
            if let Some(pli) = packet.as_any().downcast_ref::<PictureLossIndication>() {
                eprintln!("RTCP PLI from Xbox: ssrc={}", pli.media_ssrc);
            }

            // FIR from Xbox: wants keyframe from our (non-existent) outbound stream
            if let Some(fir) = packet.as_any().downcast_ref::<FullIntraRequest>() {
                eprintln!("RTCP FIR from Xbox: {:?}", fir.fir);
            }
        }
    }
}

fn rtp_stats_scratch() -> crate::api::streaming::rtc::rtp::VideoSampleStats {
    crate::api::streaming::rtc::rtp::VideoSampleStats::default()
}

pub(crate) struct AudioReceiver {
    track_id: Option<MediaStreamTrackId>,
    rtp: rtp::AudioRtp,
    pub(crate) packets: Vec<Bytes>,
}

impl AudioReceiver {
    pub(crate) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            track_id: None,
            rtp: rtp::AudioRtp::new(sample_rate, payload_type),
            packets: Vec::new(),
        }
    }

    pub(crate) fn open(&mut self, track_id: MediaStreamTrackId) {
        self.track_id = Some(track_id);
    }

    pub(crate) fn handles(&self, track_id: &MediaStreamTrackId) -> bool {
        self.track_id.as_ref() == Some(track_id)
    }

    pub(crate) fn receive(&mut self, packet: Packet) {
        self.rtp.receive(packet, &mut self.packets);
    }
}
