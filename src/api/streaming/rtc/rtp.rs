use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker};
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
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

const MAX_PENDING_AUDIO_PACKETS: usize = 32;
const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const AUDIO_MAX_LATE_PACKETS: u16 = 32;
const LOW_FPS_DAMAGE_LIMIT: u8 = 8;
const HIGH_FPS_DAMAGE_LIMIT: u8 = 3;
const TWCC_MAX_ANNOTATIONS: usize = 200;

/// Negotiated transport-cc extmap ID from the answer SDP (0 = not negotiated).
pub(crate) static TWCC_EXT_ID: AtomicU8 = AtomicU8::new(0);

#[derive(Default)]
pub(crate) struct VideoSampleStats {
    pub dropped: u32,
    pub source_frame_duration_us: Option<u64>,
    pub encoded_resolution: Option<(u32, u32)>,
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
    total_bytes: u64,
    twcc_annotations: Vec<(u16, Instant)>,
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
            total_bytes: 0,
            twcc_annotations: Vec::with_capacity(TWCC_MAX_ANNOTATIONS),
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

        // Parse TWCC transport sequence number from header extension.
        let twcc_ext_id = TWCC_EXT_ID.load(Ordering::Relaxed);
        if twcc_ext_id > 0 {
            for ext in &packet.header.extensions {
                crate::streaming::video::metrics::METRICS
                    .rtp_ext_any
                    .fetch_add(1, Ordering::Relaxed);
                if ext.id == twcc_ext_id && ext.payload.len() >= 2 {
                    let transport_seq =
                        u16::from_be_bytes([ext.payload[0], ext.payload[1]]);
                    if self.twcc_annotations.len() < TWCC_MAX_ANNOTATIONS {
                        self.twcc_annotations.push((transport_seq, Instant::now()));
                    }
                    crate::streaming::video::metrics::METRICS
                        .twcc_samples
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        if packet.payload.is_empty() {
            if self.next_sequence == Some(packet.header.sequence_number) {
                self.next_sequence = Some(packet.header.sequence_number.wrapping_add(1));
            }
            return stats;
        }
        self.total_bytes += 12 + packet.payload.len() as u64;

        let packet_timestamp = packet.header.timestamp;
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
                self.record_damage(worker);
                frame_was_damaged = true;
                stats.dropped = stats.dropped.saturating_add(1);
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
    pub(crate) fn total_bytes(&self) -> u64 { self.total_bytes }
    pub(crate) fn twcc_median_stride(&self) -> u16 {
        let mut gaps: Vec<u16> = self
            .twcc_annotations
            .windows(2)
            .map(|w| w[1].0.wrapping_sub(w[0].0))
            .collect();
        if gaps.is_empty() {
            return 0;
        }
        gaps.sort_unstable();
        gaps[gaps.len() / 2]
    }
    pub(crate) fn take_twcc_report(&mut self, media_ssrc: u32) -> Option<TwccReport> {
        if self.twcc_annotations.is_empty() {
            return None;
        }
        let annotations = std::mem::take(&mut self.twcc_annotations);
        let base_seq = annotations[0].0;
        let count = annotations.len() as u16;

        // Build a single RunLengthChunk covering all packets as ReceivedSmallDelta.
        // This tells the console we received every annotated packet. The recv_deltas
        // are set to a small constant (1 × 250us = 0.25ms) to satisfy the RTCP format
        // without precise per-packet timing.
use rtcp::transport_feedbacks::transport_layer_cc::PacketStatusChunk;
use rtcp::transport_feedbacks::transport_layer_cc::RecvDelta;
use rtcp::transport_feedbacks::transport_layer_cc::RunLengthChunk;
use rtcp::transport_feedbacks::transport_layer_cc::StatusChunkTypeTcc;
use rtcp::transport_feedbacks::transport_layer_cc::SymbolTypeTcc;
        let chunks = vec![PacketStatusChunk::RunLengthChunk(RunLengthChunk {
            type_tcc: StatusChunkTypeTcc::RunLengthChunk,
            packet_status_symbol: unsafe { std::mem::transmute::<u16, SymbolTypeTcc>(1) },
            run_length: count,
        })];
        let recv_deltas: Vec<_> = (0..count)
            .map(|_| RecvDelta {
                type_tcc_packet: unsafe { std::mem::transmute::<u16, SymbolTypeTcc>(1) },
                delta: 1,
            })
            .collect();

        Some(TwccReport {
            media_ssrc,
            base_sequence_number: base_seq,
            packet_status_count: count,
            reference_time: 0,
            chunks,
            recv_deltas,
        })
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

pub(crate) fn set_negotiated_twcc_ext_id(id: u8) {
    TWCC_EXT_ID.store(id, Ordering::Relaxed);
}

fn timestamp_is_newer(candidate: u32, reference: u32) -> bool {
    let distance = candidate.wrapping_sub(reference);
    distance != 0 && distance < (1 << 31)
}

struct AccessUnitInfo {
    has_idr: bool,
    resolution: Option<(u32, u32)>,
}

fn inspect_h264_access_unit(data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_idr: false,
        resolution: None,
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
                    info.resolution = SeqParameterSet::from_bits(nal.rbsp_bits())
                        .and_then(|sps| sps.pixel_dimensions())
                        .ok();
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
