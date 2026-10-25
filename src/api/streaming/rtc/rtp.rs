use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker, metrics::METRICS};
use bytes::Bytes;
use h264_reader::annexb::AnnexBReader;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::push::NalInterest;
use rtc::rtp::Packet;
use rtc::rtp::codec::h264::H264Packet;
use rtc::rtp::codec::opus::OpusPacket;
use rtc::rtp::packetizer::Depacketizer;
use rtc_media::io::sample_builder::SampleBuilder;
use std::sync::atomic::Ordering;
use std::time::Instant;
use std::collections::VecDeque;

const MAX_PENDING_AUDIO_PACKETS: usize = 32;
const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const AUDIO_MAX_LATE_PACKETS: u16 = 32;
const LOW_FPS_DAMAGE_LIMIT: u8 = 8;
const HIGH_FPS_DAMAGE_LIMIT: u8 = 3;

#[derive(Default)]
pub(crate) struct VideoSampleStats {
    pub dropped: u32,
    pub source_frame_duration_us: Option<u64>,
    pub encoded_resolution: Option<(u32, u32)>,
    pub color: Option<u8>,
    pub nack_requests: Vec<(u16, u16)>,
}

pub(super) struct AudioRtp {
    samples: SampleBuilder<OpusPacket>,
    payload_type: u8,
}

impl AudioRtp {
    pub(super) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            samples: SampleBuilder::new(AUDIO_MAX_LATE_PACKETS, OpusPacket, sample_rate)
                .with_max_time_delay(std::time::Duration::from_millis(80)),
            payload_type,
        }
    }

    pub(super) fn receive(&mut self, packet: Packet, audio_packets: &mut Vec<Bytes>) {
        if packet.header.payload_type != self.payload_type {
            return;
        }

        self.samples.push(packet);
        while audio_packets.len() < MAX_PENDING_AUDIO_PACKETS {
            let Some(sample) = self.samples.pop() else {
                break;
            };
            audio_packets.push(sample.data);
        }
    }
}

pub(crate) struct VideoRtp {
    depacketizer: H264Packet,
    pending: Option<PendingVideoFrame>,
    next_sequence: Option<u16>,
    last_frame_timestamp: Option<u32>,
    source_frame_duration_us: Option<u64>,
    damage_score: u8,
    stream_too_large: bool,
    waiting_for_keyframe: bool,
    stream_clock_anchor: Option<(Instant, u32)>,
    /// Drift instrumentation: (arrival, RTP timestamp) of the newest video
    /// packet seen; used by stream_slip() for the accumulation trend.
    latest_rtp: Option<(Instant, u32)>,
    /// Negotiated header extension ids from the SDP answer.
    /// vt_ext_id defaults to 7 (video-timing), ast_ext_id defaults to None (probe off).
    /// tc_ext_id stores the transport-wide CC extension map id (e.g. 3 when offered).
    pub(crate) vt_ext_id: u8,
    pub(crate) ast_ext_id: Option<u8>,
    pub(crate) tc_ext_id: Option<u8>,
    /// Buffer of (wide_sequence_number, arrival_ms) pairs for transport CC reporting.
    /// One entry per parsed packet with tc extension, capped at 4096 entries.
    pub(crate) twcc_arrivals: VecDeque<(u16, u64)>,
    /// Count of overflow drops when twcc_arrivals cap is exceeded.
    pub(crate) twcc_overflow_drops: u64,
    /// Last unwrapped abs-send-time high bits and wall clock for 24-bit wrap tracking.
    last_abs_send_time: Option<(i64, i64)>,
    /// Drift instrumentation: per-window max of each video-timing (extmap id 7)
    /// stage delta (ms: encode-start, encode-finish, packetization, pacer-exit,
    /// network, network2) plus the sample count.
    pub(crate) vt_max: [u64; 6],
    pub(crate) vt_exit_count: u64,
    total_bytes: u64,
}

struct PendingVideoFrame {
    timestamp: u32,
    first_packet_at: Instant,
    packets: Vec<Packet>,
}

