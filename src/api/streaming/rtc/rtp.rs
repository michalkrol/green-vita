use crate::streaming::video::{self, STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker, metrics::METRICS};
use crate::pipeline::clock::queue_delay::QueueDelayEstimator;
use crate::pipeline::media::assembler::{
    seq_bounds_of, AccessUnitAssembler, AssemblyOutcome,
};
use std::sync::LazyLock;
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
    /// Drift instrumentation: per-window max of each video-timing (extmap id 7)
    /// stage delta (ms: encode-start, encode-finish, packetization, pacer-exit,
    /// network, network2) plus the sample count.
    vt_max: [u64; 6],
    vt_exit_count: u64,
    slip_slope_base: Option<(Instant, i64)>,
    total_bytes: u64,
    /// Queue-delay estimator Q from §4.3 (signed µs, never clamped).
    pub(crate) q_estimator: QueueDelayEstimator,
    /// 8a shadow (§3): the new AccessUnitAssembler fed every packet in
    /// parallel with the legacy path. Never authoritative.
    shadow: AccessUnitAssembler,
    /// 8a shadow: pairs legacy and new outcomes by RTP timestamp and counts
    /// matches/mismatches. Never touches legacy state.
    shadow_cmp: ShadowComparator,
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
            slip_slope_base: None,
            vt_max: [0; 6],
            vt_exit_count: 0,
            total_bytes: 0,
            q_estimator: QueueDelayEstimator::new(),
            shadow: AccessUnitAssembler::new(),
            shadow_cmp: ShadowComparator::new(),
        }
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

        crate::streaming::video::rtp_dump_log::record(
            SHADOW_CLOCK.elapsed().as_micros() as u64,
            packet.header.sequence_number,
            packet_timestamp,
            packet.header.marker,
            &packet.payload,
        );

        // 8a shadow (STAGE8-REDO-SPEC §3): feed every non-empty video packet
        // to the new assembler alongside the legacy path. Behaviour-neutral by
        // construction — it never touches pending, worker, keyframe_requested
        // or stats; outcomes only feed the comparator counters. flush_due runs
        // here in shadow only (§3).
        let shadow_now = SHADOW_CLOCK.elapsed().as_micros() as u64;
        if let Some(outcome) = self.shadow.push(
            packet.header.sequence_number,
            packet_timestamp,
            packet.header.marker,
            &packet.payload,
            shadow_now,
        ) {
            self.observe_shadow(outcome, false);
        }
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.observe_shadow(outcome, false);
        }
        if let Some(outcome) = self.shadow.flush_due(shadow_now) {
            self.observe_shadow(outcome, true);
        }
        while let Some(outcome) = self.shadow.poll_outcome() {
            self.observe_shadow(outcome, false);
        }

        // Drift instrumentation (log only, no behaviour change): establish the
        // session media-clock anchor on the first video packet and track the
        // newest-timestamp arrival so stream_slip() can trend arrival-vs-media
        // drift for the latency-accumulation investigation.
        if self.stream_clock_anchor.is_none() {
            self.stream_clock_anchor = Some((Instant::now(), packet_timestamp));
        }
        match self.latest_rtp {
            Some((_, last_ts)) if !timestamp_is_newer(packet_timestamp, last_ts) => {}
            _ => {
                self.latest_rtp = Some((Instant::now(), packet_timestamp));
                // Push queue-delay estimator input per video RTP packet.
                // send_us = RTP timestamp gradient fallback for abs-send-time
                // (per §16.4 A3; abs-send-time wiring comes later).
                let send_us = (packet_timestamp as u64).wrapping_mul(100) / 9;
                // arrival_us = same local-µs timeline as slip: wall time since
                // the session anchor (the slip computation uses
                // arrival_at.duration_since(anchor_at)).
                let arrival_us = self.stream_clock_anchor
                    .map_or(0, |(anchor_at, _)| {
                        Instant::now().duration_since(anchor_at).as_micros() as u64
                    });
                self.q_estimator.push(crate::pipeline::clock::queue_delay::PacketInput {
                    send_us,
                    arrival_us,
                });
                // 7b: per-second Q peak maintained alongside the push (mirrors
                // stream_slip_max_us below; clamp at 0 so a negative Q cannot
                // wrap the u64 accumulator). drain_metrics() swaps it to 0.
                METRICS.q_us_max.fetch_max(self.q_estimator.q_us().max(0) as u64, Ordering::Relaxed);
            }
        }

        if let Some(pending) = &self.pending
            && pending.timestamp != packet_timestamp
        {
            if !timestamp_is_newer(packet_timestamp, pending.timestamp) {
                return stats;
            }
            if let Some(incomplete) = self.pending.take() {
                // 8a shadow: record the legacy abandon before the frame is
                // dropped (pairing happens by RTP timestamp).
                let seqs: Vec<u16> = incomplete
                    .packets
                    .iter()
                    .map(|p| p.header.sequence_number)
                    .collect();
                let (first_seq, last_seq) = seq_bounds_of(&seqs);
                self.shadow_cmp.observe_legacy(ShadowRecord {
                    ts: incomplete.timestamp,
                    class: ShadowClass::Abandoned,
                    first_seq,
                    last_seq,
                    packet_count: seqs.len() as u16,
                    bytes_hash: None,
                });
                self.next_sequence = incomplete
                    .marker_sequence()
                    .map(|sequence| sequence.wrapping_add(1));
                self.depacketizer = H264Packet::default();
                *keyframe_requested = true;
                self.record_damage(worker);
                frame_was_damaged = true;
                stats.dropped = stats.dropped.saturating_add(1);
            }
        }
        // Video-timing census (extmap id=7): parse six 16-bit stage deltas.
        if let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == 7)
            && ext.payload.len() == 13
        {
            for (i, slot) in self.vt_max.iter_mut().enumerate() {
                let d = u16::from_be_bytes([ext.payload[1 + i * 2], ext.payload[2 + i * 2]]) as u64;
                *slot = (*slot).max(d);
            }
            self.vt_exit_count += 1;
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
                // 8a shadow: record the legacy Invalid event before dropping.
                if let Some(invalid) = self.pending.take() {
                    let seqs: Vec<u16> = invalid
                        .packets
                        .iter()
                        .map(|p| p.header.sequence_number)
                        .collect();
                    let (first_seq, last_seq) = seq_bounds_of(&seqs);
                    self.shadow_cmp.observe_legacy(ShadowRecord {
                        ts: invalid.timestamp,
                        class: ShadowClass::Invalid,
                        first_seq,
                        last_seq,
                        packet_count: seqs.len() as u16,
                        bytes_hash: None,
                    });
                }
                self.next_sequence = None;
                *keyframe_requested = true;
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
        {
            // 8a shadow: record the legacy Complete event (bytes hashed for
            // byte-parity comparison against the new path).
            let seqs: Vec<u16> = completed
                .packets
                .iter()
                .map(|p| p.header.sequence_number)
                .collect();
            let (first_seq, last_seq) = seq_bounds_of(&seqs);
            self.shadow_cmp.observe_legacy(ShadowRecord {
                ts: completed.timestamp,
                class: ShadowClass::Complete,
                first_seq,
                last_seq,
                packet_count: seqs.len() as u16,
                bytes_hash: Some(hash_bytes(&data)),
            });
        }
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
            let (slip_ms, _wall_ms, media_ms) = slip;
            // Current slip (updated every packet; HUD reads this value).
            // Store signed slip directly — no clamp to positive, since signed
            // slip conveys direction (wall vs media time). Cite
            // RENDERING-PIPELINE-ARCHITECTURE-PROPOSITION.md §4.4.
            METRICS.stream_slip_us.store(slip_ms as i64, Ordering::Relaxed);
            // Maximum slip observed during the session (max absolute magnitude).
            METRICS.stream_slip_max_us.fetch_max(slip_ms.unsigned_abs() as u64, Ordering::Relaxed);
            // One-shot base slope: capture once when wall-time since anchor ≥ 60 s.
            let anchor_age_secs = self.stream_clock_anchor
                .map_or(0.0, |(anchor_at, _)| anchor_at.elapsed().as_secs_f64());
            if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) == 0
                && anchor_age_secs >= 60.0
            {
                // Session-fixed base: never re-based for the duration of this session.
                METRICS.stream_slip_slope_mpm.store(slip_ms as i64, Ordering::Relaxed);
            } else if METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) != 0
            {
                // Compute running slope relative to the base captured at ~60 s.
                // slope in ms/min: (delta_us * 60) / (1000 * elapsed_s)
                let base_slip = METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed) as i64;
                let elapsed_s = anchor_age_secs - 60.0; // seconds after the 60s base point
                if elapsed_s > 1.0 {
                    let delta_us = slip_ms as i64 - base_slip as i64;
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
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.waiting_for_keyframe = false;
            self.damage_score = 0;
        } else if !frame_was_damaged {
            self.damage_score = self.damage_score.saturating_sub(1);
        }

        // 8a telemetry (§3): submitted-AU counters that would have caught the
        // stage-8 black screen on day one.
        METRICS
            .au_submitted
            .fetch_add(1, Ordering::Relaxed);
        if !(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])) {
            METRICS.au_no_startcode.fetch_add(1, Ordering::Relaxed);
        }
        if unit.has_idr {
            METRICS.au_idr_submitted.fetch_add(1, Ordering::Relaxed);
        }
        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    /// 8a shadow (§3): record one new-path outcome for the comparator.
    /// Never touches legacy state.
    fn observe_shadow(&mut self, outcome: AssemblyOutcome, via_hold: bool) {
        if via_hold && matches!(outcome, AssemblyOutcome::Complete(_)) {
            METRICS.sh_hold_saved.fetch_add(1, Ordering::Relaxed);
        }
        self.shadow_cmp.observe_new(outcome);
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
    pub(crate) fn reset_lag_anchor(&mut self) { self.stream_clock_anchor = None; }
    /// Drift instrumentation: (slip_ms, wall_ms, media_ms).  slip = wall time
    /// since the session anchor minus media time (RTP clock at 90 kHz) advanced
    /// since the anchor.  A flat slope means arrivals keep pace with the media
    /// timeline; a growing slope means they increasingly fall behind.
    pub(crate) fn stream_slip(&self) -> Option<(i64, i64, i64)> {
        let (anchor_at, anchor_ts) = self.stream_clock_anchor?;
        let (arrival_at, arrival_ts) = self.latest_rtp?;
        let wall_ms = arrival_at.duration_since(anchor_at).as_millis() as i64;
        let media_ms = (arrival_ts.wrapping_sub(anchor_ts) as i64) * 1000 / 90;
        Some((wall_ms - media_ms, wall_ms, media_ms))
    }
    pub(crate) fn total_bytes(&self) -> u64 { self.total_bytes }
    pub(crate) fn twcc_median_stride(&self) -> u16 { 0 }
    pub(crate) fn take_twcc_report(&mut self, _media_ssrc: u32) -> Option<TwccReport> { None }
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
}

