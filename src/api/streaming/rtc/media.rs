use crate::api::streaming::rtc::rtp;
use crate::streaming::video::{self, DecodedFrame, DecoderConfig, DirectVideoOutput, VideoDecodeWorker, STREAM_HEIGHT, STREAM_WIDTH};
use anyhow::Result;
use bytes::Bytes;
use rtc::media_stream::MediaStreamTrackId;
use crate::api::streaming::rtc::peer::FeedbackPeer as RTCPeerConnection;
use rtc::rtp::Packet;
use rtc::rtp_transceiver::RTCRtpReceiverId;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use crate::streaming::video::metrics::{drain_metrics, METRICS};
use crate::api_xbox::streaming::control::input::{SEND_RENDER_TIMING_METADATA, queue_video_metadata};

const STREAM_STATS_INTERVAL: Duration = Duration::from_secs(1);
const REMB_INTERVAL: Duration = Duration::from_millis(500);
const TWCC_INTERVAL: Duration = Duration::from_millis(100);
/// RR-first probe (31/10): Chrome sends RR ~1.2/s (pcap 20261030: median
/// 0.83 s inter-arrival) — match that family.
const RR_INTERVAL: Duration = Duration::from_millis(800);
/// One-variable gate (A2 method): RTCP Receiver Reports on/off. The passive
/// SR sensor inside send_rr is NOT gated — it observes, never transmits.
const SEND_RR: bool = true;
/// Matches the SDP bitrate cap so pacing never becomes the limit.
/// Deliberate UNDER-report of the link capacity, sent to the console every
/// `REMB_INTERVAL` (policy: latency over quality).
///
/// The Vita's 2.4 GHz hop measures ~3.5-5 Mbps sustained (spikes to 7.5), and the
/// previous fixed 15 Mbps announcement told the sender there was headroom it does not
/// have: it overfilled the AP queue, and with no surplus capacity the queue never
/// drained - the accumulating-latency bug.  REMB is also the ONLY rate signal the
/// console gets from us (transport-cc is stubbed in this base).
///
/// Next step: derive this from the bitrate-cap setting (clamped, no-cap ceiling) and
/// from measured delivered throughput.
const REMB_BITRATE_BPS: f32 = 15_000_000.0;
/// Arbitrary stable local RTCP sender SSRC (we transmit no RTP of our own).
/// 2c: also the sender_ssrc of the vendored TWCC feedback Recorder.
pub(crate) const LOCAL_RTCP_SENDER_SSRC: u32 = 1;

#[derive(Default)]
struct VideoStats {
    dropped: u64,
    decode_errors: u64,
    last_sample_duration_us: Option<u64>,
    encoded_resolution: Option<(u32, u32)>,
    color: Option<u8>,
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
    pub(crate) packets: Vec<Bytes>,
    last_stats_report: Instant,
    last_bw_bytes: u64,
    last_remb_at: Option<Instant>,
    last_twcc_at: Option<Instant>,
    last_rr_at: Option<Instant>,
    stats: VideoStats,
    direct_output: Arc<DirectVideoOutput>,
}

impl VideoReceiver {
    pub(crate) fn new(
        config: DecoderConfig,
        direct_output: Arc<DirectVideoOutput>,
        video_fps: u32,
    ) -> Result<Self> {
        let decoder = VideoDecodeWorker::spawn(config, Arc::clone(&direct_output))?;
        Ok(Self::init(decoder, config, direct_output, video_fps))
    }

    fn init(decoder: VideoDecodeWorker, _config: DecoderConfig, direct_output: Arc<DirectVideoOutput>, _video_fps: u32) -> Self {
        Self {
            track_id: None,
            receiver_id: None,
            ssrc: None,
            rtp: rtp::VideoRtp::new(),
            decoder,
            latest_frame: None,
            next_frame_id: 0,
            received_packet: false,
            packets: Vec::new(),
            last_stats_report: Instant::now(),
            last_bw_bytes: 0,
            last_remb_at: None,
            last_twcc_at: None,
            last_rr_at: None,
            stats: VideoStats::default(),
            direct_output,
        }
    }