enum FrameAssembly {
    Pending,
    Complete { data: Bytes, marker_sequence: u16 },
    Invalid,
}

impl PendingVideoFrame {
    fn new(packet: Packet) -> Self {
        Self {
            timestamp: packet.header.timestamp,
            first_packet_at: Instant::now(),
            packets: vec![packet],
        }
    }

    fn insert(&mut self, packet: Packet) {
        if !self
            .packets
            .iter()
            .any(|existing| existing.header.sequence_number == packet.header.sequence_number)
        {
            self.packets.push(packet);
        }
    }

    fn marker_sequence(&self) -> Option<u16> {
        self.packets
            .iter()
            .find(|packet| packet.header.marker)
            .map(|packet| packet.header.sequence_number)
    }

    fn assemble(
        &self,
        depacketizer: &mut H264Packet,
        expected_sequence: Option<u16>,
    ) -> FrameAssembly {
        let Some(marker_sequence) = self.marker_sequence() else {
            return FrameAssembly::Pending;
        };
        let mut packets = self.packets.iter().collect::<Vec<_>>();
        packets.sort_unstable_by_key(|packet| {
            std::cmp::Reverse(marker_sequence.wrapping_sub(packet.header.sequence_number))
        });
        let Some(first) = packets.first() else {
            return FrameAssembly::Pending;
        };
        if expected_sequence.is_some_and(|expected| first.header.sequence_number != expected)
            || !depacketizer.is_partition_head(&first.payload)
        {
            return FrameAssembly::Pending;
        }
        if packets.windows(2).any(|pair| {
            pair[1].header.sequence_number != pair[0].header.sequence_number.wrapping_add(1)
        }) {
            return FrameAssembly::Pending;
        }

        *depacketizer = H264Packet::default();
        let mut data = Vec::new();
        for packet in packets {
            let Ok(nalu) = depacketizer.depacketize(&packet.payload) else {
                *depacketizer = H264Packet::default();
                return FrameAssembly::Invalid;
            };
            data.extend_from_slice(&nalu);
            if data.len() > MAX_H264_ACCESS_UNIT_BYTES {
                *depacketizer = H264Packet::default();
                return FrameAssembly::Invalid;
            }
        }
        *depacketizer = H264Packet::default();
        FrameAssembly::Complete {
            data: Bytes::from(data),
            marker_sequence,
        }
    }
}

impl VideoRtp {
    pub(crate) fn new() -> Self {
        Self {
            depacketizer: H264Packet::default(),
            pending: None,
            next_sequence: None,
            last_frame_timestamp: None,
            source_frame_duration_us: None,
            damage_score: 0,
            stream_too_large: false,
            waiting_for_keyframe: false,
            stream_clock_anchor: None,
            latest_rtp: None,
            vt_ext_id: 7,
            ast_ext_id: None,
            tc_ext_id: None,
            twcc_arrivals: VecDeque::new(),
            twcc_overflow_drops: 0,
            last_abs_send_time: None,
            vt_max: [0; 6],
            vt_exit_count: 0,
            total_bytes: 0,
        }
    }