pub(crate) struct TwccReport {
    pub media_ssrc: u32,
    pub base_sequence_number: u16,
    pub packet_status_count: u16,
    pub reference_time: u32,
    pub chunks: Vec<rtcp::transport_feedbacks::transport_layer_cc::PacketStatusChunk>,
    pub recv_deltas: Vec<rtcp::transport_feedbacks::transport_layer_cc::RecvDelta>,
}

pub(crate) fn set_negotiated_twcc_ext_id(_id: u8) {}

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

// ---------------------------------------------------------------------------
// 8a shadow (STAGE8-REDO-SPEC §3): outcome pairing between the legacy authority
// and the new AccessUnitAssembler. Records are keyed by RTP timestamp and
// paired whenever both sides have produced one; verdicts feed METRICS counters.
// The comparator never touches pending, worker, keyframe_requested or stats.
// ---------------------------------------------------------------------------

/// Monotonic µs clock for the shadow assembler's 12 ms hold window.
static SHADOW_CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);

/// FNV-1a — deterministic byte hash for legacy↔new AU parity comparison.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowClass {
    /// Legacy `FrameAssembly::Complete` / new `AssemblyOutcome::Complete`.
    Complete,
    /// New `AssemblyOutcome::Damaged` (legacy has no equivalent class).
    Damaged,
    /// New `AssemblyOutcome::Discarded` (oversize/malformed).
    Discarded,
    /// Legacy force-out of an incomplete frame on a newer timestamp.
    Abandoned,
    /// Legacy `FrameAssembly::Invalid` (depacketize error / oversize).
    Invalid,
}

