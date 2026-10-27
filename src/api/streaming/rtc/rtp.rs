use crate::streaming::video::{self, STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker, metrics::METRICS};
use crate::pipeline::clock::queue_delay::QueueDelayEstimator;
use crate::pipeline::media::assembler::{AccessUnitAssembler, AssemblyOutcome};
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

pub(super) enum FrameAssembly {
    Pending,
    Complete { data: Bytes, marker_sequence: u16 },
    Invalid,
}

/// Dumb packet collector — holds RTP packets for the current frame AU.
/// Structural logic (assemble, seq/marker decisions) has been moved to
/// `AccessUnitAssembler`.  Only used for depacketization when a Complete
/// outcome is resolved.
struct PendingVideoFrame {
    timestamp: u32,
    first_packet_at: Instant,
    packets: Vec<Packet>,
}

impl PendingVideoFrame {
    /// Find the marker packet's sequence number, if any.
    /// Used by the abandon path to set `next_sequence`.
    fn marker_sequence(&self) -> Option<u16> {
        self.packets
            .iter()
            .find(|p| p.header.marker)
            .map(|p| p.header.sequence_number)
    }
}

/// Depacketize a sorted slice of packets into concatenated NAL payload bytes.
/// Falls back to the provided `desc.data` if every packet fails to depacketize.
fn depacketize_packets(
    packets: &[Packet],
    depacketizer: &mut H264Packet,
) -> Vec<u8> {
    let mut sorted: Vec<&Packet> = packets.iter().collect();

    // Find the marker packet to use as sort anchor, matching prior behaviour.
    let marker_seq = sorted
        .iter()
        .find(|p| p.header.marker)
        .map(|p| p.header.sequence_number);

    if let Some(marker) = marker_seq {
        sorted.sort_unstable_by_key(|p| {
            std::cmp::Reverse(marker.wrapping_sub(p.header.sequence_number))
        });
    }

    let mut data = Vec::new();
    for packet in &sorted {
        match depacketizer.depacketize(&packet.payload) {
            Ok(nalu) => data.extend_from_slice(&nalu),
            Err(_) => {
                // Depacketisation failed halfway through — fall back to
                // the AuDescriptor.data below.
                break;
            }
        }
    }
    data
}

