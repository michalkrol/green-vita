use crate::streaming::audio::AUDIO_RTP_CLOCK_RATE;
use crate::streaming::video::metrics::METRICS;
use crate::streaming::video::{STREAM_HEIGHT, STREAM_WIDTH, VideoDecodeWorker};
use crate::api::streaming::rtc::media::{is_negotiated_rtx_pt, PROCESS_START};
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
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::fs::OpenOptions;
use std::io::Write;

const MAX_PENDING_AUDIO_PACKETS: usize = 32;
const MAX_H264_ACCESS_UNIT_BYTES: usize = 2 * 1024 * 1024;
const VIDEO_RTP_CLOCK_RATE: u32 = 90_000;
const AUDIO_MAX_LATE_PACKETS: u16 = 32;
const LOW_FPS_DAMAGE_LIMIT: u8 = 8;
const HIGH_FPS_DAMAGE_LIMIT: u8 = 3;
const TWCC_MAX_ANNOTATIONS: usize = 200;
/// Max time (1 VSYNC) we wait for missing packets before dropping an incomplete AU.
/// Max time (1 VSYNC - processing overhead) we wait for missing packets before
/// dropping an incomplete AU.  The 16.67ms VSYNC window must fit assembly +
/// decode (2.9ms) + blit (1.1ms) + swap (0.3ms) = ~4.9ms processing overhead.
/// 15ms left only 1.67ms margin — guaranteed VSYNC miss.  12.5ms leaves 4.17ms.
/// How long to hold an incomplete access unit before giving up on it.
///
/// Must exceed one RTT: a NACK for the missing packets is only sent once the
/// marker has arrived, so the retransmission cannot possibly come back before
/// one round trip.  At 12.5 ms (below the LAN RTT to the console) we always
/// dropped the AU and asked for a keyframe before the repair could arrive,
/// which is why loss recovery never worked and artefacts persisted until the
/// next IDR.
const AU_ASSEMBLY_TIMEOUT: Duration = Duration::from_millis(12);

/// Catch-up: if a completed access unit lags the newest RTP timestamp we have
/// seen (any packet of a newer AU) by more than this, skip it and jump to live
/// instead of presenting the backlog.  ~6 frames at 60 fps; tolerates reordering.
/// 0 disables the mechanism entirely (A/B on hardware).
const AU_CATCHUP_LAG_THRESHOLD_US: u64 = 100_000;

/// Stall expiry for an access unit whose marker bit never arrived: abandon it
/// once no further packets have been seen for this long.  Measured from the
/// LAST observed packet (not the first) — first-packet expiry caused a resync
/// storm historically.
const AU_STALL_EXPIRY: Duration = Duration::from_millis(60);

/// Length of the lag measurement window.  The reported lag is the slip relative
/// to the best-served access unit seen within this window, so a constant
/// clock-rate difference between the console's 90 kHz RTP timeline and our
/// monotonic clock (which accumulates as a linear bias) cancels out instead of
/// being reported as queue growth.  At a 25 ppm rate difference the residual
/// tilt across one window is ~1.5 ms, negligible against the 30/50/100 ms
/// thresholds the REMB controller uses.
const LAG_WINDOW: Duration = Duration::from_secs(60);

/// A slip estimate this far *behind* schedule cannot come from queueing, so the
/// RTP timeline must have been reset (encoder rebuild, new SSRC).  Re-anchor
/// rather than pin the metric at zero for the rest of the session.
const LAG_RESET_BACKWARD_US: u64 = 1_000_000;

/// A slip estimate this far *ahead* of schedule is equally impossible; treat it
/// as a discontinuity rather than reporting a phantom latency spike.
const LAG_RESET_FORWARD_US: u64 = 5_000_000;

/// Negotiated transport-cc extmap ID from the answer SDP (0 = not negotiated).
pub(crate) static TWCC_EXT_ID: AtomicU8 = AtomicU8::new(0);

/// Counter for video-timing extension packets (extension id = 7).
pub(crate) static VT_PACKETS: AtomicU64 = AtomicU64::new(0);

/// Counter for video-timing extension packets with non-zero payload.
pub(crate) static VT_NONZERO: AtomicU64 = AtomicU64::new(0);

/// Counter to limit video-timing raw dump to first 100 packets.
static VT_DUMP_COUNTER: AtomicU32 = AtomicU32::new(0);

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
    slip_anchor: Option<(Instant, u32)>,
    slip_baseline_us: i64,
    slip_max_magnitude_us: u64,
    slip_reanchor_count: u64,
}

impl AudioRtp {
    pub(super) fn new(sample_rate: u32, payload_type: u8) -> Self {
        Self {
            samples: SampleBuilder::new(AUDIO_MAX_LATE_PACKETS, OpusPacket, sample_rate)
                .with_max_time_delay(std::time::Duration::from_millis(80)),
            payload_type,
            slip_anchor: None,
            slip_baseline_us: 0,
            slip_max_magnitude_us: 0,
            slip_reanchor_count: 0,
        }
    }