#[derive(Debug, Clone)]
struct ShadowRecord {
    ts: u32,
    class: ShadowClass,
    first_seq: u16,
    last_seq: u16,
    packet_count: u16,
    bytes_hash: Option<u64>,
}

impl ShadowRecord {
    /// Build a shadow-side record from a new-path outcome.
    fn from_new(outcome: AssemblyOutcome) -> Self {
        match outcome {
            AssemblyOutcome::Complete(desc) => ShadowRecord {
                ts: desc.rtp_ts,
                class: ShadowClass::Complete,
                first_seq: desc.first_seq,
                last_seq: desc.last_seq,
                packet_count: desc.packet_count,
                bytes_hash: Some(hash_bytes(&desc.data)),
            },
            AssemblyOutcome::Damaged(desc) => ShadowRecord {
                ts: desc.rtp_ts,
                class: ShadowClass::Damaged,
                first_seq: desc.first_seq,
                last_seq: desc.last_seq,
                packet_count: desc.packet_count,
                bytes_hash: None,
            },
            AssemblyOutcome::Discarded { reason: _, rtp_ts } => ShadowRecord {
                ts: rtp_ts,
                class: ShadowClass::Discarded,
                first_seq: 0,
                last_seq: 0,
                packet_count: 0, // Discarded carries no packet bounds
                bytes_hash: None,
            },
        }
    }
}

/// Verdict for one paired (legacy, new) outcome tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairVerdict {
    Match,
    BytesMismatch,
    ClassMismatch,
    /// Expected-delta bucket: new Damaged where legacy completed (§3 — the
    /// H3 fix; in this base also reached by reorder-marker-early pairings).
    BucketDamagedOldComplete,
    /// Expected-delta bucket: new Complete where legacy abandoned (§3 —
    /// 12 ms hold / markerless force-out equivalence).
    BucketCompleteOldAbandon,
}

/// Pure classification of a paired outcome tuple (unit-tested).
fn classify_pair(legacy: &ShadowRecord, new_rec: &ShadowRecord) -> PairVerdict {
    use ShadowClass::*;
    match (legacy.class, new_rec.class) {
        (Complete, Complete) => {
            if legacy.bytes_hash == new_rec.bytes_hash {
                PairVerdict::Match
            } else {
                PairVerdict::BytesMismatch
            }
        }
        (Abandoned | Invalid, Damaged | Discarded) => PairVerdict::Match,
        (Complete, Damaged) => PairVerdict::BucketDamagedOldComplete,
        (Abandoned, Complete) => PairVerdict::BucketCompleteOldAbandon,
        _ => PairVerdict::ClassMismatch,
    }
}

#[derive(Debug)]
struct ShadowComparator {
    legacy: Vec<ShadowRecord>,
    new: Vec<ShadowRecord>,
}

impl ShadowComparator {
    fn new() -> Self {
        Self {
            legacy: Vec::new(),
            new: Vec::new(),
        }
    }