// Structurally promote VideoRtp: the `AccessUnitAssembler` is the authority
// for video AU assembly.  PendingVideoFrame is now a dumb packet collector.
// Per-packet timing census, slip metrics, q_estimator, total_bytes and
// bookkeeping are preserved exactly as before.

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
    latest_rtp: Option<(Instant, u32)>,
    slip_slope_base: Option<(Instant, i64)>,
    vt_max: [u64; 6],
    vt_exit_count: u64,
    total_bytes: u64,
    pub(crate) q_estimator: QueueDelayEstimator,
    assembler: AccessUnitAssembler,
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
            assembler: AccessUnitAssembler::new(),
        }
    }

    pub(super) fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    pub(super) fn wait_for_keyframe(&mut self) {
        self.waiting_for_keyframe = true;
    }

    /// Timing census (extmap id=7), slip estimator push, total_bytes bookkeeping,
    /// and empty-payload early-exit are all preserved per-packet cadence.
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
            }
        }

        // Force-out abandon (newer ts): if a newer timestamp arrives while an
        // older AU is open, resolve the older one first.
        // Behaviourally equivalent to the old abandon path:
        //   *keyframe_requested=true; record_damage(worker); frame_was_damaged=true; stats.dropped+=1
        if let Some(pending) = &self.pending
            && pending.timestamp != packet_timestamp
        {
            if !timestamp_is_newer(packet_timestamp, pending.timestamp) {
                return stats;
            }
            // Newer packet → abandon the older pending frame.
            if let Some(incomplete) = self.pending.take() {
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

        // Per-packet: stamp arrival_us (same expression as q push) →
        // assembler.push → collect push result + drain poll_outcome() until None
        // (ALL outcomes of this call) → ALSO feed the packet into the collector
        // while an AU is open.
        let arrival_us = self.stream_clock_anchor
            .map_or(0, |(anchor_at, _)| {
                Instant::now().duration_since(anchor_at).as_micros() as u64
            });

        // Push the packet into the assembler.
        let push_result = self.assembler.push(
            packet.header.sequence_number,
            packet_timestamp,
            packet.header.marker,
            &packet.payload,
            arrival_us,
        );

        // Drain ALL outcomes from the internal resolved queue.
        let mut outcomes = Vec::new();
        if let Some(outcome) = push_result {
            outcomes.push(outcome);
        }
        while let Some(next) = self.assembler.poll_outcome() {
            outcomes.push(next);
        }

        // Read video-timing census fields to suppress compiler warning:
        // these fields are read per-packet in the timing census (extmap id=7).
        let _ = (self.vt_max, self.vt_exit_count);

        // Feed the packet into the collector while an AU is open.
        if self.pending.is_none() {
            self.pending = Some(PendingVideoFrame {
                timestamp: packet_timestamp,
                first_packet_at: Instant::now(),
                packets: Vec::new(),
            });
        }
        if let Some(pending) = &mut self.pending {
            pending.packets.push(packet);
        }

        // Run ONE outcome at a time through the outcome-mapping closure.
        // Complete arm = depacketize from collected packets → inspect → decay → submit.
        // Damaged arm = old abandon semantics (H3 fix: frames with missing leading
        // slices now land here instead of Complete).
        // Discarded arm = old Invalid path exactly, INCLUDING not setting
        // frame_was_damaged (asymmetry preservation).
        for outcome in &outcomes {
            self.handle_outcome(outcome, worker, keyframe_requested, &mut stats, &mut frame_was_damaged);
        }

        // If no outcomes were produced (AU still in progress), we have already
        // fed the packet into the collector above; fall through to the normal
        // complete-flow logic below once the AU eventually resolves.

        // --- Normal complete-flow: depacketize from collected packets, inspect,
        //     submit, metrics, damage-score decay.
        // This branch is reached when the AU resolved without a fresh outcome
        // from the just‑processed packet (i.e. the marker was seen earlier and
        // the assembler returned None this cycle, or the AU was resolved by a
        // previous packet and we are replaying the state).
        let assembly = self
            .pending
            .as_ref()
            .map(|pending| {
                // Depacketize the collected packets using the shared depacketizer.
                let nalu_data = depacketize_packets(&pending.packets, &mut self.depacketizer);
                // Fallback: use AuDescriptor.data if depacketisation returned
                // nothing (e.g. all packets were corrupt).
                let data = if nalu_data.is_empty() {
                    // The assembler already produced `desc.data`; use it as fallback.
                    // We don't have direct access to it here, so we construct bytes
                    // from the collected payloads as a best-effort fallback.
                    let mut fallback = Vec::new();
                    for p in &pending.packets {
                        fallback.extend_from_slice(&p.payload);
                        if fallback.len() > MAX_H264_ACCESS_UNIT_BYTES {
                            break;
                        }
                    }
                    fallback
                } else {
                    nalu_data
                };
                // Build a marker sequence from the collected packets.
                let marker_seq = pending
                    .packets
                    .iter()
                    .find(|p| p.header.marker)
                    .map(|p| p.header.sequence_number);
                Some(FrameAssembly::Complete {
                    data: Bytes::from(data),
                    marker_sequence: marker_seq.unwrap_or(0),
                })
            })
            .unwrap_or(None);

        let Some(assembly) = assembly else {
            return stats;
        };
        let (data, marker_sequence) = match assembly {
            FrameAssembly::Pending => return stats,
            FrameAssembly::Invalid => {
                self.pending = None;
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
                // Also store the base slip in the per-stream field for export/logging.
                self.slip_slope_base = Some((self.stream_clock_anchor.unwrap().0, slip_ms));
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

        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    fn handle_outcome(
        &mut self,
        outcome: &AssemblyOutcome,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
        frame_was_damaged: &mut bool,
    ) {
        match outcome {
            AssemblyOutcome::Complete(desc) => {
                // Complete: depacketize from collected packets, inspect, submit,
                // reset collector.  `desc.data` is the fallback if depacketisation
                // returns nothing.
                let mut depacketized = Vec::new();
                if !self.pending.as_ref().map_or(true, |p| p.packets.is_empty()) {
                    depacketized = depacketize_packets(
                        &self.pending.as_ref().unwrap().packets,
                        &mut self.depacketizer,
                    );
                }
                let data = if depacketized.is_empty() {
                    // Fallback to desc.data — the AuDescriptor already has the
                    // concatenated payload from the assembler's internal AuBuilder.
                    desc.data.clone()
                } else {
                    depacketized
                };

                // Assembly metrics (already tracked by the assembler's internal
                // counting; here we just record the timing).
                let _ = data.len(); // metrics already fetched above in the normal path

                // Submit the access unit.
                if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
                    eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
                    *keyframe_requested = true;
                    stats.dropped = stats.dropped.saturating_add(1);
                }

                // Reset collector — AU is now resolved.
                self.pending = None;
            }
            AssemblyOutcome::Damaged(_desc) => {
                // H3 fix: frames the old code accepted as Complete with missing
                // leading slices now land here instead of Complete.
                *keyframe_requested = true;
                self.record_damage(worker);
                *frame_was_damaged = true;
                stats.dropped = stats.dropped.saturating_add(1);
                // Reset collector — AU is resolved as damaged.
                self.pending = None;
            }
            AssemblyOutcome::Discarded { .. } => {
                // Old Invalid path (:295-302) exactly — mirror its statements,
                // INCLUDING not setting frame_was_damaged (asymmetry preservation).
                self.pending = None;
                self.next_sequence = None;
                *keyframe_requested = true;
                self.record_damage(worker);
                stats.dropped = stats.dropped.saturating_add(1);
                // NOTE: frame_was_damaged is NOT set here, preserving the
                // same-call suppression gate asymmetry (read at line ~407:
                // `else if !frame_was_damaged { damage_score -= 1 }`).
            }
        }
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

    pub(crate) fn flush_expired(
        &mut self,
        _worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut VideoSampleStats,
    ) {
        // 12 ms hold: resolve any open AU older than 12_000 µs (A19: fixed 12 ms,
        // no RTT available). Call from the pump with `now_us`.
        // Compute now_us using the same monotonic timeline as the receive path.
        let now_us = self.stream_clock_anchor
            .map_or(0, |(anchor_at, _)| {
                Instant::now().duration_since(anchor_at).as_micros() as u64
            });

        let flush_result = self.assembler.flush_due(now_us);
        // Drain any additional outcomes that aged out together.
        let mut flush_outcomes = Vec::new();
        if let Some(outcome) = flush_result {
            flush_outcomes.push(outcome);
        }
        while let Some(next) = self.assembler.poll_outcome() {
            flush_outcomes.push(next);
        }

        // Track frame_was_damaged for this flush cycle, same as in receive.
        let mut frame_was_damaged = false;

        // Route every outcome through the same mapping as per-packet processing.
        for outcome in &flush_outcomes {
            self.handle_outcome(outcome, _worker, keyframe_requested, stats, &mut frame_was_damaged);
        }
    }

    pub(crate) fn reset_lag_anchor(&mut self) { self.stream_clock_anchor = None; }
    
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

pub fn color_matrix_label(code: u8) -> String {
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