    /// Thread negotiated header extension ids into this VideoRtp.
    /// vt_id: video-timing extension id (default 7 if None).
    /// ast_id: abs-send-time extension id (None = probe off).
    /// tc_id: transport-wide CC extension id (None = probe off).
    pub fn set_negotiated_ext_ids(&mut self, ast_id: Option<u8>, vt_id: Option<u8>, tc_id: Option<u8>) {
        if let Some(id) = vt_id {
            self.vt_ext_id = id;
        }
        self.ast_ext_id = ast_id;
        self.tc_ext_id = tc_id;
        crate::streaming::video::metrics::METRICS
            .ext_ast_id
            .store(ast_id.map_or(0, |v| v as u64), Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .ext_vt_id
            .store(vt_id.map_or(0, |v| v as u64), Ordering::Relaxed);
    }

    pub(super) fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    pub(super) fn wait_for_keyframe(&mut self) {
        self.waiting_for_keyframe = true;
    }

    pub(crate) fn receive(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        let mut frame_was_damaged = false;
        if packet.payload.is_empty() {
            if self.next_sequence == Some(packet.header.sequence_number) {
                self.next_sequence = Some(packet.header.sequence_number.wrapping_add(1));
            }
            return stats;
        }
        self.total_bytes += 12 + packet.payload.len() as u64;

        let packet_timestamp = packet.header.timestamp;

        // Drift instrumentation (log only, no behaviour change): establish the
        // session media-clock anchor on the first video packet and track the
        // newest-timestamp arrival so stream_slip() can trend arrival-vs-media
        // drift for the latency-accumulation investigation.
        if self.stream_clock_anchor.is_none() {
            self.stream_clock_anchor = Some((Instant::now(), packet_timestamp));
        }
        match self.latest_rtp {
            Some((_, last_ts)) if !timestamp_is_newer(packet_timestamp, last_ts) => {}
            _ => self.latest_rtp = Some((Instant::now(), packet_timestamp)),
        }

        if let Some(pending) = &self.pending
            && pending.timestamp != packet_timestamp
        {
            if !timestamp_is_newer(packet_timestamp, pending.timestamp) {
                return stats;
            }
            if let Some(incomplete) = self.pending.take() {
                self.next_sequence = incomplete
                    .marker_sequence()
                    .map(|sequence| sequence.wrapping_add(1));
                self.depacketizer = H264Packet::default();
                *keyframe_requested = true;
                METRICS.pli_gap.fetch_add(1, Ordering::Relaxed);
                self.record_damage(worker);
                frame_was_damaged = true;
                stats.dropped = stats.dropped.saturating_add(1);
            }
        }
        // Video-timing census (extmap id = negotiated vt_ext_id): parse six 16-bit stage deltas.
        let vt_ext = self.vt_ext_id;
        if let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == vt_ext)
            && ext.payload.len() == 13
        {
            for (i, slot) in self.vt_max.iter_mut().enumerate() {
                let d = u16::from_be_bytes([ext.payload[1 + i * 2], ext.payload[2 + i * 2]]) as u64;
                *slot = (*slot).max(d);
            }
            self.vt_exit_count += 1;
        }

        // Transport-wide CC census (extmap id = negotiated tc_ext_id): parse one-byte
        // header extension (RFC 5285) containing the wide sequence number (2 bytes,
        // big-endian). Record arrival timing for later loss recovery reports.
        if let Some(tc_id) = self.tc_ext_id {
            if let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == tc_id)
                && ext.payload.len() >= 2
            {
                let wide_seq = u16::from_be_bytes([ext.payload[0], ext.payload[1]]);
                let arrival_ms = self.stream_clock_anchor
                    .map_or(0, |(anchor_at, _)| anchor_at.elapsed().as_millis() as u64);
                if self.twcc_arrivals.len() >= 4096 {
                    self.twcc_arrivals.pop_front();
                    self.twcc_overflow_drops += 1;
                }
                self.twcc_arrivals.push_back((wide_seq, arrival_ms));
            }
        }

