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
const LOCAL_RTCP_SENDER_SSRC: u32 = 1;

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
    twcc_fb_pkt_count: u8,
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
            twcc_fb_pkt_count: 0,
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

    /// Sends Receiver Estimated Max Bitrate to keep the console's estimate fresh.
    /// Currently not called; kept for quick re-enable.
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

    /// Manual transport-cc feedback: per-packet arrival deltas for the console's
    /// annotated RTP. The pcap of a smooth Safari session showed ~17 of these per
    /// second; the console's sender pacing depends on them.
    pub(crate) fn send_twcc(&mut self, peer: &mut RTCPeerConnection, now: Instant) {
        let (Some(receiver_id), Some(ssrc)) = (self.receiver_id, self.ssrc) else {
            return;
        };
        if self
            .last_twcc_at
            .is_some_and(|sent_at| now.duration_since(sent_at) < TWCC_INTERVAL)
        {
            return;
        }
        let Some(report) = self.rtp.take_twcc_report(ssrc) else {
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

        // Read current metrics for HUD display first (without draining).
        // The actual draining happens later for CSV logging.
        let performance = crate::streaming::video::video_performance_summary();

        // Drain all per-second metrics into a snapshot for CSV logging.
        // Since video_performance_summary() uses load() (not swap), the metrics
        // are still intact and will be properly captured by drain_metrics().
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