    fn observe_legacy(&mut self, record: ShadowRecord) {
        self.legacy.push(record);
        if self.legacy.len() > 64 {
            self.legacy.remove(0);
            METRICS
                .sh_legacy_unmatched
                .fetch_add(1, Ordering::Relaxed);
        }
        self.try_match();
    }

    fn observe_new(&mut self, outcome: AssemblyOutcome) {
        let record = ShadowRecord::from_new(outcome);
        self.new.push(record);
        if self.new.len() > 64 {
            self.new.remove(0);
            METRICS.sh_new_unmatched.fetch_add(1, Ordering::Relaxed);
        }
        self.try_match();
    }

    /// Pair pending records by RTP timestamp and apply counters.
    fn try_match(&mut self) {
        let mut i = 0;
        while i < self.legacy.len() {
            let ts = self.legacy[i].ts;
            if let Some(j) = self.new.iter().position(|record| record.ts == ts) {
                let new_rec = self.new.remove(j);
                let legacy_rec = self.legacy.remove(i);
                self.compare(&legacy_rec, &new_rec);
            } else {
                i += 1;
            }
        }
    }

    fn compare(&mut self, legacy: &ShadowRecord, new_rec: &ShadowRecord) {
        // Boundary comparison (§3): only when both sides report packet bounds.
        if legacy.packet_count > 0 && new_rec.packet_count > 0 {
            let same_bounds = (legacy.first_seq, legacy.last_seq, legacy.packet_count)
                == (
                    new_rec.first_seq,
                    new_rec.last_seq,
                    new_rec.packet_count,
                );
            if !same_bounds {
                METRICS
                    .sh_bound_mismatch
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        match classify_pair(legacy, new_rec) {
            PairVerdict::Match => {
                METRICS.sh_match.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BytesMismatch => {
                METRICS.sh_bytes_mismatch.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::ClassMismatch => {
                METRICS.sh_class_mismatch.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BucketDamagedOldComplete => {
                METRICS
                    .sh_new_damaged_old_complete
                    .fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BucketCompleteOldAbandon => {
                METRICS
                    .sh_new_complete_old_abandon
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod shadow_tests {
    use super::*;
    use crate::pipeline::media::assembler::DiscardReason;

    fn rec(ts: u32, class: ShadowClass, hash: Option<u64>) -> ShadowRecord {
        ShadowRecord {
            ts,
            class,
            first_seq: 10,
            last_seq: 12,
            packet_count: 3,
            bytes_hash: hash,
        }
    }

    #[test]
    fn classify_complete_pair_matches_on_equal_hash() {
        let l = rec(1, ShadowClass::Complete, Some(42));
        let n = rec(1, ShadowClass::Complete, Some(42));
        assert_eq!(classify_pair(&l, &n), PairVerdict::Match);
        let n_bad = rec(1, ShadowClass::Complete, Some(43));
        assert_eq!(classify_pair(&l, &n_bad), PairVerdict::BytesMismatch);
    }

    #[test]
    fn classify_legacy_reject_vs_new_reject_matches() {
        let abandoned = rec(2, ShadowClass::Abandoned, None);
        let damaged = rec(2, ShadowClass::Damaged, None);
        assert_eq!(
            classify_pair(&abandoned, &damaged),
            PairVerdict::Match
        );
        let invalid = rec(3, ShadowClass::Invalid, None);
        let discarded = rec(3, ShadowClass::Discarded, None);
        assert_eq!(
            classify_pair(&invalid, &discarded),
            PairVerdict::Match
        );
    }

    #[test]
    fn classify_expected_delta_buckets() {
        // Bucket 1: new Damaged where legacy completed (H3 fix).
        let complete = rec(4, ShadowClass::Complete, Some(7));
        let damaged = rec(4, ShadowClass::Damaged, None);
        assert_eq!(
            classify_pair(&complete, &damaged),
            PairVerdict::BucketDamagedOldComplete
        );
        // Bucket 2: new Complete where legacy abandoned (12 ms hold).
        let abandoned = rec(5, ShadowClass::Abandoned, None);
        let complete_new = rec(5, ShadowClass::Complete, Some(9));
        assert_eq!(
            classify_pair(&abandoned, &complete_new),
            PairVerdict::BucketCompleteOldAbandon
        );
    }

    #[test]
    fn classify_unexpected_pair_is_class_mismatch() {
        let complete = rec(6, ShadowClass::Complete, Some(1));
        let discarded = rec(6, ShadowClass::Discarded, None);
        assert_eq!(
            classify_pair(&complete, &discarded),
            PairVerdict::ClassMismatch
        );
        // (Invalid, Complete) is NOT bucket 2 — only Abandoned is.
        let invalid = rec(7, ShadowClass::Invalid, None);
        let complete_new = rec(7, ShadowClass::Complete, Some(2));
        assert_eq!(
            classify_pair(&invalid, &complete_new),
            PairVerdict::ClassMismatch
        );
    }

    #[test]
    fn record_from_new_outcome_maps_classes_and_ts() {
        let mut assembler = AccessUnitAssembler::new();
        let payload = [0x01u8, 0x00, 0x00];
        let outcome = assembler
            .push(10, 90_000, true, &payload, 1_000)
            .expect("marker resolves");
        let record = ShadowRecord::from_new(outcome);
        assert_eq!(record.class, ShadowClass::Complete);
        assert_eq!(record.ts, 90_000);
        assert_eq!((record.first_seq, record.last_seq), (10, 10));
        assert!(record.bytes_hash.is_some());

        let discarded = ShadowRecord::from_new(AssemblyOutcome::Discarded {
            reason: DiscardReason::Malformed,
            rtp_ts: 180_000,
        });
        assert_eq!(discarded.class, ShadowClass::Discarded);
        assert_eq!(discarded.ts, 180_000);
    }

    #[test]
    fn hash_bytes_is_stable_and_discriminating() {
        assert_eq!(hash_bytes(&[1, 2, 3]), hash_bytes(&[1, 2, 3]));
        assert_ne!(hash_bytes(&[1, 2, 3]), hash_bytes(&[1, 2, 4]));
    }
}

// ---------------------------------------------------------------------------
// Differential fixtures — STAGE8-REDO-SPEC §5.1/§5.2.
//
// Each fixture replays an identical packet stream through (a) the LEGACY
// authority (PendingVideoFrame + H264Packet, mirroring receive()'s control
// flow) and (b) the new AccessUnitAssembler, asserting byte-identical AUs and
// boundaries wherever legacy completes. The two sanctioned expected-delta
// buckets (§3) are asserted explicitly; classification parity for the reject
// class (legacy Invalid ↔ new Discarded) is asserted where relevant.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod diff_fixtures {
    use super::*;
    use crate::pipeline::media::assembler::{
        AccessUnitAssembler, AssemblyOutcome, DiscardReason,
    };
    use rtc::rtp::Header;

    const TS0: u32 = 90_000;
    const TS1: u32 = 180_000;

    #[derive(Debug, PartialEq)]
    pub(crate) enum LegacyEvent {
        Complete { at: usize, ts: u32, data: Vec<u8> },
        Abandoned { at: usize, ts: u32 },
        Invalid { at: usize, ts: u32 },
    }

    pub(crate) fn pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet {
            header: Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(payload),
        }
    }

    /// Legacy-valid single NAL: >= 3 bytes (legacy depacketize returns
    /// ErrShortPacket for <= 2-byte non-AUD payloads).
    fn single_nal(nal_type: u8, fill: u8) -> Vec<u8> {
        vec![nal_type, fill, fill.wrapping_add(1)]
    }

    fn stap_a(nals: &[&[u8]]) -> Vec<u8> {
        let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
        for nal in nals {
            payload.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            payload.extend_from_slice(nal);
        }
        payload
    }

    /// Split `nal` into FU-A fragments of `frag_size` body bytes, sequenced
    /// from `first_seq` (marker left false; caller sets it).
    fn fu_packets(nal: &[u8], first_seq: u16, frag_size: usize) -> Vec<Packet> {
        let indicator = (nal[0] & 0x60) | 28; // preserve nri, type = FU-A
        let body = &nal[1..];
        let chunks: Vec<&[u8]> = body.chunks(frag_size).collect();
        let total = chunks.len();
        let mut seq = first_seq;
        let mut out = Vec::new();
        for (k, chunk) in chunks.iter().enumerate() {
            let mut fu_header = nal[0] & 0x1F;
            if k == 0 {
                fu_header |= 0x80; // start
            }
            if k + 1 == total {
                fu_header |= 0x40; // end
            }
            let mut payload = vec![indicator, fu_header];
            payload.extend_from_slice(chunk);
            out.push(pkt(seq, 0, false, &payload)); // ts set by caller
            seq = seq.wrapping_add(1);
        }
        out
    }

    /// Mirror of receive()'s byte-relevant control flow: force-out on newer
    /// timestamp, stale-ts drop, last_frame gate, insert dedup, next_sequence
    /// tracking. Damage/PLI side effects are out of scope (no worker here).
    pub(crate) fn legacy_run(packets: &[Packet]) -> Vec<LegacyEvent> {
        let mut events = Vec::new();
        let mut pending: Option<PendingVideoFrame> = None;
        let mut depacketizer = H264Packet::default();
        let mut next_sequence: Option<u16> = None;
        let mut last_frame_ts: Option<u32> = None;
        for (i, packet) in packets.iter().enumerate() {
            if packet.payload.is_empty() {
                continue;
            }
            let ts = packet.header.timestamp;
            if let Some(existing) = &pending
                && existing.timestamp != ts
            {
                if !timestamp_is_newer(ts, existing.timestamp) {
                    continue; // receive(): stale packet dropped
                }
                let incomplete = pending.take().expect("pending frame");
                next_sequence = incomplete.marker_sequence().map(|s| s.wrapping_add(1));
                depacketizer = H264Packet::default();
                events.push(LegacyEvent::Abandoned {
                    at: i,
                    ts: incomplete.timestamp,
                });
            }
            if pending.is_none() {
                if last_frame_ts.is_some_and(|last| !timestamp_is_newer(ts, last)) {
                    continue; // receive(): last_frame_timestamp gate
                }
                pending = Some(PendingVideoFrame::new(packet.clone()));
            } else {
                pending.as_mut().expect("pending frame").insert(packet.clone());
            }
            match pending
                .as_ref()
                .expect("pending frame")
                .assemble(&mut depacketizer, next_sequence)
            {
                FrameAssembly::Pending => {}
                FrameAssembly::Invalid => {
                    pending = None;
                    next_sequence = None;
                    events.push(LegacyEvent::Invalid { at: i, ts });
                }
                FrameAssembly::Complete { data, marker_sequence } => {
                    pending = None;
                    next_sequence = Some(marker_sequence.wrapping_add(1));
                    last_frame_ts = Some(ts);
                    events.push(LegacyEvent::Complete {
                        at: i,
                        ts,
                        data: data.to_vec(),
                    });
                }
            }
        }
        events
    }

    /// Run packets through the new assembler (1 ms inter-arrival), draining
    /// the outcome queue after every push.
    fn new_run(packets: &[Packet]) -> Vec<(usize, AssemblyOutcome)> {
        let mut assembler = AccessUnitAssembler::new();
        let mut events = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let arrival_us = 1_000 + i as u64 * 1_000;
            if let Some(outcome) = assembler.push(
                packet.header.sequence_number,
                packet.header.timestamp,
                packet.header.marker,
                &packet.payload,
                arrival_us,
            ) {
                events.push((i, outcome));
            }
            while let Some(outcome) = assembler.poll_outcome() {
                events.push((i, outcome));
            }
        }
        events
    }

    fn legacy_completes(events: &[LegacyEvent]) -> Vec<(usize, Vec<u8>)> {
        events
            .iter()
            .filter_map(|event| match event {
                LegacyEvent::Complete { at, data, .. } => Some((*at, data.clone())),
                _ => None,
            })
            .collect()
    }

    fn new_completes(events: &[(usize, AssemblyOutcome)]) -> Vec<(usize, Vec<u8>)> {
        events
            .iter()
            .filter_map(|(at, outcome)| match outcome {
                AssemblyOutcome::Complete(desc) => Some((*at, desc.data.clone())),
                _ => None,
            })
            .collect()
    }

    /// Core parity assertion: where legacy completes, the new path completes
    /// the same AU bytes at the same packet index — and vice versa.
    fn assert_parity(packets: &[Packet]) {
        let legacy = legacy_completes(&legacy_run(packets));
        let new = new_completes(&new_run(packets));
        assert_eq!(
            legacy, new,
            "new path must produce legacy-identical AU bytes and boundaries"
        );
    }

    // ---- §5.1 synthetic ----------------------------------------------------

    #[test]
    fn probe_annexb_start_code_is_four_bytes() {
        let packets = vec![pkt(10, TS0, true, &single_nal(0x01, 0xAA))];
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete a marker'd single NAL: {other:?}"),
        };
        assert_eq!(&data[..4], &[0, 0, 0, 1], "legacy emits 4-byte start codes");
        assert_eq!(&data[4..], &packets[0].payload[..], "NAL verbatim after start code");
        assert_parity(&packets);
    }

    #[test]
    fn parity_single_nal_p_frame() {
        assert_parity(&[pkt(10, TS0, true, &single_nal(0x01, 0x01))]);
    }

    #[test]
    fn parity_stap_a_sps_pps_split_in_order() {
        let sps: &[u8] = &[0x67, 0x42, 0x00, 0x1E, 0xAB];
        let pps: &[u8] = &[0x68, 0xCE, 0x06, 0xE2];
        let packets = vec![pkt(20, TS0, true, &stap_a(&[sps, pps]))];
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete STAP-A: {other:?}"),
        };
        let mut expect = vec![0, 0, 0, 1];
        expect.extend_from_slice(sps);
        expect.extend_from_slice(&[0, 0, 0, 1]);
        expect.extend_from_slice(pps);
        assert_eq!(data, expect, "SPS then PPS, each start-code framed");
        assert_parity(&packets);
    }

    #[test]
    fn parity_idr_fu_a_multi_fragment() {
        let mut idr = vec![0x65]; // nri=3, type=5 (IDR)
        idr.extend(0..40u8);
        let mut packets = fu_packets(&idr, 100, 10);
        assert!(packets.len() >= 3, "need a fragmented IDR");
        packets.last_mut().expect("non-empty").header.marker = true;
        for packet in &mut packets {
            packet.header.timestamp = TS0;
        }
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("legacy should complete fragmented IDR: {other:?}"),
        };
        let mut expect = vec![0, 0, 0, 1];
        expect.extend_from_slice(&idr);
        assert_eq!(data, expect, "FU-A reassembles to one start-coded IDR NAL");
        assert_parity(&packets);
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Complete(desc) => assert!(desc.is_idr, "IDR flag must be set"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn parity_multi_packet_p_frame() {
        let packets = vec![
            pkt(30, TS0, false, &single_nal(0x01, 0x10)),
            pkt(31, TS0, false, &single_nal(0x01, 0x20)),
            pkt(32, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
    }

    #[test]
    fn parity_sei_single() {
        let sei = [0x06, 0x05, 0x00, 0x80, 0x11];
        assert_parity(&[pkt(40, TS0, true, &sei)]);
    }

    // ---- §5.2 edge cases ---------------------------------------------------

    #[test]
    fn parity_reordered_within_frame_marker_last() {
        // Arrival order 31, 30, 32(marker): legacy sorts by seq around the
        // marker; the new path must apply the same key => identical bytes.
        let packets = vec![
            pkt(31, TS0, false, &single_nal(0x01, 0x20)),
            pkt(30, TS0, false, &single_nal(0x01, 0x10)),
            pkt(32, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
        let legacy = legacy_run(&packets);
        let data = match &legacy[..] {
            [LegacyEvent::Complete { data, .. }] => data.clone(),
            other => panic!("{other:?}"),
        };
        // NALs must come out in SEQUENCE order (30, 31, 32), not arrival order.
        assert_eq!(&data[5], &0x10, "first NAL fill byte must be seq 30's");
    }

    #[test]
    fn bucket1_lost_middle_new_damaged_legacy_abandons() {
        let packets = vec![
            pkt(50, TS0, false, &single_nal(0x01, 0x10)),
            // seq 51 lost
            pkt(52, TS0, true, &single_nal(0x01, 0x30)),
            pkt(53, TS1, true, &single_nal(0x01, 0x40)), // frame 2
        ];
        let legacy = legacy_run(&packets);
        // Only frame 2 may complete on legacy (it starts exactly at frame-1's
        // marker + 1); the gapped frame 1 must never be submitted.
        let legacy_complete_indices: Vec<usize> = legacy_completes(&legacy)
            .iter()
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(
            legacy_complete_indices,
            vec![2],
            "legacy must never submit the gapped frame 1: {legacy:?}"
        );
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }),
            "legacy abandons frame 1 on force-out: {legacy:?}"
        );
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert_eq!(desc.holes, 1, "one missing sequence number");
                assert!(desc.data.is_empty(), "Damaged must carry no bytes (§4)");
            }
            other => panic!("expected Damaged for frame 1, got {other:?}"),
        }
        // Frame 2 (seq starts exactly at frame-1 marker + 1) completes
        // identically on both paths.
        assert_eq!(legacy_completes(&legacy), new_completes(&new));
    }

    #[test]
    fn bucket1_lost_first_fu_start_never_submitted() {
        let packets = vec![
            pkt(70, TS0, false, &[0x7C, 0x05, 0xAA, 0xBB]), // FU continuation (no start)
            pkt(71, TS0, true, &[0x7C, 0x45, 0xCC, 0xDD]),  // FU end fragment
            pkt(72, TS1, true, &single_nal(0x01, 0x40)),    // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy_completes(&legacy).len() == 1,
            "only frame 2 may complete on legacy: {legacy:?}"
        );
        assert!(legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }));
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(!desc.had_first_slice, "H3: unverified start");
                assert!(desc.data.is_empty(), "§4: no bytes from Damaged");
            }
            other => panic!("expected Damaged, got {other:?}"),
        }
        assert_eq!(legacy_completes(&legacy), new_completes(&new));
    }

    #[test]
    fn bucket2_markerless_frame_completes_only_via_forceout() {
        let packets = vec![
            pkt(80, TS0, false, &single_nal(0x01, 0x10)), // frame 1, marker lost
            pkt(81, TS1, true, &single_nal(0x01, 0x20)),  // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 1, ts: TS0 }),
            "legacy abandons the markerless frame: {legacy:?}"
        );
        let new = new_run(&packets);
        // New resolves frame 1 (contiguous + verified start) at the force-out.
        let frame1_bytes = new_completes(&new)
            .into_iter()
            .find(|(at, _)| *at == 1)
            .map(|(_, data)| data);
        let mut expect_frame1 = vec![0, 0, 0, 1];
        expect_frame1.extend_from_slice(&single_nal(0x01, 0x10));
        assert_eq!(
            frame1_bytes.as_deref(),
            Some(&expect_frame1[..]),
            "bucket 2: new completes the abandoned frame with correct bytes"
        );
        // Frame 2 completes identically on both.
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            2,
            "new completes frame 1 (bucket 2) + frame 2: {new:?}"
        );
        assert_eq!(
            legacy_f2[0],
            *new_all.last().expect("frame 2 event"),
            "frame 2 bytes/boundary parity"
        );
    }