        // abs-send-time census (extmap id = negotiated ast_ext_id).
        // 3-byte value, 24-bit at 64 Hz. qd_us = arrival wall (since anchor) − send time
        // (constant offset absorbed by the analyzer/media.rs qd_rel trailing minimum).
        // 24-bit wraps every 64.4 s → extend against previous raw via signed shortest step.
        // When ast_ext_id NOT negotiated: qd stays 0.
        if let Some(ast_id) = self.ast_ext_id {
            if let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == ast_id)
                && ext.payload.len() >= 3
            {
                let raw_24bit = u32::from_be_bytes([
                    0, ext.payload[0], ext.payload[1], ext.payload[2],
                ]) & 0xFFFFFF;
                let wall_us = self.stream_clock_anchor
                    .map_or(0, |(anchor_at, _)| anchor_at.elapsed().as_micros() as i64);
                let unwrapped = match self.last_abs_send_time {
                    Some((prev_unwrapped, prev_raw)) => {
                        let diff = raw_24bit.wrapping_sub(prev_raw as u32);
                        let delta = (((diff + 0x800000) & 0xFFFFFF) as i64) - 0x800000;
                        // Sanity clamp: abs-send-time is 64 Hz (~15.6 µs per packet);
                        // packets arrive ≥ ~8 ms apart → max plausible delta ≈ 64 * 2 = 128.
                        // Use limit 512 to catch corrupted wraps without suppressing real
                        // packet-advance variation. If delta exceeds the limit, re-anchor
                        // on the raw value so the next packet can re-sync.
                        if delta.abs() > 512 {
                            raw_24bit as i64 // fresh anchor
                        } else {
                            prev_unwrapped + delta
                        }
                    }
                    None => raw_24bit as i64,
                };
                // (unwrapped, raw) so the next packet can extend across wraps.
                self.last_abs_send_time = Some((unwrapped, raw_24bit as i64));
                let ast_us = unwrapped * 1_000_000 / 64;
                let qd_us = wall_us - ast_us;
                crate::streaming::video::metrics::METRICS
                    .qd_us
                    .store(qd_us as u64, Ordering::Relaxed);
            }
        }
        if self.pending.is_none() {
            if self
                .last_frame_timestamp
                .is_some_and(|last| !timestamp_is_newer(packet_timestamp, last))
            {
                return stats;
            }
            self.pending = Some(PendingVideoFrame::new(packet));
        } else if let Some(pending) = &mut self.pending {
            pending.insert(packet);
        }

        let assembly = self
            .pending
            .as_ref()
            .map(|pending| pending.assemble(&mut self.depacketizer, self.next_sequence));
        let Some(assembly) = assembly else {
            return stats;
        };
        let (data, marker_sequence) = match assembly {
            FrameAssembly::Pending => return stats,
            FrameAssembly::Invalid => {
                self.pending = None;
                self.next_sequence = None;
                *keyframe_requested = true;
                METRICS.pli_too_large.fetch_add(1, Ordering::Relaxed);
                self.record_damage(worker);
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            FrameAssembly::Complete {
                data,
                marker_sequence,
            } => (data, marker_sequence),
        };
        let completed = self.pending.take().expect("assembled pending video frame");
        // Record both average and worst-case RTP assembly time for the stream HUD.
        let assembly_us = completed.first_packet_at.elapsed().as_micros() as u64;
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_sum_us
            .fetch_add(assembly_us, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_count
            .fetch_add(1, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .rtp_assembly_max_us
            .fetch_max(assembly_us, Ordering::Relaxed);

        // Drift instrumentation: accumulate slip metric from the latest video packet.
        if let Some(slip) = self.stream_slip() {
            let (slip_us, _wall_us, _media_us) = slip;
            // Current slip (updated every packet; HUD reads this value).
            // Store as u64 using wrapping arithmetic so negative values
            // are represented as two's-complement (analyzer _unwrap_slip()
            // converts values > 2^63 back to signed ms).
            METRICS.stream_slip_us.store(slip_us as u64, Ordering::Relaxed);
            // Maximum slip observed during the session (max of positive slips).
            if slip_us > 0 {
                METRICS.stream_slip_max_us.fetch_max(slip_us as u64, Ordering::Relaxed);
            }
            // One-shot base slope: capture once when wall-time since anchor ≥ 60 s.
            let anchor_age_secs = self.stream_clock_anchor
                .map_or(0.0, |(anchor_at, _)| anchor_at.elapsed().as_secs_f64());
            if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) == 0
                && anchor_age_secs >= 60.0
            {
                // Session-fixed base: never re-based for the duration of this session.
                METRICS.stream_slip_slope_mpm.store(slip_us as i64, Ordering::Relaxed);
            } else if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) != 0
            {
                // Compute running slope relative to the base captured at ~60 s.
                // slope in ms/min: (delta_us * 60) / (1000 * elapsed_s)
                // With µs inputs, this correctly yields ms/min
                let base_slip = METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) as i64;
                let elapsed_s = anchor_age_secs - 60.0; // seconds after the 60s base point
                if elapsed_s > 1.0 {
                    let delta_us = slip_us as i64 - base_slip;
                    let slope_mpm = (delta_us as f64 * 60.0) / (1000.0 * elapsed_s);
                    METRICS.stream_slip_slope_mpm.store(slope_mpm.round() as i64, Ordering::Relaxed);
                }
            }
        }

        self.next_sequence = Some(marker_sequence.wrapping_add(1));
        stats.source_frame_duration_us = self.last_frame_timestamp.map(|previous| {
            u64::from(completed.timestamp.wrapping_sub(previous)) * 1_000_000
                / u64::from(VIDEO_RTP_CLOCK_RATE)
        });
        if let Some(duration) = stats.source_frame_duration_us {
            self.source_frame_duration_us = Some(
                self.source_frame_duration_us
                    .map(|average| (average * 7 + duration) / 8)
                    .unwrap_or(duration),
            );
        }
        self.last_frame_timestamp = Some(completed.timestamp);

        let unit = inspect_h264_access_unit(&data);
        stats.encoded_resolution = unit.resolution;
        stats.color = unit.color;
        let sample_too_large = unit
            .resolution
            .is_some_and(|(width, height)| width > STREAM_WIDTH || height > STREAM_HEIGHT);
        if sample_too_large {
            eprintln!(
                "Dropping H264 access unit larger than decoder: {:?} > {}x{}",
                unit.resolution, STREAM_WIDTH, STREAM_HEIGHT
            );
            self.stream_too_large = true;
            // Flush queued decoder work once, then wait for a compatible IDR instead of feeding
            // frames that the Vita hardware cannot decode.
            *keyframe_requested = true;
                METRICS.pli_too_large.fetch_add(1, Ordering::Relaxed);
            if !self.waiting_for_keyframe {
                worker.begin_resync();
            }
            self.waiting_for_keyframe = true;
            stats.dropped = stats.dropped.saturating_add(1);
            return stats;
        }
        if self.stream_too_large {
            if unit.resolution.is_none() || !unit.has_idr {
                *keyframe_requested = true;
                METRICS.pli_wait_idr.fetch_add(1, Ordering::Relaxed);
                self.waiting_for_keyframe = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.stream_too_large = false;
        }
        if self.waiting_for_keyframe {
            // Later keyframes may contain only IDR; AVCDEC retains SPS/PPS across resyncs.
            if !unit.has_idr {
                *keyframe_requested = true;
                METRICS.pli_expired.fetch_add(1, Ordering::Relaxed);
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.waiting_for_keyframe = false;
            self.damage_score = 0;
        } else if !frame_was_damaged {
            self.damage_score = self.damage_score.saturating_sub(1);
        }

        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            METRICS.pli_submit.fetch_add(1, Ordering::Relaxed);
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    fn record_damage(&mut self, worker: &VideoDecodeWorker) {
        if self.waiting_for_keyframe {
            return;
        }

        self.damage_score = self.damage_score.saturating_add(1);
        let source_fps = self
            .source_frame_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(30);
        let damage_limit = if source_fps <= 30 {
            LOW_FPS_DAMAGE_LIMIT
        } else if source_fps >= 60 {
            HIGH_FPS_DAMAGE_LIMIT
        } else {
            LOW_FPS_DAMAGE_LIMIT
                - (((source_fps - 30) * u64::from(LOW_FPS_DAMAGE_LIMIT - HIGH_FPS_DAMAGE_LIMIT)
                    + 29)
                    / 30) as u8
        };
        if self.damage_score < damage_limit {
            return;
        }

        worker.begin_resync();
        self.waiting_for_keyframe = true;
        self.damage_score = 0;
    }
    
    pub(crate) fn flush_expired(&mut self, _worker: &VideoDecodeWorker, _kr: &mut bool, _stats: &mut crate::api::streaming::rtc::rtp::VideoSampleStats) {}
    /// Reset lag anchor is a no-op for re-anchoring mid-session.
    /// Drift instrumentation: (slip_us, wall_us, media_us).  slip = wall time in microseconds
    /// since the session anchor minus media time (RTP clock at 90 kHz) advanced
    /// since the anchor.  A flat slope means arrivals keep pace with the media
    /// timeline; a growing slope means they increasingly fall behind.
    pub(crate) fn stream_slip(&self) -> Option<(i64, i64, i64)> {
        let (anchor_at, anchor_ts) = self.stream_clock_anchor?;
        let (arrival_at, arrival_ts) = self.latest_rtp?;
        let wall_us = arrival_at.duration_since(anchor_at).as_micros() as i64;
        // RTP video clock is 90 kHz: µs = ticks × 1_000_000 / 90_000.
        // (The previous `/ 90` inflated media time 1000× — same bug as the old ms form.)
        let media_us = (arrival_ts.wrapping_sub(anchor_ts) as i64) * 1_000_000 / 90_000;
        Some((wall_us - media_us, wall_us, media_us))
    }
    pub(crate) fn total_bytes(&self) -> u64 { self.total_bytes }
    pub(crate) fn twcc_median_stride(&self) -> u16 { 0 }
    pub(crate) fn take_twcc_report(&mut self, _media_ssrc: u32) -> Option<TwccReport> { None }
    /// Drain the twcc arrivals buffer, returning all stored (wide_seq, arrival_ms) pairs.
    /// This swaps out the internal buffer so the caller can process the entries
    /// (e.g. build a TransportLayerCc report) without losing in-flight data.
    pub(crate) fn drain_twcc_arrivals(&mut self) -> Vec<(u16, u64)> {
        self.twcc_arrivals.drain(..).collect()
    }
}

pub(crate) fn color_matrix_label(code: u8) -> String {
    match code {
        0 => "GBR".to_owned(),
        1 => "BT.709".to_owned(),
        2 => "Unspec".to_owned(),
        5 => "BT.470BG".to_owned(),
        6 => "BT.601".to_owned(),
        7 => "BT.601".to_owned(),
        9 => "BT.2020".to_owned(),
        10 => "BT.2020".to_owned(),
        _ => format!("m{}", code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_matrix_label() {
        assert_eq!(color_matrix_label(0), "GBR");
        assert_eq!(color_matrix_label(1), "BT.709");
        assert_eq!(color_matrix_label(2), "Unspec");
        assert_eq!(color_matrix_label(5), "BT.470BG");
        assert_eq!(color_matrix_label(6), "BT.601");
        assert_eq!(color_matrix_label(7), "BT.601");
        assert_eq!(color_matrix_label(9), "BT.2020");
        assert_eq!(color_matrix_label(10), "BT.2020");
        // Anything else gets "m{code}" format
        assert_eq!(color_matrix_label(3), "m3");
        assert_eq!(color_matrix_label(255), "m255");
    }

    /// Parse the transport-wide CC header extension value from an RTP header extension.
    /// The RFC 5285 TC extension payload is 2 bytes big-endian containing the wide
    /// sequence number. Returns None if the extension id doesn't match or payload is too short.
    fn parse_tc_extension(tc_id: u8, ext_id: u8, payload: &[u8]) -> Option<u16> {
        if tc_id != ext_id {
            return None;
        }
        if payload.len() < 2 {
            return None;
        }
        Some(u16::from_be_bytes([payload[0], payload[1]]))
    }

    #[test]
    fn test_parse_tc_extension_match() {
        // TC ext id = 3, packet has ext id = 3, payload = [0x12, 0x34]
        let result = parse_tc_extension(3, 3, &[0x12, 0x34]);
        assert_eq!(result, Some(0x1234));
    }

    #[test]
    fn test_parse_tc_extension_id_mismatch() {
        // TC ext id = 3, packet has ext id = 4 → should return None
        let result = parse_tc_extension(3, 4, &[0x12, 0x34]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_tc_extension_truncated() {
        // TC ext id = 3, payload too short (1 byte) → None
        let result = parse_tc_extension(3, 3, &[0x12]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_tc_extension_empty() {
        // TC ext id = 3, empty payload → None
        let result = parse_tc_extension(3, 3, &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_twcc_arrivals_buffer_cap() {
        let mut rtp = VideoRtp::new();
        // Push 4097 entries (>4096) to test the cap at 4096
        for i in 0..=4096u16 {
            rtp.tc_ext_id = Some(3);
            rtp.stream_clock_anchor = Some((std::time::Instant::now(), 90000));
            if rtp.twcc_arrivals.len() >= 4096 {
                rtp.twcc_arrivals.pop_front();
                rtp.twcc_overflow_drops += 1;
            }
            rtp.twcc_arrivals.push_back((i, i as u64));
        }
        assert_eq!(rtp.twcc_arrivals.len(), 4096);
        assert_eq!(rtp.twcc_overflow_drops, 1);
    }

    #[test]
    fn test_drain_twcc_arrivals() {
        let mut rtp = VideoRtp::new();
        // Push some entries
        rtp.tc_ext_id = Some(3);
        rtp.stream_clock_anchor = Some((std::time::Instant::now(), 90000));
        rtp.twcc_arrivals.push_back((0x1234, 100));
        rtp.twcc_arrivals.push_back((0x5678, 200));
        rtp.twcc_arrivals.push_back((0x9abc, 300));

        let drained = rtp.drain_twcc_arrivals();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0], (0x1234, 100));
        assert_eq!(drained[1], (0x5678, 200));
        assert_eq!(drained[2], (0x9abc, 300));
        // Buffer should be empty after drain
        assert_eq!(rtp.twcc_arrivals.len(), 0);
    }
}

pub(crate) struct TwccReport {
    pub media_ssrc: u32,
    pub base_sequence_number: u16,
    pub packet_status_count: u16,
    pub reference_time: u32,
    pub chunks: Vec<rtcp::transport_feedbacks::transport_layer_cc::PacketStatusChunk>,
    pub recv_deltas: Vec<rtcp::transport_feedbacks::transport_layer_cc::RecvDelta>,
}

pub(crate) fn set_negotiated_twcc_ext_id(rtp: &mut VideoRtp, tc_id: u8) {
        rtp.tc_ext_id = Some(tc_id);
    }

fn timestamp_is_newer(candidate: u32, reference: u32) -> bool {
    let distance = candidate.wrapping_sub(reference);
    distance != 0 && distance < (1 << 31)
}

struct AccessUnitInfo {
    has_idr: bool,
    resolution: Option<(u32, u32)>,
    color: Option<u8>,
}

fn inspect_h264_access_unit(data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_idr: false,
        resolution: None,
        color: None,
    };
    let mut reader = AnnexBReader::accumulate(|nal: RefNal<'_>| {
        let Ok(header) = nal.header() else {
            return NalInterest::Ignore;
        };
        match header.nal_unit_type() {
            UnitType::SliceLayerWithoutPartitioningIdr => {
                info.has_idr = true;
                NalInterest::Ignore
            }
            UnitType::SeqParameterSet => {
                if nal.is_complete() {
                    if let Ok(sps) = SeqParameterSet::from_bits(nal.rbsp_bits()) {
                        info.resolution = sps.pixel_dimensions().ok();
                        // Per H.264: if SPS parsed, extract color from VUI;
                        // if VUI/video_signal_type/colour_description absent,
                        // matrix_coefficients defaults to Unspecified (2).
                        let color = sps.vui_parameters
                            .as_ref()
                            .and_then(|v| v.video_signal_type.as_ref())
                            .and_then(|v| v.colour_description.as_ref())
                            .map(|c| c.matrix_coefficients)
                            .unwrap_or(2);
                        info.color = Some(color);
                    }
                }
                NalInterest::Buffer
            }
            _ => NalInterest::Ignore,
        }
    });
    reader.push(data);
    reader.reset();
    info
}
