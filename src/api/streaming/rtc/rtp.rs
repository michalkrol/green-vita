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
    /// Arrival-order packet digests for the mismatch-evidence capture
    /// (observe-only; fixed-capacity, no hot-path allocation).
    arrival: mismatch_log::ArrivalRing,
}

struct PendingVideoFrame {
    timestamp: u32,
    first_packet_at: Instant,
    packets: Vec<Packet>,
}

/// 8b (STAGE8-REDO-SPEC §6.1): a completed AU handed to `finish_au`.
struct ResolvedAu {
    data: Bytes,
    marker_sequence: u16,
    timestamp: u32,
    first_packet_at: Instant,
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
            arrival: mismatch_log::ArrivalRing::new(),
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

        // Mismatch-evidence arrival ring (observe-only): per-packet
        // (seq, len, payload hash) in arrival order for the frame in
        // flight; fixed-capacity arrays, zero heap traffic.
        self.arrival.push(
            packet_timestamp,
            packet.header.sequence_number,
            &packet.payload,
        );

        // 8a shadow (STAGE8-REDO-SPEC §3): feed every non-empty video packet
        // to the new assembler alongside the legacy path. Behaviour-neutral by
        // construction — it never touches pending, worker, keyframe_requested
        // or stats; outcomes only feed the comparator counters. flush_due runs
        // here in shadow only (§3).
        let shadow_now = SHADOW_CLOCK.elapsed().as_micros() as u64;
        let late_before = self.shadow.late_after_resolve();
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
        // Rule-4 phantom-guard drops (§11) mirrored into METRICS for the
        // 8a-v2 gate report.
        let late_delta = self.shadow.late_after_resolve() - late_before;
        if late_delta > 0 {
            METRICS
                .late_after_resolve
                .fetch_add(late_delta, Ordering::Relaxed);
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
                    digests: Box::new([]),
                    data: bytes::Bytes::new(),
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
                        digests: Box::new([]),
                        data: bytes::Bytes::new(),
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
                // Evidence: the packets THIS path kept (arrival order,
                // post-legacy-dedup) + the assembled bytes (refcount clone).
                digests: completed
                    .packets
                    .iter()
                    .map(|packet| mismatch_log::PacketDigest {
                        seq: packet.header.sequence_number,
                        len: packet.payload.len() as u16,
                        hash: hash_bytes(&packet.payload),
                    })
                    .collect(),
                data: data.clone(),
            });
        }
        // 8b (STAGE8-REDO-SPEC §6.1): legacy post-assembly body extracted
        // verbatim as finish_au; the authority switch is a call-site change.
        let stats = self.finish_au(
            ResolvedAu {
                data,
                marker_sequence,
                timestamp: completed.timestamp,
                first_packet_at: completed.first_packet_at,
            },
            worker,
            keyframe_requested,
            stats,
            frame_was_damaged,
        );
        stats
    }

    /// 8b (STAGE8-REDO-SPEC §6.1): the legacy post-assembly body, moved
    /// verbatim. Executes once per Complete AU, in checklist order:
    /// assembly metrics -> slip -> next_sequence -> frame duration +
    /// last_frame_timestamp -> inspect_h264_access_unit -> size gates ->
    /// waiting_for_keyframe gate (+D0b) -> damage decay -> submit_access_unit
    /// (+queue_full handling).
    fn finish_au(
        &mut self,
        au: ResolvedAu,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        mut stats: VideoSampleStats,
        frame_was_damaged: bool,
    ) -> VideoSampleStats {
        let ResolvedAu {
            data,
            marker_sequence,
            ..
        } = au;
        // Record both average and worst-case RTP assembly time for the stream HUD.
        let assembly_us = au.first_packet_at.elapsed().as_micros() as u64;
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
            u64::from(au.timestamp.wrapping_sub(previous)) * 1_000_000
                / u64::from(VIDEO_RTP_CLOCK_RATE)
        });
        if let Some(duration) = stats.source_frame_duration_us {
            self.source_frame_duration_us = Some(
                self.source_frame_duration_us
                    .map(|average| (average * 7 + duration) / 8)
                    .unwrap_or(duration),
            );
        }
        self.last_frame_timestamp = Some(au.timestamp);

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
        // Mismatch-evidence digests are only needed for Complete outcomes:
        // copy the arrival-ring list here so the comparator owns it until
        // pairing. Observe-only — no pending/submit/timing contact.
        let digests = if let AssemblyOutcome::Complete(desc) = &outcome {
            self.arrival.digests_for(desc.rtp_ts)
        } else {
            Box::new([])
        };
        self.shadow_cmp.observe_new(outcome, digests);
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