    pub(crate) fn open(
        &mut self,
        track_id: MediaStreamTrackId,
        receiver_id: RTCRtpReceiverId,
        ssrc: u32,
    ) {
        self.track_id = Some(track_id);
        self.receiver_id = Some(receiver_id);
        self.ssrc = Some(ssrc);
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
            // Gaps are typically the console skipping AUs under load, not network loss.
            // NACKing creates a feedback loop (throttled egress → deeper queue → more skips).
            // Count for diagnostics; do not send NACKs.
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
        if sample_stats.color.is_some() {
            self.stats.color = sample_stats.color;
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

    pub(crate) fn request_keyframe(&self, peer: &mut RTCPeerConnection) {        // A PLI needs both identifiers recorded when the remote video track was opened.
        if let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc)
            && let Some(mut receiver) = peer.rtp_receiver(receiver_id)
        {
            let pli = rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication {
                sender_ssrc: 0,
                media_ssrc: ssrc,
            };
            let _ = receiver.write_rtcp(vec![Box::new(pli)]);
        }
    }

    /// Sends Receiver Estimated Max Bitrate — CONSTANT 15 Mbps @500 ms to keep
    /// the console's estimate fresh. VERIFIED 2026-10-30: this IS called every
    /// pump (session.rs) — the old "Currently not called" note was false.
    /// Chrome dumps: Chrome negotiates goog-remb but sends ZERO REMB packets;
    /// the console paces from TWCC instead. So this is our own experiment
    /// with weak prior (verdict #8 ruled computed REMB wrong; the const
    /// sidesteps computation but advertises 15 Mbps ≫ wifi capacity anyway).
    pub(crate) fn send_remb(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_remb_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < REMB_INTERVAL)
        {
            return;
        }
        self.last_remb_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            let remb =
                rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate {
                    sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
                    bitrate: REMB_BITRATE_BPS,
                    ssrcs: vec![ssrc],
                };
            let _ = receiver.write_rtcp(vec![Box::new(remb)]);
        }
    }

    /// 2c (2026-10-30): MANUAL transport-cc feedback — the vendored Recorder
    /// (twcc/, fed from VideoRtp::receive()) is drained here every
    /// TWCC_INTERVAL (100 ms, unchanged from the interceptor era; Safari's
    /// ~17/s ≈ 59 ms noted but not adopted — one variable per build).
    /// Built packets carry sender_ssrc/media_ssrc/fb_pkt_count internally
    /// (recorder.rs). Proof-of-send = twcc_sent + rtcp_rtpfb_any at the
    /// socket flush; the console annotates 100% of packets when negotiated
    /// (session 154412: rtp_ext_twcc = 202,571). Flag-gated per A2 — flip
    /// MANUAL_TWCC (rtp.rs) for the on/off A/B. History: the interceptor-era
    /// send_twcc never sent a packet (take_twcc_report -> None, and pre-2a
    /// the offer carried no extmaps at all).
    pub(crate) fn send_twcc(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !rtp::MANUAL_TWCC {
            return;
        }
        let (Some(receiver_id), Some(_ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_twcc_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < TWCC_INTERVAL)
        {
            return;
        }
        let packets = self.rtp.take_twcc_feedback();
        if packets.is_empty() {
            return;
        }
        self.last_twcc_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            if let Err(error) = receiver.write_rtcp(packets) {
                eprintln!("twcc write_rtcp failed: {error}");
            }
        }
    }

    /// RR-first probe (31/10): send an RTCP Receiver Report every RR_INTERVAL
    /// (Chrome parity — pcap: ~1.2/s), as an RFC 3550 §6.1 COMPOUND
    /// [RR + SDES CNAME] — probe iteration 2: the bare-RR v1 was non-
    /// compliant and may have been discarded (contaminating the first
    /// SR reading). Report block = RFC A.3 stats from VideoRtp;
    /// LSR/DLSR = 0 while no SR has been received (valid per RFC). Passive sensor: while rtc's stats show zero
    /// inbound SRs, ask get_stats whether an SR arrived (RemoteOutbound
    /// reports_sent is fed ONLY by on_rtcp_sr_received — the crate's SR read
    /// path) and store the count to CSV column `rtcp_sr_count`. Answers the
    /// gating question: does the console SR us once we RR?
    pub(crate) fn send_rr(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        if !SEND_RR {
            return;
        }
        let (Some(receiver_id), Some(_ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_rr_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < RR_INTERVAL)
        {
            return;
        }
        let Some(report) = self.rtp.take_reception_report() else {
            return;
        };
        self.last_rr_at = Some(now);
        if let Some(mut receiver) = peer.rtp_receiver(receiver_id) {
            use rtcp::receiver_report::ReceiverReport;
            use rtcp::source_description::{
                SourceDescription, SourceDescriptionChunk, SourceDescriptionItem, SdesType,
            };
            let rr = ReceiverReport {
                ssrc: LOCAL_RTCP_SENDER_SSRC,
                reports: vec![report],
                ..Default::default()
            };
            // RFC 3550 §6.1: RTCP MUST be compound (SR/RR first, SDES CNAME
            // required). v1 sent a bare RR — non-compliant, may be discarded;
            // iteration 2 sends [RR, SDES].
            let sdes = SourceDescription {
                chunks: vec![SourceDescriptionChunk {
                    source: LOCAL_RTCP_SENDER_SSRC,
                    items: vec![SourceDescriptionItem {
                        sdes_type: SdesType::SdesCname,
                        text: Bytes::from_static(b"green-vita"),
                    }],
                }],
            };
            if let Err(error) = receiver.write_rtcp(vec![Box::new(rr), Box::new(sdes)]) {
                eprintln!("rr write_rtcp failed: {error}");
            }
        }
        // Passive SR sensor (ungated): one get_stats probe per interval until
        // the first SR; after each window reset, one probe refreshes the count.
        if METRICS.rtcp_sr_count.load(Ordering::Relaxed) == 0 {
            use rtc::statistics::report::RTCStatsReportEntry;
            use rtc::statistics::stats::RTCStatsType;
            use rtc::statistics::StatsSelector;
            let stats = peer.get_stats(now, StatsSelector::None);
            let mut sr_count = 0u64;
            for entry in stats.iter_by_type(RTCStatsType::RemoteOutboundRTP) {
                if let RTCStatsReportEntry::RemoteOutboundRtp(s) = entry {
                    sr_count = sr_count.max(s.reports_sent);
                }
            }
            if sr_count > 0 {
                METRICS.rtcp_sr_count.store(sr_count, Ordering::Relaxed);
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

        // Read current metrics for HUD display first (without draining).
        // The actual draining happens later for CSV logging.
        let performance = crate::streaming::video::video_performance_summary();

        // Drain all per-second metrics into a snapshot for CSV logging.
        // Since video_performance_summary() uses load() (not swap), the metrics
        // are still intact and will be properly captured by drain_metrics().
        // Compute q_slope_mpm from the queue-delay estimator before draining.
        let q_slope_mpm = self.rtp.q_estimator.trend_us_per_s() * 60.0 / 1000.0;
        METRICS.q_slope_mpm.store(q_slope_mpm.round() as i64, Ordering::Relaxed);
        // 7b: publish the live Q estimate so HUD/CSV q_us is fed (the drain
        // below swaps it into the snapshot; it was never stored before ⇒ 0).
        METRICS.q_us
            .store(self.rtp.q_estimator.q_us(), Ordering::Relaxed);
        let snapshot = drain_metrics();

        // If render timing metadata is enabled, queue a metadata frame.
        if SEND_RENDER_TIMING_METADATA {
            queue_video_metadata(
                0,
                Instant::now(),
            );
        }

        // Compute source fps and negotiated resolution before building extras.
        let source_fps = self
            .stats
            .last_sample_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(0);
        let (enc_width, enc_height) = self
            .stats
            .encoded_resolution
            .unwrap_or((STREAM_WIDTH, STREAM_HEIGHT));

        // Build receiver-level extras for the CSV row.
        let extras = crate::streaming::video::metrics_log::RowExtras {
            src_fps: source_fps,
            enc_width,
            enc_height,
            waiting_keyframe: self.rtp.waiting_for_keyframe(),
            drops: self.stats.dropped,
            decode_errors: self.stats.decode_errors,
        };

        // Write the row to the RAM-buffered CSV log (no file I/O during the session).
        if let Ok(mut guard) = crate::streaming::video::metrics::METRICS_CSV_LOG.lock() {
            if let Some(log) = guard.as_mut() {
                let wrote = log.write_row(&snapshot, &extras);
                let _ = wrote;
            }
        }



        // Compute audio queue depth in ms from pending packets.
// RTP-queue proxy: this tracks the inbound RTP packet queue depth in milliseconds,
// derived from pending packet sizes at 48 kHz 16-bit stereo (192000 bytes/sec).
        // 48 kHz, 16-bit stereo: 1s = 192000 bytes.
        let pending_bytes: usize = self.packets.iter().map(|p| p.len()).sum();
        let queue_ms = (pending_bytes.saturating_mul(1000) / 192000) as u64;
        METRICS.audio_queue_ms.store(queue_ms, Ordering::Relaxed);
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

        let encoded_resolution = self
            .stats
            .encoded_resolution
            .map(|(width, height)| format!("{width}x{height}"))
            .unwrap_or_else(|| "?".to_owned());
        let cs_suffix = self.stats.color
            .map(|code| format!(" cs:{}", crate::api::streaming::rtc::rtp::color_matrix_label(code)))
            .unwrap_or_default();
        Some(format!(
            "enc:{encoded_resolution} srcfps:{source_fps} {performance} wait:{} drop:{} err:{}{cs_suffix}",
            u8::from(self.rtp.waiting_for_keyframe()),
            self.stats.dropped,
            self.stats.decode_errors,
        ))
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