    #[test]
    fn bucket2_hold_expiry_completes_at_12ms() {
        let payload = single_nal(0x01, 0x10);
        let mut assembler = AccessUnitAssembler::new();
        assert!(assembler.push(90, TS0, false, &payload, 100_000).is_none());
        // Inside the hold window: no flush (regression guard: au_first_arrival_us
        // must be anchored at AU start, else the first flush_due fires early).
        assert!(
            assembler.flush_due(105_000).is_none(),
            "must not flush 5 ms into the 12 ms hold window"
        );
        let outcome = assembler
            .flush_due(113_000)
            .expect("flush after the 12 ms hold window");
        match outcome {
            AssemblyOutcome::Complete(desc) => {
                let mut expect = vec![0, 0, 0, 1];
                expect.extend_from_slice(&payload);
                assert_eq!(desc.data, expect, "hold-expired AU is valid Annex-B");
            }
            other => panic!("expected Complete via hold expiry, got {other:?}"),
        }
        // Legacy has no hold mechanism: the markerless AU never completes.
        assert!(
            legacy_run(&[pkt(90, TS0, false, &payload)]).is_empty(),
            "legacy never completes a markerless frame by itself"
        );
    }

    #[test]
    fn parity_duplicate_packet_mid_frame() {
        let packets = vec![
            pkt(100, TS0, false, &single_nal(0x01, 0x10)),
            pkt(101, TS0, false, &single_nal(0x01, 0x20)),
            pkt(101, TS0, false, &single_nal(0x01, 0x20)), // duplicate
            pkt(102, TS0, true, &single_nal(0x01, 0x30)),
        ];
        assert_parity(&packets);
        let new = new_run(&packets);
        match &new[0].1 {
            AssemblyOutcome::Complete(desc) => assert_eq!(desc.packet_count, 3, "dedup"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn parity_sequence_wrap_across_frames() {
        let packets = vec![
            pkt(65_534, TS0, false, &single_nal(0x01, 0x10)),
            pkt(65_535, TS0, true, &single_nal(0x01, 0x20)),
            pkt(0, TS1, false, &single_nal(0x01, 0x30)), // wraps; expected seq = 0
            pkt(1, TS1, true, &single_nal(0x01, 0x40)),
        ];
        assert_parity(&packets);
        for (at, outcome) in new_run(&packets) {
            if let AssemblyOutcome::Damaged(desc) = outcome {
                panic!("no holes expected (at {at}): {desc:?}");
            }
        }
    }

    #[test]
    fn parity_timestamp_wrap_between_frames() {
        let packets = vec![
            pkt(10, 0xFFFF_FF00, true, &single_nal(0x01, 0x10)),
            pkt(11, 0x0000_1000, true, &single_nal(0x01, 0x20)), // u32 wrap
        ];
        assert_parity(&packets);
    }

    #[test]
    fn reject_oversize_is_rejected_by_both_paths() {
        let big = vec![0x01u8; 100_000];
        let mut packets = Vec::new();
        for i in 0..21u16 {
            let marker = i == 20;
            packets.push(pkt(200 + i, TS0, marker, &big));
        }
        let legacy = legacy_run(&packets);
        assert!(
            legacy_completes(&legacy).is_empty(),
            "legacy must not submit oversize: {legacy:?}"
        );
        assert!(
            legacy.contains(&LegacyEvent::Invalid { at: 20, ts: TS0 }),
            "legacy rejects at the marker packet: {legacy:?}"
        );
        let new = new_run(&packets);
        assert!(
            new_completes(&new).is_empty(),
            "new must not submit oversize: {new:?}"
        );
        assert!(
            new.iter().any(|(_, outcome)| matches!(
                outcome,
                AssemblyOutcome::Discarded {
                    reason: DiscardReason::Oversize,
                    ..
                }
            )),
            "new classifies oversize: {new:?}"
        );
    }

    #[test]
    fn classification_malformed_stap_maps_legacy_invalid() {
        // STAP-A whose declared size overruns the buffer → legacy depacketize
        // Err → Invalid; new → Discarded{Malformed}. Neither submits bytes.
        let bad_stap = [0x78, 0xFF, 0xFF, 0x67, 0x42];
        let packets = vec![pkt(110, TS0, true, &bad_stap)];
        assert_eq!(
            legacy_run(&packets),
            vec![LegacyEvent::Invalid { at: 0, ts: TS0 }],
            "legacy Invalid for malformed STAP-A"
        );
        assert!(
            matches!(
                new_run(&packets).as_slice(),
                [(0, AssemblyOutcome::Discarded {
                    reason: DiscardReason::Malformed,
                    ..
                })]
            ),
            "new Discarded{{Malformed}} for the same payload"
        );
    }

    #[test]
    fn parity_lost_marker_followed_by_next_timestamp() {
        // §5.2: marker-less frame followed by next timestamp — legacy force-out
        // path with an in-sequence follower (no expected-sequence mismatch).
        let packets = vec![
            pkt(120, TS0, false, &single_nal(0x01, 0x10)), // no marker
            pkt(121, TS0, false, &single_nal(0x01, 0x20)),
            pkt(122, TS1, true, &single_nal(0x01, 0x30)), // frame 2
        ];
        let legacy = legacy_run(&packets);
        assert!(
            legacy.contains(&LegacyEvent::Abandoned { at: 2, ts: TS0 }),
            "legacy force-outs the markerless frame: {legacy:?}"
        );
        let new = new_run(&packets);
        // Bucket 2: new completes frame 1 where legacy abandoned it; frame 2
        // completes on both with identical bytes.
        assert_eq!(
            new_completes(&new).len(),
            2,
            "new completes frame 1 (bucket 2) and frame 2: {new:?}"
        );
        assert_eq!(
            legacy_completes(&legacy).len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            2,
            "new completes frame 1 (bucket 2) + frame 2: {new:?}"
        );
        assert_eq!(
            legacy_f2[0],
            *new_all.last().expect("frame 2"),
            "frame 2 bytes/boundary parity"
        );
    }
}

// Real-capture fixture replay (§5.3) — separate file, child of this module so
// it can reuse the legacy harness above.
#[cfg(test)]
#[path = "diff_replay.rs"]
mod diff_replay;