    pub(super) fn receive(&mut self, packet: Packet, audio_packets: &mut Vec<Bytes>) {
        if packet.header.payload_type != self.payload_type {
            return;
        }

        let arrived_at = Instant::now();
        self.samples.push(packet);
        while audio_packets.len() < MAX_PENDING_AUDIO_PACKETS {
            let Some(sample) = self.samples.pop() else {
                break;
            };
            let slip_us = slip_sample(&mut self.slip_anchor, &mut self.slip_baseline_us,
                &mut self.slip_max_magnitude_us, &mut self.slip_reanchor_count,
                sample.packet_timestamp, arrived_at, AUDIO_RTP_CLOCK_RATE);
            METRICS.audio_slip_us.store(slip_us, Ordering::Relaxed);
            METRICS.audio_slip_max_us.fetch_max(slip_us.unsigned_abs(), Ordering::Relaxed);
            METRICS.audio_builder_drops.fetch_add(u64::from(sample.prev_dropped_packets), Ordering::Relaxed);
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
    /// Best (smallest) raw slip observed since the current lag anchor, in
    /// microseconds.  `u64::MAX` means "no sample yet in this window".
    lag_baseline_us: u64,
    /// Session-fixed anchor for absolute slip metric.  Set on the first video AU
    /// of the stream session and only re-anchored on a genuine timeline
    /// discontinuity (backward/forward jump exceeding the reset thresholds).
    /// Never re-anchors on a time window rollover.
    slip_anchor: Option<(Instant, u32)>,
    /// Signed baseline slip (microseconds) at the current slip anchor.  Used to
    /// compute the absolute slip relative to the session start.
    slip_baseline_us: i64,
    /// Largest magnitude of slip seen in the current 1-second logging window.
    /// Reset to 0 each second via `swap(0)` in `drain_metrics()`.
    slip_max_magnitude_us: u64,
    /// Count of discontinuity-induced re-anchors of the slip metric.  Incremented
    /// each time the slip anchor is reset due to a timeline jump.  Allows
    /// post-hoc detection of discontinuities that could masquerade as latency
    /// accumulation.
    slip_reanchor_count: u64,
    /// RTP timestamp of the newest non-RTX access unit received in this session.
    pub(crate) newest_received_ts: Option<u32>,
    /// Track whether a catch-up skip was recently armed.
    catchup_armed: bool,
    total_bytes: u64,
    twcc_annotations: Vec<(u16, Instant)>,
    /// Sliding-window bitrate tracker: (bytes_snapshot, timestamp) pairs sampled
    /// every time current_bitrate_bps() is called.  Used for TWCC delay modulation.
    bitrate_sample_bytes: u64,
    bitrate_sample_at: Instant,
    /// Missing sequence numbers captured at assembly timeout, waiting to be sent as NACKs.
    /// Drained by `take_nack_sequences()`.
    pending_nack_sequences: Vec<u16>,
    /// Rate-limiting: minimum interval between NACK batches.
    last_nack_at: Option<Instant>,
}

struct PendingVideoFrame {
    timestamp: u32,
    first_packet_at: Instant,
    last_packet_at: Instant,
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
            last_packet_at: Instant::now(),
            packets: vec![packet],
        }
    }