// ---------------------------------------------------------------------------
// RANDOMIZED DIFFERENTIAL TEST
// §5.4: run identical packet streams through both the LEGACY and NEW
// assembly paths with a seeded PRNG, asserting byte-identical AU output for
// all Complete-vs-Complete pairs.  Designed to surface the rare legacy-vs-new
// byte disagreements observed on device (1 in 57,757 AUs).
// ---------------------------------------------------------------------------

/// Very small inline xorshift64* PRNG — no external crates.
/// State is a u64; next_u64() advances and returns the new state; below(n)
/// returns a uniform [0, n) u64.
#[cfg(test)]
struct XorShift64 {
    state: u64,
}

#[cfg(test)]
impl XorShift64 {
    const fn from_seed(seed: u64) -> Self {
        // Seed of 0 is disallowed (xorshift requires odd/non-zero).
        let state = seed | 1;
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        // Xorshift64* : xor with left-shift, then multiply by odd constant.
        // Constant 0x254A_5555_5555_5551 works well for 64-bit.
        let mut z = self.state;
        z ^= z << 13;
        z ^= z >> 7;
        z ^= z << 17;
        self.state = z.wrapping_mul(0x254A_5555_5555_5551u64);
        self.state
    }

    fn below(&mut self, n: u64) -> u64 {
        // Uniform [0, n). Uses rejection sampling to avoid modulo bias.
        if n == 0 { return 0; }
        if n == 1 { return 0; }
        // Compute the minimum value that would cause modulo bias.
        // We want the largest multiple of n that is <= u64::MAX.
        // limit = ((u64::MAX + 1) / n) * n, but we compute it without overflow.
        // u64::MAX + 1 wraps to 0, so: limit = u64::MAX - (u64::MAX % n) + 1
        // but this can overflow when n=1 (already handled) or when the result wraps.
        // Safe approach: reject any r >= n * (u64::MAX / n)
        let quota = u64::MAX / n;     // number of complete groups of n
        let bound = quota * n;        // last valid value (always < u64::MAX since quota < n when n > 1)
        loop {
            let r = self.next_u64();
            if r < bound { return r % n; }
        }
    }
}