    fn insert(&mut self, packet: Packet) {
        self.last_packet_at = Instant::now();
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

    /// Returns all missing sequence numbers between the first packet and the marker.
    /// Assumes packets have been sorted by sequence number (as done in assemble()).
    /// Sequences we still expect for the pending AU, computed wrap-aware.
    ///
    /// The previous implementation sorted raw `u16`s, so an access unit that
    /// straddles the 65535 -> 0 boundary produced roughly 65,000 bogus entries,
    /// and its post-marker loop could overrun the same way.  Everything here is
    /// measured as a wrapping distance from the marker, exactly like `assemble`.
    /// Test-only convenience wrapper; production passes the expected sequence.
    #[cfg(test)]
    fn missing_sequences(&self) -> Vec<u16> {
        self.missing_sequences_from(None)
    }

    fn missing_sequences_from(&self, expected_first: Option<u16>) -> Vec<u16> {
        let Some(marker_sequence) = self.marker_sequence() else {
            return Vec::new();
        };
        // Expected range starts at the sequence we still owe from the previous
        // AU when the caller knows it, otherwise at the lowest packet received.
        let lowest_received = self
            .packets
            .iter()
            .map(|packet| packet.header.sequence_number)
            .min_by_key(|sequence| marker_sequence.wrapping_sub(*sequence))
            .unwrap_or(marker_sequence);
        let first = expected_first.unwrap_or(lowest_received);

        let span = marker_sequence.wrapping_sub(first) as usize + 1;
        // Implausible span (stale marker, or the range wrapped oddly): let the
        // keyframe path handle it rather than NACKing tens of thousands of
        // sequences that were never part of this AU.
        if span > 512 {
            return Vec::new();
        }

        let present: std::collections::HashSet<u16> = self
            .packets
            .iter()
            .map(|packet| packet.header.sequence_number)
            .collect();

        let mut missing = Vec::new();
        let mut sequence = first;
        for _ in 0..span {
            if !present.contains(&sequence) {
                missing.push(sequence);
            }
            sequence = sequence.wrapping_add(1);
        }
        missing
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
            lag_baseline_us: u64::MAX,
            slip_anchor: None,
            slip_baseline_us: 0,
            slip_max_magnitude_us: 0,
            slip_reanchor_count: 0,
            newest_received_ts: None,
            catchup_armed: false,
            total_bytes: 0,
            twcc_annotations: Vec::with_capacity(TWCC_MAX_ANNOTATIONS),
            bitrate_sample_bytes: 0,
            bitrate_sample_at: Instant::now(),
            // Missing sequence numbers captured at assembly timeout, waiting to be sent as NACKs.
            // Drained by `take_nack_sequences()`.
            pending_nack_sequences: Vec::new(),
            // Rate-limiting: minimum interval between NACK batches.
            last_nack_at: None,
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

        // Check for video-timing extension (extmap id=7) in the packet header.
        let video_timing_ext_id = 7u8;
        if packet.header.extensions.iter().any(|ext| ext.id == video_timing_ext_id) {
            // Increment METRICS counters (not local statics)
            METRICS.vt_packets.fetch_add(1, Ordering::Relaxed);
            // Check if the extension payload is not all zeros
            if let Some(ext) = packet.header.extensions.iter().find(|ext| ext.id == video_timing_ext_id) {
                if !ext.payload.iter().all(|&b| b == 0) {
                    METRICS.vt_nonzero.fetch_add(1, Ordering::Relaxed);
                }
                // Decode pacer-exit when payload length is 13
                if ext.payload.len() == 13 {
                    let f4 = u16::from_be_bytes([ext.payload[7], ext.payload[8]]) as u64;
                    METRICS.vt_exit_ms_sum.fetch_add(f4, Ordering::Relaxed);
                    METRICS.vt_exit_ms_max.fetch_max(f4, Ordering::Relaxed);
                    METRICS.vt_exit_samples.fetch_add(1, Ordering::Relaxed);
                    // Queue estimate for the drain governor: our arrival (monotonic us)
                    // minus the console's wire time (RTP capture timestamp + pacer-exit
                    // delta). It spans two clocks so the absolute value carries an
                    // unknown constant; the session minimum is the best-case path and
                    // the governor uses the difference (buffer depth above best case).
                    let now_us = PROCESS_START.elapsed().as_micros() as i64;
                    let wire_us = (packet.header.timestamp as i64) * 1_000_000 / 90_000
                        + (f4 as i64) * 1_000;
                    let transit_us = now_us - wire_us;
                    METRICS.vt_transit_us_last.store(transit_us, Ordering::Relaxed);
                    METRICS.vt_transit_us_min.fetch_min(transit_us, Ordering::Relaxed);
                }
                // Whole-session sampling: append a line at most once every 500 ms
                // (using a static AtomicU64 holding the last dump's PROCESS_START.elapsed().as_micros()),
                // with a total-lines cap of 6000 (reusing VT_DUMP_COUNTER).
                static VT_DUMP_LAST_MICROS: AtomicU64 = AtomicU64::new(0);
                let now_micros: u64 = PROCESS_START.elapsed().as_micros().try_into().unwrap_or(0);
                let last_dump = VT_DUMP_LAST_MICROS.load(Ordering::Relaxed);
                let line_count = VT_DUMP_COUNTER.load(Ordering::Relaxed);
                if now_micros.saturating_sub(last_dump) >= 500_000 && line_count < 6000 {
                    VT_DUMP_LAST_MICROS.store(now_micros, Ordering::Relaxed);
                    VT_DUMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let rtp_ts = packet.header.timestamp;
                    let seq = packet.header.sequence_number;
                    let f4 = if ext.payload.len() == 13 {
                        u16::from_be_bytes([ext.payload[7], ext.payload[8]]) as u64
                    } else {
                        0
                    };
                    let hex_bytes: String = ext.payload.iter().map(|b| format!("{:02x}", b)).collect();
                    // Use OpenOptions to create/append to the log file.
                    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("ux0:data/xcloud-rust/video-timing.log") {
                        let _ = writeln!(file, "{} {} {} {} {}", now_micros, rtp_ts, seq, f4, hex_bytes);
                    }
                }
            }
        }

        // Handle RTX (retransmission) packets: unwrap the original sequence number
        // and feed the payload into the normal reassembly path. RTX packets must
        // not disturb the main stream's sequence accounting, gap detection, or metrics.
        if is_negotiated_rtx_pt(packet.header.payload_type) {
            // Count raw RTX packets seen at receive path (before unwrapping) for metrics.
            crate::streaming::video::metrics::METRICS
                .rtx_packets_seen
                .fetch_add(1, Ordering::Relaxed);
            if packet.payload.len() >= 2 {
                let original_seq = u16::from_be_bytes([packet.payload[0], packet.payload[1]]);
                let inner_payload = &packet.payload[2..];
                if !inner_payload.is_empty() {
                    // Create a synthetic packet with the original sequence number and payload
                    // but preserve the original timestamp, marker, and SSRC for assembly.
                    let mut rtx_packet = packet.clone();
                    rtx_packet.header.sequence_number = original_seq;
                    rtx_packet.payload = Bytes::copy_from_slice(inner_payload);
                    // Process the unwrapped packet through the normal path, but skip
                    // RTX-specific metrics and sequence accounting.
                    return self.receive_unwrapped_rtx(worker, rtx_packet, keyframe_requested);
                }
            }
            // Malformed RTX packet (too short); count and drop.
            crate::streaming::video::metrics::METRICS
                .rtp_invalid
                .fetch_add(1, Ordering::Relaxed);
            return stats;
        }

        if packet.payload.is_empty() {
            if self.next_sequence == Some(packet.header.sequence_number) {
                self.next_sequence = Some(packet.header.sequence_number.wrapping_add(1));
            }
            return stats;
        }
        self.total_bytes += 12 + packet.payload.len() as u64;

        // Track the newest non-RTX RTP timestamp for catch-up decisions.
        if self.newest_received_ts.is_none_or(|newest| timestamp_is_newer(packet.header.timestamp, newest)) {
            self.newest_received_ts = Some(packet.header.timestamp);
        }

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
            FrameAssembly::Pending => {
                // Only drop the AU on timeout if the marker bit has already arrived
                // but assembly still fails (real sequence gap).  Without a marker,
                // we're still receiving packets — don't fire early.
                if let Some(pending) = &self.pending
                    && pending.marker_sequence().is_some()
                    && pending.first_packet_at.elapsed() > AU_ASSEMBLY_TIMEOUT
                {
                    // Capture missing sequence numbers for NACK generation.
                    // This replaces the old single-marker approach with actual gap data.
                    let missing = pending.missing_sequences_from(self.next_sequence);
                    if !missing.is_empty() {
                        self.pending_nack_sequences.extend(missing);
                    }
                    self.pending = None;
                    self.next_sequence = None;
                    *keyframe_requested = true;
                    self.record_damage(worker);
                    stats.dropped = stats.dropped.saturating_add(1);
                }
                return stats;
            }
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
        let au_timestamp = completed.timestamp;
        self.update_stream_lag(completed.timestamp, completed.first_packet_at);
        self.update_stream_slip(completed.timestamp, completed.first_packet_at);
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

        if let Some(lag_us) = catchup_decision(self.newest_received_ts, au_timestamp, unit.has_idr) {
            // Jump to live: drop this whole AU and let the existing keyframe hold keep
            // the last good frame on screen until a clean reference (IDR) arrives.
            self.waiting_for_keyframe = true;
            self.damage_score = 0;
            self.catchup_armed = true;
            *keyframe_requested = true;
            crate::streaming::video::metrics::METRICS
                .catchup_aus_skipped
                .fetch_add(1, Ordering::Relaxed);
            crate::streaming::video::metrics::METRICS
                .catchup_lag_max_us
                .fetch_max(lag_us, Ordering::Relaxed);
            let queued = worker.discard_queued_access_units();
            if queued > 0 {
                crate::streaming::video::metrics::METRICS
                    .catchup_aus_skipped
                    .fetch_add(queued as u64, Ordering::Relaxed);
            }
            return stats;
        }

        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    /// Processes an unwrapped RTX packet (retransmission) by feeding it into the
    /// normal reassembly path WITHOUT updating the main stream's sequence accounting,
    /// gap/damage detection, stream-lag metric, or decoder queue. RTX packets are
    /// duplicates by definition and must not count as new frames or trigger keyframe
    /// requests on their own.
    fn receive_unwrapped_rtx(
        &mut self,
        worker: &VideoDecodeWorker,
        packet: Packet,
        keyframe_requested: &mut bool,
    ) -> VideoSampleStats {
        let mut stats = VideoSampleStats::default();
        
        // Count RTX packets received for metrics
        crate::streaming::video::metrics::METRICS
            .rtx_packets_received
            .fetch_add(1, Ordering::Relaxed);

        let packet_timestamp = packet.header.timestamp;
        
        // Skip timestamp validation for RTX packets - they belong to a frame
        // that may already be pending or even completed. We only care about
        // assembling the missing pieces.
        
        // If we have no pending frame, this RTX is for an already-completed frame.
        // Check if it matches the last completed frame's timestamp.
        if self.pending.is_none() {
            // RTX for an already-assembled frame - nothing to do
            return stats;
        }

        // If the pending frame has a different timestamp, this RTX is for an
        // older frame that we've already moved past (keyframe requested, etc.)
        if let Some(pending) = &self.pending
            && pending.timestamp != packet_timestamp
        {
            return stats;
        }

        // Insert the RTX packet into the pending frame assembly
        if let Some(pending) = &mut self.pending {
            pending.insert(packet);
        } else {
            // Should not happen given the check above, but handle gracefully
            return stats;
        }

        // Try to assemble the frame (same logic as normal receive)
        let assembly = self
            .pending
            .as_ref()
            .map(|pending| pending.assemble(&mut self.depacketizer, self.next_sequence));
        let Some(assembly) = assembly else {
            return stats;
        };
        let (data, marker_sequence) = match assembly {
            FrameAssembly::Pending => {
                // Still waiting for more packets - RTX helped but not complete yet
                return stats;
            }
            FrameAssembly::Invalid => {
                // Assembly failed - this shouldn't happen for RTX but handle gracefully
                self.pending = None;
                self.next_sequence = None;
                *keyframe_requested = true;
                self.record_damage(worker);
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            FrameAssembly::Complete { data, marker_sequence } => (data, marker_sequence),
        };
        
        let completed = self.pending.take().expect("assembled pending video frame");
        
        // NOTE: We do NOT update stream lag, capture-to-arrival, or next_sequence
        // for RTX-recovered frames. They are duplicates and must not affect metrics.
        
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
            if !unit.has_idr {
                *keyframe_requested = true;
                stats.dropped = stats.dropped.saturating_add(1);
                return stats;
            }
            self.waiting_for_keyframe = false;
            self.damage_score = 0;
        } else {
            // RTX recovery succeeded - reduce damage score since we filled a gap
            self.damage_score = self.damage_score.saturating_sub(1);
        }

        if let Some(lag_us) = catchup_decision(self.newest_received_ts, packet_timestamp, unit.has_idr) {
            // Jump to live: drop this whole AU and let the existing keyframe hold keep
            // the last good frame on screen until a clean reference (IDR) arrives.
            // RTX-recovered AUs are old by definition; skip forward rather than re-inject staleness.
            self.waiting_for_keyframe = true;
            self.damage_score = 0;
            self.catchup_armed = true;
            *keyframe_requested = true;
            crate::streaming::video::metrics::METRICS
                .catchup_aus_skipped
                .fetch_add(1, Ordering::Relaxed);
            crate::streaming::video::metrics::METRICS
                .catchup_lag_max_us
                .fetch_max(lag_us, Ordering::Relaxed);
            let queued = worker.discard_queued_access_units();
            if queued > 0 {
                crate::streaming::video::metrics::METRICS
                    .catchup_aus_skipped
                    .fetch_add(queued as u64, Ordering::Relaxed);
            }
            return stats;
        }

        if !worker.submit_access_unit(data.to_vec(), self.source_frame_duration_us) {
            eprintln!("Video decoder queue is full; continuing while requesting a keyframe");
            *keyframe_requested = true;
            stats.dropped = stats.dropped.saturating_add(1);
        }
        stats
    }

    /// Drains pending NACK sequences and returns them for transmission.
    /// Called once per pump tick to generate NACK feedback packets.
    pub(crate) fn take_nack_sequences(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.pending_nack_sequences)
    }

    /// Generates RTCP TransportLayerNack packets from missing sequence numbers.
    /// Batches multiple sequence numbers per packet using bitmask coverage (RFC 4585).
    /// Rate-limits to at most one NACK packet per `NACK_MIN_INTERVAL`.
    pub(crate) fn generate_nack_packets(
        &mut self,
        media_ssrc: u32,
        now: Instant,
    ) -> Vec<rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack> {
        const NACK_MIN_INTERVAL: Duration = Duration::from_millis(50);
        // A single NACK packet can carry far more than 20 sequences: each pair is
        // a 16-bit PID plus a 16-bit BLP, so ~100 pairs still fit comfortably in
        // one RTCP packet.  The old limit of 20 silently discarded the rest of
        // the gap forever, so retransmissions never covered whole losses.
        const MAX_NACKS_PER_PACKET: usize = 128;
        const LOCAL_RTCP_SENDER_SSRC: u32 = 1;

        if self.last_nack_at.is_some_and(|last| now.duration_since(last) < NACK_MIN_INTERVAL) {
            return Vec::new();
        }

        let sequences = self.take_nack_sequences();
        if sequences.is_empty() {
            return Vec::new();
        }

        // Sort and deduplicate
        let mut seqs = sequences;
        seqs.sort_unstable();
        seqs.dedup();

        // Limit the batch, but keep the remainder queued so the next batch
        // (one NACK_MIN_INTERVAL later) still asks for it.  Truncating and
        // dropping the remainder loses those sequences for good.
        if seqs.len() > MAX_NACKS_PER_PACKET {
            let remainder = seqs.split_off(MAX_NACKS_PER_PACKET);
            self.pending_nack_sequences.extend(remainder);
        }

        // Use the rtcp crate's helper to convert sequences to NackPairs
        let nack_pairs = rtcp::transport_feedbacks::transport_layer_nack::nack_pairs_from_sequence_numbers(&seqs);

        self.last_nack_at = Some(now);

        vec![rtcp::transport_feedbacks::transport_layer_nack::TransportLayerNack {
            sender_ssrc: LOCAL_RTCP_SENDER_SSRC,
            media_ssrc,
            nacks: nack_pairs,
        }]
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
    
    /// Drift-free stream lag: how much later than the best-served access unit in
    /// the current window this one arrived.  See `lag_sample` and `LAG_WINDOW`.
    fn update_stream_lag(&mut self, au_timestamp: u32, au_first_packet_at: Instant) {
        let lag_us = lag_sample(
            &mut self.stream_clock_anchor,
            &mut self.lag_baseline_us,
            au_timestamp,
            au_first_packet_at,
        );
        crate::streaming::video::metrics::METRICS
            .stream_lag_us
            .store(lag_us, Ordering::Relaxed);
        crate::streaming::video::metrics::METRICS
            .stream_lag_max_us
            .fetch_max(lag_us, Ordering::Relaxed);
    }

    /// Absolute stream slip: divergence between real elapsed time and RTP-timestamp
    /// elapsed time, measured against a SESSION-FIXED anchor that is never
    /// re-based on a window rollover.  Only re-anchors on a genuine timeline
    /// discontinuity (reusing the same guards as `lag_sample`), and counts those
    /// re-anchors so a discontinuity cannot masquerade as accumulated latency.
    ///
    /// The returned value is SIGNED (i64):
    ///   - positive  = console clock running slow (delay accumulating)
    ///   - negative  = console clock running fast (ahead of schedule)
    /// Only the TREND matters; we do not clamp to zero or take absolute value.
    fn update_stream_slip(&mut self, au_timestamp: u32, au_first_packet_at: Instant) {
        let slip_us = slip_sample(
            &mut self.slip_anchor,
            &mut self.slip_baseline_us,
            &mut self.slip_max_magnitude_us,
            &mut self.slip_reanchor_count,
            au_timestamp,
            au_first_packet_at,
            VIDEO_RTP_CLOCK_RATE,
        );
        crate::streaming::video::metrics::METRICS
            .stream_slip_us
            .store(slip_us, Ordering::Relaxed);
        // Track max magnitude in the current window (reset each second via swap(0))
        crate::streaming::video::metrics::METRICS
            .stream_slip_max_us
            .fetch_max(slip_us.unsigned_abs(), Ordering::Relaxed);
    }

    /// Expire an access unit whose marker bit never arrived and after which no
    /// further packets were seen (e.g. a burst followed by silence).  The
    /// newer-timestamp abandonment path cannot fire when nothing newer arrives,
    /// which would otherwise strand the frame forever.
    ///
    /// Only applies when the marker is absent and the stall is measured from the
    /// LAST observed packet.  The marker-present + first-packet timeout path
    /// above is intentionally left untouched.
    pub(crate) fn flush_expired(
        &mut self,
        worker: &VideoDecodeWorker,
        keyframe_requested: &mut bool,
        stats: &mut crate::api::streaming::rtc::rtp::VideoSampleStats,
    ) {
        let stalled = match self.pending.as_ref() {
            Some(pending) => {
                pending.marker_sequence().is_none()
                    && pending.last_packet_at.elapsed() >= AU_STALL_EXPIRY
            }
            None => false,
        };
        if !stalled {
            return;
        }

        self.pending = None;
        self.next_sequence = None;
        self.depacketizer = H264Packet::default();
        *keyframe_requested = true;
        self.record_damage(worker);
        stats.dropped = stats.dropped.saturating_add(1);
        crate::streaming::video::metrics::METRICS
            .rtp_hold_expired
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn reset_lag_anchor(&mut self) {
        self.stream_clock_anchor = None;
        self.lag_baseline_us = u64::MAX;
    }

    /// Reset the session-fixed slip anchor to None (effectively re-anchoring at the next AU).
    pub(crate) fn reset_slip_anchor(&mut self) {
        self.slip_anchor = None;
        self.slip_baseline_us = 0;
        self.slip_max_magnitude_us = 0;
        self.slip_reanchor_count = 0;
    }
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

    pub(crate) fn twcc_annotation_count(&self) -> usize {
        self.twcc_annotations.len()
    }

    /// Rolling-window ingess bitrate in bps, sampled on call.
    /// Returns 0 until enough data accumulates (~250ms of data).
    pub(crate) fn current_bitrate_bps(&mut self) -> u32 {
        let now = Instant::now();
        let elapsed_us = now.duration_since(self.bitrate_sample_at).as_micros() as u64;
        if elapsed_us < 200_000 {
            return 0;
        }
        let bytes = self.total_bytes.saturating_sub(self.bitrate_sample_bytes);
        self.bitrate_sample_bytes = self.total_bytes;
        self.bitrate_sample_at = now;
        (bytes as u64 * 8_000_000 / elapsed_us.max(1)) as u32
    }

    pub(crate) fn take_twcc_report(
        &mut self,
        media_ssrc: u32,
        _current_bitrate_bps: u32,
        _ceiling_bps: u32,
        twcc_delay_injection: bool,
        twcc_delay_ramp_max: u8,
    ) -> Option<TwccReport> {
        if self.twcc_annotations.is_empty() {
            return None;
        }
        let annotations = std::mem::take(&mut self.twcc_annotations);
        let base_seq = annotations[0].0;
        let count = annotations.len() as u16;

        // Use real packet arrival timestamps to compute inter-packet deltas.
        // Flat delta=1 (0.25ms per packet) tells the Xbox "14 packets arrived
        // in 3.5ms" when reality is ~12ms — the GCC sees negative delay and
        // ramps bitrate up, causing 16-packet bursts that exceed WiFi airtime.
        // Real deltas let GCC find the stable 4.5-5 Mbps sweet spot (~7-9
        // packets per frame, arriving in <8ms, zero drops).
        //
        // Clamp max delta to 6 (1.5ms) so transient jitter spikes don't
        // make GCC overreact and throttle too low.
        // If delay injection is enabled and we're over the ceiling, inject
        // synthetic delay to signal congestion to the sender's GCC.
        // Ramp: max delta = twcc_delay_ramp_max * base_delta (capped at 15).
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
            .map(|i| {
                let base_delta = if i == 0 {
                    1 // first packet: small placeholder delta
                } else {
                    let elapsed_us = annotations[i as usize].1
                        .duration_since(annotations[i as usize - 1].1)
                        .as_micros() as u64;
                    (elapsed_us / 250).clamp(1, 6) as i64
                };
                // If delay injection is enabled, apply ramp to signal congestion
                let delta = if twcc_delay_injection && base_delta > 0 {
                    // Ramp up the delay proportionally to signal congestion
                    // Max delta = twcc_delay_ramp_max * base_delta, capped at 15
                    let ramp_max = twcc_delay_ramp_max as i64;
                    (base_delta * ramp_max).min(15)
                } else {
                    base_delta
                };
                RecvDelta {
                    type_tcc_packet: unsafe { std::mem::transmute::<u16, SymbolTypeTcc>(1) },
                    delta,
                }
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

/// Microseconds of stream time between two RTP timestamps, wrapping-safe.
fn rtp_span_us(anchor_timestamp: u32, au_timestamp: u32, clock_rate: u32) -> u64 {
    u64::from(au_timestamp.wrapping_sub(anchor_timestamp))
        .saturating_mul(1_000_000)
        / u64::from(clock_rate)
}

/// Drift-tolerant schedule-slip filter.
///
/// Returns the arrival slip above the *best* slip seen since the anchor, which
/// removes the linear bias a session-long anchor accumulates from a clock-rate
/// difference between the console's RTP timeline and our monotonic clock.  The
/// anchor is refreshed when the window rolls over and when the RTP timeline
/// jumps (either direction), so a base reset can neither pin the metric at zero
/// forever nor produce a phantom spike.  The first sample of every window
/// reports 0.
fn lag_sample(
    anchor: &mut Option<(Instant, u32)>,
    baseline_us: &mut u64,
    au_timestamp: u32,
    au_at: Instant,
) -> u64 {
    let Some((anchor_at, anchor_timestamp)) = *anchor else {
        *anchor = Some((au_at, au_timestamp));
        *baseline_us = u64::MAX;
        return 0;
    };

    let elapsed = au_at.saturating_duration_since(anchor_at);
    let actual_us = elapsed.as_micros() as u64;
    let expected_us = rtp_span_us(anchor_timestamp, au_timestamp, VIDEO_RTP_CLOCK_RATE);

    // Window rollover bounds the uncorrected clock-rate tilt.
    if elapsed >= LAG_WINDOW {
        *anchor = Some((au_at, au_timestamp));
        *baseline_us = u64::MAX;
        return 0;
    }
    // Timeline discontinuity: re-anchor instead of reporting a phantom spike
    // (forward jump) or pinning the metric at zero for the rest of the session
    // (backward jump).
    if expected_us > actual_us.saturating_add(LAG_RESET_BACKWARD_US)
        || actual_us > expected_us.saturating_add(LAG_RESET_FORWARD_US)
    {
        *anchor = Some((au_at, au_timestamp));
        *baseline_us = u64::MAX;
        return 0;
    }

    let raw_us = schedule_slip_us(anchor_at, anchor_timestamp, au_at, au_timestamp);
    *baseline_us = (*baseline_us).min(raw_us);
    raw_us - *baseline_us
}

/// Expected-vs-actual arrival slip for one access unit, in microseconds.
/// `expected` is how long the RTP timestamps say should have elapsed since the
/// anchor; `actual` is how long really elapsed.  Never negative.
fn schedule_slip_us(
    anchor_at: Instant,
    anchor_timestamp: u32,
    au_at: Instant,
    au_timestamp: u32,
) -> u64 {
    let expected_us = u64::from(au_timestamp.wrapping_sub(anchor_timestamp))
        .saturating_mul(1_000_000)
        / u64::from(VIDEO_RTP_CLOCK_RATE);
    let actual_us = au_at.saturating_duration_since(anchor_at).as_micros() as u64;
    actual_us.saturating_sub(expected_us)
}

/// Signed absolute slip relative to a session-fixed anchor.
///
/// Computes: (au_at - anchor_at).as_micros() - rtp_span_us(anchor_ts, au_ts, clock_rate)
/// Result is SIGNED (i64):
///   - positive  = real time elapsed more than RTP time (console clock slow, delay accumulating)
///   - negative  = real time elapsed less than RTP time (console clock fast)
///
/// The anchor is set on the first video AU and ONLY re-anchored on a genuine
/// timeline discontinuity (reusing the LAG_RESET_BACKWARD_US / LAG_RESET_FORWARD_US
/// guards from lag_sample).  It NEVER re-anchors on a time window rollover.
///
/// Returns the current slip (signed i64) and updates max_magnitude_us and
/// reanchor_count as side effects.
fn slip_sample(
    anchor: &mut Option<(Instant, u32)>,
    baseline_us: &mut i64,
    max_magnitude_us: &mut u64,
    reanchor_count: &mut u64,
    au_timestamp: u32,
    au_at: Instant,
    clock_rate: u32,
) -> i64 {
    let Some((anchor_at, anchor_timestamp)) = *anchor else {
        // First video AU of the session: establish the session-fixed anchor.
        *anchor = Some((au_at, au_timestamp));
        *baseline_us = 0;
        *max_magnitude_us = 0;
        return 0;
    };

    let elapsed = au_at.saturating_duration_since(anchor_at);
    let actual_us = elapsed.as_micros() as i64;
    let expected_us = rtp_span_us(anchor_timestamp, au_timestamp, clock_rate) as i64;

    // Timeline discontinuity detection (same guards as lag_sample):
    // - Backward jump: RTP timeline went backwards (encoder rebuild, new SSRC)
    // - Forward jump:  RTP timeline jumped ahead implausibly
    // In either case, re-anchor the slip metric and count the event.
    if expected_us > actual_us.saturating_add(LAG_RESET_BACKWARD_US as i64)
        || actual_us > expected_us.saturating_add(LAG_RESET_FORWARD_US as i64)
    {
        *anchor = Some((au_at, au_timestamp));
        *baseline_us = 0;
        *max_magnitude_us = 0;
        *reanchor_count = reanchor_count.saturating_add(1);
        return 0;
    }

    // Absolute slip: signed difference between real and RTP elapsed time.
    let slip_us = actual_us - expected_us;

    // Track the maximum magnitude seen in the current logging window.
    let magnitude = slip_us.unsigned_abs();
    if magnitude > *max_magnitude_us {
        *max_magnitude_us = magnitude;
    }

    // Return slip relative to the session baseline (which stays at 0 unless
    // a discontinuity re-anchors).  This makes the metric an absolute measure
    // of cumulative clock drift since session start.
    slip_us - *baseline_us
}

/// `Some(lag_us)` = this access unit must be skipped to catch up to live.
///
/// The lag is how far this AU sits *behind* the newest timestamp we have seen:
/// `newest - au_timestamp` in RTP time.  The arguments to `rtp_span_us` are the
/// earlier timestamp first; swapping them wraps the subtraction and reports a
/// ~47,000 s lag for a frame that is a single tick behind, which would skip
/// every frame and hold for a keyframe forever.
fn catchup_decision(newest: Option<u32>, au_timestamp: u32, has_idr: bool) -> Option<u64> {
    if AU_CATCHUP_LAG_THRESHOLD_US == 0 || has_idr { return None; }
    let newest = newest?;
    let lag_us = rtp_span_us(au_timestamp, newest, VIDEO_RTP_CLOCK_RATE);
    (lag_us > AU_CATCHUP_LAG_THRESHOLD_US).then_some(lag_us)
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

#[cfg(test)]
mod tests {
    use super::{lag_sample, schedule_slip_us, LAG_WINDOW, PendingVideoFrame};
    use std::time::{Duration, Instant};

    #[test]
    fn zero_slip_when_arrival_matches_schedule() {
        let anchor = Instant::now();
        // 30 frames at 90 kHz == 1 second of RTP time, arriving 1s later.
        assert_eq!(
            schedule_slip_us(anchor, 0, anchor + Duration::from_secs(1), 90_000),
            0
        );
    }

    #[test]
    fn reports_positive_slip_when_late() {
        let anchor = Instant::now();
        // RTP says 0.5s elapsed, but 0.75s really elapsed -> 250ms slip.
        assert_eq!(
            schedule_slip_us(anchor, 0, anchor + Duration::from_millis(750), 45_000),
            250_000
        );
    }

    #[test]
    fn clamps_early_or_reordered_arrival_to_zero() {
        let anchor = Instant::now();
        assert_eq!(
            schedule_slip_us(anchor, 0, anchor, 45_000),
            0
        );
    }

    #[test]
    fn wraps_across_the_rtp_timestamp_space() {
        let anchor = Instant::now();
        // Anchor near u32::MAX; the next timestamp wraps around.
        let anchor_ts = u32::MAX - 45_000 + 1;
        assert_eq!(
            schedule_slip_us(anchor, anchor_ts, anchor + Duration::from_secs(1), 90_000 - 45_000),
            0
        );
    }

    #[test]
    fn first_lag_sample_of_a_window_reports_zero() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        assert_eq!(lag_sample(&mut anchor, &mut baseline, 0, t0), 0);
    }

    #[test]
    fn lag_reports_growth_above_the_window_best() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        // Anchor: ts 0 at t0.
        assert_eq!(lag_sample(&mut anchor, &mut baseline, 0, t0), 0);
        // 15 ms elapsed against 10 ms of RTP time -> 5 ms slip, the new best.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 900, t0 + Duration::from_millis(15)),
            0
        );
        // 60 ms elapsed against 20 ms of RTP time -> 40 ms slip, i.e. 35 ms
        // above the window's best.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 1_800, t0 + Duration::from_millis(60)),
            35_000
        );
    }

    #[test]
    fn lag_window_rollover_discards_accumulated_drift() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        assert_eq!(lag_sample(&mut anchor, &mut baseline, 0, t0), 0);
        // 60 s of wall clock against 59 s of RTP time: a 1 s "tilt" that a
        // session-long anchor reported forever.  The window boundary discards it.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 59 * 90_000, t0 + LAG_WINDOW),
            0
        );
    }

    #[test]
    fn lag_backward_timeline_jump_reanchors_and_stays_alive() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        assert_eq!(lag_sample(&mut anchor, &mut baseline, 1_000_000, t0), 0);
        // Console restarts its RTP base at 0: expected becomes astronomically
        // large.  Re-anchor instead of pinning lag at zero forever.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 0, t0 + Duration::from_millis(17)),
            0
        );
        // First sample of the new window establishes its baseline.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 1_800, t0 + Duration::from_millis(117)),
            0
        );
        // A later 60 ms of growth is still reported: the metric is alive.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 3_600, t0 + Duration::from_millis(197)),
            60_000
        );
    }

    #[test]
    fn lag_forward_timeline_jump_is_not_a_phantom_spike() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        assert_eq!(lag_sample(&mut anchor, &mut baseline, 0, t0), 0);
        // One frame of RTP time but 10 s of wall clock.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 1_500, t0 + Duration::from_secs(10)),
            0
        );
    }

    #[test]
    fn lag_reports_growth_only_not_a_steady_queue() {
        let mut anchor = None;
        let mut baseline = u64::MAX;
        let t0 = Instant::now();
        // ~40 ms queue present from the start.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 1_500, t0 + Duration::from_millis(57)),
            0
        );
        // Same queue one frame later: no growth, so still zero.
        assert_eq!(
            lag_sample(&mut anchor, &mut baseline, 3_000, t0 + Duration::from_millis(73)),
            0
        );
    }

    #[test]
    fn missing_sequences_detects_gaps() {
        use rtc::rtp::Packet;
        use rtc::rtp::header::Header;
        use bytes::Bytes;

        let mut frame = PendingVideoFrame::new(Packet {
            header: Header {
                sequence_number: 100,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 101,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 103, // gap at 102
                timestamp: 90000,
                marker: true,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 104,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 106, // gap at 105
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });

        let missing = frame.missing_sequences();
        assert_eq!(missing, vec![102, 105]);
    }

    #[test]
    fn missing_sequences_handles_wraparound() {
        use rtc::rtp::Packet;
        use rtc::rtp::header::Header;
        use bytes::Bytes;

        let mut frame = PendingVideoFrame::new(Packet {
            header: Header {
                sequence_number: 65534,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 65535,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 0, // wrap
                timestamp: 90000,
                marker: true,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 2, // gap at 1
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });

        let missing = frame.missing_sequences();
        assert_eq!(missing, vec![1]);
    }

    #[test]
    fn missing_sequences_no_marker_returns_empty() {
        use rtc::rtp::Packet;
        use rtc::rtp::header::Header;
        use bytes::Bytes;

        let mut frame = PendingVideoFrame::new(Packet {
            header: Header {
                sequence_number: 100,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        frame.insert(Packet {
            header: Header {
                sequence_number: 101,
                timestamp: 90000,
                marker: false,
                ..Default::default()
            },
            payload: Bytes::new(),
        });
        // No marker bit set
        let missing = frame.missing_sequences();
        assert!(missing.is_empty());
    }

    #[test]
    fn first_sample_after_reset_returns_zero() {
        let mut anchor = None;
        let mut baseline_us = 0i64;
        let mut max_magnitude_us = 0u64;
        let mut reanchor_count = 0u64;
        let t0 = Instant::now();
        // First sample with anchor None establishes the session anchor; slip is 0.
        assert_eq!(
            slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 0, t0, AUDIO_RTP_CLOCK_RATE),
            0
        );
        // Second sample with matching cadence (48 kHz: +960 ticks, +20 ms real) ≈ 0.
        let t1 = t0 + Duration::from_millis(20);
        let slip = slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 960, t1, AUDIO_RTP_CLOCK_RATE);
        assert!(slip.abs() < 1_000, "expected <1 ms after reset, got {slip} us");
        // Reset the anchor: the next sample must read 0, not the old baseline.
        anchor = None;
        baseline_us = 0;
        max_magnitude_us = 0;
        assert_eq!(
            slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 0, t1, AUDIO_RTP_CLOCK_RATE),
            0
        );
    }

    #[test]
    fn signed_late_check_ts_4800_at_150ms_real_is_50ms() {
        let mut anchor = None;
        let mut baseline_us = 0i64;
        let mut max_magnitude_us = 0u64;
        let mut reanchor_count = 0u64;
        let t0 = Instant::now();
        // Establish the session anchor first: a fresh anchor always reports 0.
        assert_eq!(
            slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 0, t0, AUDIO_RTP_CLOCK_RATE),
            0
        );
        // RTP time advanced 100 ms (4800 ticks at 48 kHz) while 150 ms of real
        // time elapsed -> +50 ms of positive slip.
        let t1 = t0 + Duration::from_millis(150);
        let slip = slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 4_800, t1, AUDIO_RTP_CLOCK_RATE);
        assert_eq!(slip, 50_000, "expected +50 ms of slip, got {slip} us");
    }

    #[test]
    fn wrap_near_u32_max() {
        let mut anchor = None;
        let mut baseline_us = 0i64;
        let mut max_magnitude_us = 0u64;
        let mut reanchor_count = 0u64;
        let t0 = Instant::now();
        // Anchor 4800 ticks below the wrap point.
        let anchor_ts = u32::MAX - 4_800 + 1;
        assert_eq!(
            slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, anchor_ts, t0, AUDIO_RTP_CLOCK_RATE),
            0
        );
        // 4800 ticks later the timestamp has wrapped through 0; the elapsed real
        // time matches exactly, so the wrap must not read as a timeline jump.
        let t1 = t0 + Duration::from_micros(100_000);
        let slip = slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 0, t1, AUDIO_RTP_CLOCK_RATE);
        assert_eq!(slip, 0, "wrap must report 0, got {slip} us");
        assert_eq!(reanchor_count, 0, "a wrap must not count as a re-anchor");
    }

    #[test]
    fn discontinuity_backward_jump_over_1s() {
        let mut anchor = None;
        let mut baseline_us = 0i64;
        let mut max_magnitude_us = 0u64;
        let mut reanchor_count = 0u64;
        let t0 = Instant::now();
        // First sample establishes the anchor.
        assert_eq!(
            slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, 0, t0, AUDIO_RTP_CLOCK_RATE),
            0
        );
        // The RTP timeline jumps backwards by ~47,000 s of RTP time: a
        // discontinuity, so re-anchor and report 0 instead of pinning the metric.
        let t1 = t0 + Duration::from_secs(2);
        let slip = slip_sample(&mut anchor, &mut baseline_us, &mut max_magnitude_us, &mut reanchor_count, u32::MAX, t1, AUDIO_RTP_CLOCK_RATE);
        assert_eq!(slip, 0);
        assert_eq!(reanchor_count, 1);
    }

    #[test]
    fn catchup_decision_cases() {
        use crate::api::streaming::rtc::rtp::catchup_decision;
        // The threshold is a const, so its 0-disabled path cannot be exercised
        // here; every case below uses the compiled-in 100 ms value.
        // No newest timestamp yet → nothing to catch up to.
        assert_eq!(catchup_decision(None, 0, false), None);
        // A keyframe is a clean jump point: never skip it.
        assert_eq!(catchup_decision(Some(0), 0, true), None);
        // Zero lag → present in order.
        assert_eq!(catchup_decision(Some(90_000), 90_000, false), None);
        // 900 ticks behind (10 ms) is under the threshold → present in order.
        assert_eq!(catchup_decision(Some(90_000), 89_100, false), None);
        // 30_000 ticks at 90 kHz = 333_333 us (333 ms) > 100 ms → skip, and the
        // returned value is microseconds, not RTP ticks.
        assert_eq!(catchup_decision(Some(120_000), 90_000, false), Some(333_333));
        // A timestamp newer than the AU under test yields no lag.
        // (In production `newest` is the maximum seen, so this only guards the
        // wrap: an AU just before the wrap point is still "behind" in modulo time.)
        assert_eq!(catchup_decision(Some(1_000), u32::MAX - 100, false), None);
        // …and just over the threshold across the same wrap is still detected.
        assert_eq!(catchup_decision(Some(1_000), u32::MAX - 10_000, false), Some(122_233));
    }
}