// Helper: create a Packet with the given fields.
#[cfg(test)]
fn make_pkt(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> rtc::rtp::Packet {
    rtc::rtp::Packet {
        header: rtc::rtp::Header {
            sequence_number: seq,
            timestamp: ts,
            marker,
            ..Default::default()
        },
        payload: bytes::Bytes::copy_from_slice(payload),
    }
}

// Helper: build a STAP-A packet containing SPS and PPS NALs.
#[cfg(test)]
fn make_stap_a(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
    payload.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    payload.extend_from_slice(sps);
    payload.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    payload.extend_from_slice(pps);
    payload
}

// Helper: build FU-A fragments for a NAL body, starting at the given seq.
// Inlined to avoid needing pub(crate) visibility on diff_fixtures helpers.
#[cfg(test)]
fn make_fu_fragments_inline(nal_body: &[u8], first_seq: u16, frag_size: usize, ts: u32, marker: bool) -> Vec<rtc::rtp::Packet> {
    let indicator = (nal_body[0] & 0x60) | 28; // preserve nri, type = FU-A
    let body = &nal_body[1..]; // drop NAL type byte
    let chunks: Vec<&[u8]> = body.chunks(frag_size).collect();
    let total = chunks.len();
    let mut seq = first_seq;
    let mut out = Vec::new();
    for (k, chunk) in chunks.iter().enumerate() {
        let mut fu_header = nal_body[0] & 0x1F;
        if k == 0 { fu_header |= 0x80; } // start bit
        if k + 1 == total { fu_header |= 0x40; } // end bit
        let mut payload = vec![indicator, fu_header];
        payload.extend_from_slice(chunk);
        out.push(rtc::rtp::Packet {
            header: rtc::rtp::Header {
                sequence_number: seq,
                timestamp: ts,
                marker,
                ..Default::default()
            },
            payload: bytes::Bytes::copy_from_slice(&payload),
        });
        seq = seq.wrapping_add(1);
    }
    out
}

// Core randomized differential test: run identical packet streams through
// both the LEGACY and NEW assembly paths with a seeded PRNG, asserting
// byte-identical AU output for all Complete-vs-Complete pairs.
#[test]
fn randomized_differential_matches() {
    // Seeded PRNG — deterministic across runs.
    let mut rng = XorShift64::from_seed(0x5DEECE66D_1234u64);

    const TOTAL_ITERATIONS: u64 = 8000; // tuned to fit < 60s host runtime
    const TS0: u32 = 90_000;

    let mut failures = 0;

    for iter in 0..TOTAL_ITERATIONS {
        // ---- Build a synthetic "session" of RTP video packets -----------------
        // Each session has: SPS/PPS STAP-A, then 2-3 frames (IDR FU-A + P-frames + SEI)
        // with distinct 30fps timestamps so rule-4 stragglers don't silently drop.

        // --- Per-iteration parameters (randomized) ---
        let frame_count = rng.below(3) + 2; // 2 or 3 frames per session
        let mut packets = Vec::new();

        // --- SPS/PPS STAP-A at the start ---
        let sps: Vec<u8> = {
            // Random-ish SPS bytes (3+ NAL bytes, valid Annex-B)
            let mut s = Vec::new();
            s.push(0x67); // forbidden_zero=0, nal_unit_type=7 (SPS)
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            // A few SPS parameters... keep it minimal but valid
            s.extend_from_slice(&[0x42, 0x00, 0x1E]); // typical SPS constraints
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s.push(rng.below(256) as u8);
            s
        };
        let pps: Vec<u8> = {
            let mut p = Vec::new();
            p.push(0x68); // nal_unit_type=8 (PPS)
            p.push(rng.below(256) as u8);
            p.push(rng.below(256) as u8);
            p.extend_from_slice(&[0xCE, 0x06]); // typical PPS
            p.push(rng.below(256) as u8);
            p
        };
        // Helper to make a Packet (local to avoid crossing module boundaries)
        fn make_pkt_local(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> rtc::rtp::Packet {
            rtc::rtp::Packet {
                header: rtc::rtp::Header {
                    sequence_number: seq,
                    timestamp: ts,
                    marker,
                    ..Default::default()
                },
                payload: bytes::Bytes::copy_from_slice(payload),
            }
        }
        // Helper to make STAP-A (local)
        fn make_stap_a_local(sps: &[u8], pps: &[u8]) -> Vec<u8> {
            let mut payload = vec![0x78]; // nri=3, type=24 (STAP-A)
            payload.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            payload.extend_from_slice(sps);
            payload.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            payload.extend_from_slice(pps);
            payload
        }
        // Use push instead of extend_from_slice to add single packet
        packets.push(make_pkt_local(0x0000, TS0, true, &make_stap_a_local(&sps, &pps)));

        // --- Per-frame construction ---
        let mut frame_ts = TS0;
        // We use a wide seq space so the session can wrap near 0xFFF0 as allowed.
        let base_seq: u16 = {
            let tmp = rng.below(2);
            if tmp == 0 { 0xFFF0 } else { tmp as u16 }
        };

        for frame_idx in 0..frame_count {
            let is_idr = rng.below(2) == 0; // ~50% IDR frames
            let payload_size = (rng.below(1200) + 1) as usize; // 1..1200 payload bytes

            if is_idr {
                // IDR frame: start with 0x65 (IDR type), followed by payload bytes
                let mut nal_body = vec![0x65]; // IDR NAL type
                // Fill with random-ish bytes
                for _ in 0..(payload_size.saturating_sub(1)) {
                    nal_body.push(rng.below(256) as u8);
                }
                nal_body.push(0x00); // ensure at least 3 bytes

                // Fragments: vary frag_size; some frames may have single fragment,
                // others many fragments to exercise the FU-A path.
                let frag_size = if rng.below(3) == 0 { 1 } else { (rng.below(120) + 1) as usize }; // 1..121
                // Set marker on the last fragment
                let frag_ts = frame_ts;

                // Build fragments using inline implementation
                let mut frag_packets = make_fu_fragments_inline(&nal_body, base_seq.wrapping_add(frame_idx as u16), frag_size, frag_ts, false);

                // Randomly set marker on one of the fragments (not necessarily the last)
                if rng.below(4) == 0 && frag_packets.len() > 1 {
                    let marker_idx = frag_packets.len() / 2;
                    frag_packets[marker_idx].header.marker = true;
                }

                // occasionally drop the marker entirely (markerless frame)
                if rng.below(10) == 0 {
                    // clear marker from all fragments
                    for pk in &mut frag_packets {
                        pk.header.marker = false;
                    }
                }

                packets.extend_from_slice(&frag_packets);
            } else {
                // P-frame: single NAL unit (no FU-A)
                let mut nal_body = vec![0x21]; // P type = 1, nal_ref_idc=1
                for _ in 0..(payload_size.saturating_sub(1)) {
                    nal_body.push(rng.below(256) as u8);
                }
                nal_body.push(0x00); // ensure at least 3 bytes

                // P-frame with marker on the last packet of the frame
                let p_ts = frame_ts;
                packets.push(make_pkt_local(
                    base_seq.wrapping_add(frame_idx as u16),
                    p_ts,
                    true, // marker
                    &nal_body,
                ));
            }

            // Advance frame timestamp by ~30fps (30000/90000 = 33333 us step)
            // but randomize slightly so timestamps aren't perfectly contiguous
            let step = 33333u32 + (rng.below(3000) as u32); // jitter ±3ms
            frame_ts = frame_ts.wrapping_add(step);
        }

        // --- Now run both paths ---
        // Legacy path
        let legacy_events = crate::api::streaming::rtc::rtp::diff_fixtures::legacy_run(&packets);

        // New path: feed packets with increasing now_us (1ms inter-arrival)
        // respecting MARKER_FILL_WINDOW_US=12ms and MARKERLESS_BACKSTOP_US=40ms
        let mut assembler = crate::pipeline::media::assembler::AccessUnitAssembler::new();
        let mut new_events: Vec<(usize, crate::pipeline::media::assembler::AssemblyOutcome)> = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let arrival_us = 1_000 + i as u64 * 1_000; // 1 ms inter-arrival
            if let Some(outcome) = assembler.push(
                packet.header.sequence_number,
                packet.header.timestamp,
                packet.header.marker,
                &packet.payload,
                arrival_us,
            ) {
                new_events.push((i, outcome));
            }
            while let Some(outcome) = assembler.poll_outcome() {
                new_events.push((i, outcome));
            }
        }
        // Drain any remaining outcomes via flush_due
        let mut now_us = 1_000 + packets.len() as u64 * 1_000;
        // The assembler tracks internal state; we pump enough to flush everything.
        for _ in 0..(packets.len() + 10) {
            if let Some(outcome) = assembler.flush_due(now_us) {
                new_events.push((new_events.len(), outcome));
            }
            now_us += 1_000;
            while let Some(outcome) = assembler.poll_outcome() {
                new_events.push((new_events.len(), outcome));
            }
            if now_us > 10_000_000 { break; } // safety cap
        }

        // --- Collect completes from both paths (inlined, not using private functions) ---
        // Legacy completes: filter LegacyEvent::Complete
        let legacy_complete: Vec<(usize, Vec<u8>)> = legacy_events
            .iter()
            .filter_map(|event| match event {
                crate::api::streaming::rtc::rtp::diff_fixtures::LegacyEvent::Complete { at, data, .. } => Some((*at, data.clone())),
                _ => None,
            })
            .collect();

        // New completes: filter AssemblyOutcome::Complete
        let new_complete: Vec<(usize, Vec<u8>)> = new_events
            .iter()
            .filter_map(|(at, outcome)| match outcome {
                crate::pipeline::media::assembler::AssemblyOutcome::Complete(desc) => Some((*at, desc.data.clone())),
                _ => None,
            })
            .collect();

        // --- Core assertion: where both sides complete, bytes must match ---
        // Build a map of packet index -> legacy data for frames that completed
        let mut legacy_by_idx: std::collections::HashMap<usize, Vec<u8>> = std::collections::HashMap::new();
        for (at, data) in &legacy_complete {
            legacy_by_idx.insert(*at, data.clone());
        }

        // Check each new complete against legacy
        let mut iteration_failed = false;
        for (at, data) in &new_complete {
            // Find matching legacy complete by packet index
            if let Some(ld_data) = legacy_by_idx.get(&at) {
                // Both sides completed a frame with the same packet index — bytes must match
                if data != ld_data {
                    failures += 1;
                    if failures <= 1 {
                        // Compute first differing byte offset
                        let min_len = data.len().min(ld_data.len());
                        let mut first_diff_idx = 0;
                        for j in 0..min_len {
                            if data[j] != ld_data[j] {
                                first_diff_idx = j;
                                break;
                            }
                        }
                        let window = 16usize;
                        let data_start = first_diff_idx.saturating_sub(window);
                        let data_end = std::cmp::min(data.len(), first_diff_idx + window);
                        let ld_start = ld_data.len().saturating_sub(window);
                        let ld_end = std::cmp::min(ld_data.len(), first_diff_idx + window);

                        eprintln!("\n=== RANDOMIZED DIFFERENTIAL MISMATCH ===");
                        eprintln!("SEED: 0x{:016X}", 0x5DEECE66D_1234u64);
                        eprintln!("ITERATION: {}", iter);
                        eprintln!("FIRST DIFFERING BYTE OFFSET: {}", first_diff_idx);
                        eprintln!("LEGACY bytes [{:02?}..{:02?}]: {:?}", 
                            if data_start < data.len() { &data[data_start..std::cmp::min(data.len(), data_start + window)] } else { &[] },
                            if data_start < data.len() { data_start } else { 0 },
                            &data[data_start..std::cmp::min(data.len(), data_start + window)]
                        );
                        eprintln!("NEW    bytes [{:02?}..{:02?}]: {:?}", 
                            if ld_start < ld_data.len() { &ld_data[ld_start..std::cmp::min(ld_data.len(), ld_start + window)] } else { &[] },
                            if ld_start < ld_data.len() { ld_start } else { 0 },
                            &ld_data[ld_start..std::cmp::min(ld_data.len(), ld_start + window)]
                        );

                        eprintln!("LEGACY full data len: {} bytes, first 32: {:?}", ld_data.len(), &ld_data[..std::cmp::min(32usize, ld_data.len())]);
                        eprintln!("NEW    full data len: {} bytes, first 32: {:?}", data.len(), &data[..std::cmp::min(32usize, data.len())]);
                    }
                    iteration_failed = true;
                    break; // stop processing this iteration's completes
                }
            }
        }

        if iteration_failed {
            panic!(
                "randomized_differential_matches: iteration {} FAILED with byte mismatch (seed=0x{:016X})",
                iter, 0x5DEECE66D_1234u64
            );
        }
    }

    // If we reach here, all iterations passed (no panics)
    eprintln!("randomized_differential_matches: {} iterations, 0 failures", TOTAL_ITERATIONS);
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
    /// Mismatch-evidence (observe-only): arrival-order (seq, len, hash)
    /// digests of the packets this path kept for the AU. Populated on
    /// Complete records; empty for reject-class records.
    digests: Box<[mismatch_log::PacketDigest]>,
    /// Mismatch-evidence: the assembled output bytes (refcount clone on the
    /// legacy side, zero-copy adoption on the new side). Empty for rejects.
    data: bytes::Bytes,
}

impl ShadowRecord {
    /// Build a shadow-side record from a new-path outcome. `digests` is the
    /// arrival-ring evidence copy for Complete outcomes (empty otherwise).
    fn from_new(
        outcome: AssemblyOutcome,
        digests: Box<[mismatch_log::PacketDigest]>,
    ) -> Self {
        match outcome {
            AssemblyOutcome::Complete(desc) => {
                let data = bytes::Bytes::from(desc.data);
                ShadowRecord {
                    ts: desc.rtp_ts,
                    class: ShadowClass::Complete,
                    first_seq: desc.first_seq,
                    last_seq: desc.last_seq,
                    packet_count: desc.packet_count,
                    bytes_hash: Some(hash_bytes(&data)),
                    digests,
                    data,
                }
            }
            AssemblyOutcome::Damaged(desc) => ShadowRecord {
                ts: desc.rtp_ts,
                class: ShadowClass::Damaged,
                first_seq: desc.first_seq,
                last_seq: desc.last_seq,
                packet_count: desc.packet_count,
                bytes_hash: None,
                digests: Box::new([]),
                data: bytes::Bytes::new(),
            },
            AssemblyOutcome::Discarded { reason: _, rtp_ts } => ShadowRecord {
                ts: rtp_ts,
                class: ShadowClass::Discarded,
                first_seq: 0,
                last_seq: 0,
                packet_count: 0, // Discarded carries no packet bounds
                bytes_hash: None,
                digests: Box::new([]),
                data: bytes::Bytes::new(),
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

    fn observe_new(
        &mut self,
        outcome: AssemblyOutcome,
        digests: Box<[mismatch_log::PacketDigest]>,
    ) {
        let record = ShadowRecord::from_new(outcome, digests);
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
        // Boundary comparison (§11.3a): gated only when at least one side is
        // Complete; both-reject pairs are exempt (their packet sets depend on
        // when each path stopped collecting — neither is submitted) and are
        // counted separately so they stay visible.
        if legacy.packet_count > 0 && new_rec.packet_count > 0 {
            let same_bounds = (legacy.first_seq, legacy.last_seq, legacy.packet_count)
                == (
                    new_rec.first_seq,
                    new_rec.last_seq,
                    new_rec.packet_count,
                );
            if !same_bounds {
                let both_reject = matches!(
                    legacy.class,
                    ShadowClass::Abandoned | ShadowClass::Invalid
                ) && matches!(
                    new_rec.class,
                    ShadowClass::Damaged | ShadowClass::Discarded
                );
                if both_reject {
                    METRICS
                        .sh_reject_bound_artifact
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    METRICS
                        .sh_bound_mismatch
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        match classify_pair(legacy, new_rec) {
            PairVerdict::Match => {
                METRICS.sh_match.fetch_add(1, Ordering::Relaxed);
            }
            PairVerdict::BytesMismatch => {
                METRICS.sh_bytes_mismatch.fetch_add(1, Ordering::Relaxed);
                // Evidence snapshot (≤16/session, RAM, flushed at session
                // end): the only allocation point that fires on a mismatch.
                mismatch_log::capture(
                    legacy.ts,
                    legacy.first_seq,
                    legacy.last_seq,
                    legacy.packet_count,
                    &legacy.digests,
                    &new_rec.digests,
                    &legacy.data,
                    &new_rec.data,
                    legacy.bytes_hash,
                    new_rec.bytes_hash,
                );
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
            digests: Box::new([]),
            data: bytes::Bytes::new(),
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
        let record = ShadowRecord::from_new(outcome, Box::new([]));
        assert_eq!(record.class, ShadowClass::Complete);
        assert_eq!(record.ts, 90_000);
        assert_eq!((record.first_seq, record.last_seq), (10, 10));
        assert!(record.bytes_hash.is_some());

        let discarded = ShadowRecord::from_new(
            AssemblyOutcome::Discarded {
                reason: DiscardReason::Malformed,
                rtp_ts: 180_000,
            },
            Box::new([]),
        );
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
    fn markerless_frame_damaged_on_forceout_never_completes() {
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
        // §11 rule 2: the markerless frame resolves Damaged at the force-out —
        // never Complete (the old Complete-on-forceout was the hold defect).
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(
                    desc.data.is_empty(),
                    "§4: Damaged carries no bytes"
                );
            }
            other => panic!("markerless force-out must be Damaged, got {other:?}"),
        }
        // Both paths submit exactly frame 2, byte-identical.
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            1,
            "new completes only frame 2: {new:?}"
        );
        assert_eq!(
            legacy_f2[0],
            new_all[0],
            "frame 2 bytes/boundary parity"
        );
    }

    #[test]
    fn markerless_resolves_damaged_only_at_40ms_backstop() {
        // §11 rule 2: a timer NEVER yields Complete. Markerless AUs resolve
        // Damaged at the ~40 ms backstop — the old 12 ms Complete-on-timer is
        // exactly the defect the 8a gate exposed (280 byte mismatches).
        let payload = single_nal(0x01, 0x10);
        let mut assembler = AccessUnitAssembler::new();
        assert!(assembler.push(90, TS0, false, &payload, 100_000).is_none());
        assert!(
            assembler.flush_due(113_000).is_none(),
            "13 ms: a markerless AU must NOT resolve on a timer"
        );
        assert!(
            assembler.flush_due(139_000).is_none(),
            "39 ms: still inside the 40 ms backstop"
        );
        match assembler.flush_due(141_000) {
            Some(AssemblyOutcome::Damaged(desc)) => {
                assert!(
                    desc.data.is_empty(),
                    "backstop resolution must be Damaged with no bytes"
                );
            }
            other => panic!("expected Damaged at the 40 ms backstop, got {other:?}"),
        }
        // Legacy never completes a markerless frame on its own either.
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
        // §11 rule 2: markerless frame 1 resolves Damaged at the force-out
        // (never Complete); only frame 2 is submitted — byte-identical.
        match &new[0].1 {
            AssemblyOutcome::Damaged(desc) => {
                assert!(desc.data.is_empty(), "§4: Damaged carries no bytes");
            }
            other => panic!("markerless force-out must be Damaged, got {other:?}"),
        }
        let legacy_f2 = legacy_completes(&legacy);
        let new_all = new_completes(&new);
        assert_eq!(
            legacy_f2.len(),
            1,
            "legacy completes only frame 2: {legacy:?}"
        );
        assert_eq!(
            new_all.len(),
            1,
            "new completes only frame 2: {new:?}"
        );
        assert_eq!(legacy_f2[0], new_all[0], "frame 2 bytes/boundary parity");
    }
}

// Mismatch-evidence capture (8b investigation): RAM-buffered snapshots of
// sh_bytes_mismatch events, flushed at session end from metrics_log.
#[path = "mismatch_log.rs"]
pub(crate) mod mismatch_log;

// Real-capture fixture replay (§5.3) — separate file, child of this module so
// it can reuse the legacy harness above.
#[cfg(test)]
#[path = "diff_replay.rs"]
mod diff_replay;