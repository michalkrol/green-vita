use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

pub(crate) struct VideoMetrics {
    pub(crate) rtp_assembly_sum_us: AtomicU64,
    pub(crate) rtp_assembly_count: AtomicU64,
    pub(crate) rtp_assembly_max_us: AtomicU64,
    pub(crate) decode_us: AtomicU64,
    pub(crate) pipeline_age_us: AtomicU64,
    pub(crate) decoded: AtomicU64,
    pub(crate) skipped: AtomicU64,
    pub(crate) rate_limited: AtomicU64,
    pub(crate) presented: AtomicU64,
    pub(crate) replaced: AtomicU64,
    pub(crate) queue_full: AtomicU64,
    pub(crate) resyncs: AtomicU64,
    pub(crate) resets: AtomicU64,
    pub(crate) hw_buffered: AtomicU64,
    pub(crate) pipeline_age_max_us: AtomicU64,
    pub(crate) stream_lag_us: AtomicU64,
    pub(crate) stream_lag_max_us: AtomicU64,
    pub(crate) rtp_hold_saved: AtomicU64,
    pub(crate) rtp_hold_expired: AtomicU64,
    pub(crate) rtp_gaps: AtomicU64,
    pub(crate) rtp_abandons: AtomicU64,
    pub(crate) rtp_wait_drops: AtomicU64,
    pub(crate) rtp_invalid: AtomicU64,
    pub(crate) dec_queue_full: AtomicU64,
    pub(crate) flushes: AtomicU64,
    pub(crate) twcc_sent: AtomicU64,
    pub(crate) twcc_samples: AtomicU64,
    pub(crate) rtp_ext_any: AtomicU64,
    pub(crate) twcc_stride: AtomicU64,
    pub(crate) video_metadata_sent: AtomicU64,
    pub(crate) video_bandwidth_kbps: AtomicU64,
    pub(crate) display_pickup_us: AtomicU64,
    pub(crate) display_pickup_max_us: AtomicU64,
    // RTCP Receiver Report metrics (from Xbox)
    pub(crate) rtcp_rr_loss_fraction: AtomicU64,  // integer percent (e.g., 5% = 5)
    pub(crate) rtcp_rr_jitter: AtomicU64,         // jitter in RTP timestamp units
    pub(crate) rtcp_sr_ntp_timestamp: AtomicU64,  // NTP timestamp from SenderReport

    // RTCP Sender Report anchor (NTP wall-clock + RTP timestamp + local arrival)
    // Used to derive true capture-to-arrival latency: arrival - (sr_local + (rtp - sr_rtp) * 1e6 / 90k)
    pub(crate) sr_anchor_ntp: AtomicU64,          // NTP timestamp from SR (ms since 1900)
    pub(crate) sr_anchor_rtp_ts: AtomicU64,       // RTP timestamp from SR
    pub(crate) sr_anchor_local_us: AtomicU64,     // Local monotonic time when SR arrived (us since boot)

    // True end-to-end capture-to-arrival latency (microseconds)
    // Measures console capture queue + transport + assembly; NOT blind to pre-RTP-stamp queue
    pub(crate) capture_to_arrival_us: AtomicU64,
    pub(crate) capture_to_arrival_max_us: AtomicU64,

    // Absolute stream slip metric (microseconds, SIGNED via i64 storage)
    // Measures cumulative divergence between real elapsed time and RTP-timestamp elapsed time
    // since session start.  Anchor is session-fixed, only re-anchored on timeline discontinuity.
    // Positive = console clock running slow (delay accumulating), negative = console clock fast.
    pub(crate) stream_slip_us: AtomicI64,
    // Maximum magnitude of slip seen in the current 1-second window (unsigned, reset via swap(0))
    pub(crate) stream_slip_max_us: AtomicU64,
    // Count of discontinuity-induced re-anchors of the slip metric
    pub(crate) stream_slip_reanchor_count: AtomicU64,

    // Loss recovery metrics (Stage 3)
    // Number of NACK feedback packets sent (each packet may carry multiple sequence numbers)
    pub(crate) rtcp_nack_sent: AtomicU64,
    // Number of RTX packets received and successfully unwrapped
    pub(crate) rtx_packets_received: AtomicU64,
    // Number of raw packets observed at receive path with negotiated RTX payload type
    // (before unwrapping) — distinguishes "retransmissions never arrive" from
    // "they arrive but handler mishandles them".
    pub(crate) rtx_packets_seen: AtomicU64,
    // Number of RTP packets received for unknown track_id (neither video nor audio).
    // Used to diagnose whether RTX packets (bearing separate SSRC) are delivered at all.
    pub(crate) unknown_track_packets: AtomicU64,
    // RTX packets counted by the rtc crate's stats accumulator (before endpoint drops them).
    // This proves the console actually retransmits even when our receive path doesn't see them.
    pub(crate) rtx_crate_received: AtomicU64,
    // RTX bytes counted by the rtc crate's stats accumulator.
    pub(crate) rtx_crate_bytes: AtomicU64,
}

pub(crate) static METRICS: VideoMetrics = VideoMetrics {
    rtp_assembly_sum_us: AtomicU64::new(0),
    rtp_assembly_count: AtomicU64::new(0),
    rtp_assembly_max_us: AtomicU64::new(0),
    decode_us: AtomicU64::new(0),
    pipeline_age_us: AtomicU64::new(0),
    decoded: AtomicU64::new(0),
    skipped: AtomicU64::new(0),
    rate_limited: AtomicU64::new(0),
    presented: AtomicU64::new(0),
    replaced: AtomicU64::new(0),
    queue_full: AtomicU64::new(0),
    resyncs: AtomicU64::new(0),
    resets: AtomicU64::new(0),
    hw_buffered: AtomicU64::new(0),
    pipeline_age_max_us: AtomicU64::new(0),
    stream_lag_us: AtomicU64::new(0),
    stream_lag_max_us: AtomicU64::new(0),
    rtp_hold_saved: AtomicU64::new(0),
    rtp_hold_expired: AtomicU64::new(0),
    rtp_gaps: AtomicU64::new(0),
    rtp_abandons: AtomicU64::new(0),
    rtp_wait_drops: AtomicU64::new(0),
    rtp_invalid: AtomicU64::new(0),
    dec_queue_full: AtomicU64::new(0),
    flushes: AtomicU64::new(0),
    twcc_sent: AtomicU64::new(0),
    twcc_samples: AtomicU64::new(0),
    rtp_ext_any: AtomicU64::new(0),
    twcc_stride: AtomicU64::new(0),
    video_metadata_sent: AtomicU64::new(0),
    video_bandwidth_kbps: AtomicU64::new(0),
    display_pickup_us: AtomicU64::new(0),
    display_pickup_max_us: AtomicU64::new(0),
    rtcp_rr_loss_fraction: AtomicU64::new(0),
    rtcp_rr_jitter: AtomicU64::new(0),
    rtcp_sr_ntp_timestamp: AtomicU64::new(0),
    sr_anchor_ntp: AtomicU64::new(0),
    sr_anchor_rtp_ts: AtomicU64::new(0),
    sr_anchor_local_us: AtomicU64::new(0),
    capture_to_arrival_us: AtomicU64::new(0),
    capture_to_arrival_max_us: AtomicU64::new(0),
    stream_slip_us: AtomicI64::new(0),
    stream_slip_max_us: AtomicU64::new(0),
    stream_slip_reanchor_count: AtomicU64::new(0),
    rtcp_nack_sent: AtomicU64::new(0),
    rtx_packets_received: AtomicU64::new(0),
    rtx_packets_seen: AtomicU64::new(0),
    unknown_track_packets: AtomicU64::new(0),
    rtx_crate_received: AtomicU64::new(0),
    rtx_crate_bytes: AtomicU64::new(0),
};

/// One drained sample of every metric, taken once per second.
pub(crate) struct MetricsSnapshot {
    pub(crate) rtp_assembly_sum_us: u64,
    pub(crate) rtp_assembly_count: u64,
    pub(crate) rtp_assembly_max_us: u64,
    pub(crate) asm_avg_us: u64,
    pub(crate) decode_us: u64,
    pub(crate) pipeline_age_us: u64,
    pub(crate) decoded: u64,
    pub(crate) skipped: u64,
    pub(crate) rate_limited: u64,
    pub(crate) presented: u64,
    pub(crate) replaced: u64,
    pub(crate) queue_full: u64,
    pub(crate) resyncs: u64,
    pub(crate) resets: u64,
    pub(crate) hw_buffered: u64,
    pub(crate) pipeline_age_max_us: u64,
    pub(crate) stream_lag_us: u64,
    pub(crate) stream_lag_max_us: u64,
    pub(crate) rtp_hold_saved: u64,
    pub(crate) rtp_hold_expired: u64,
    pub(crate) rtp_gaps: u64,
    pub(crate) rtp_abandons: u64,
    pub(crate) rtp_wait_drops: u64,
    pub(crate) rtp_invalid: u64,
    pub(crate) dec_queue_full: u64,
    pub(crate) flushes: u64,
    pub(crate) twcc_sent: u64,
    pub(crate) twcc_samples: u64,
    pub(crate) rtp_ext_any: u64,
    pub(crate) twcc_stride: u64,
    pub(crate) video_metadata_sent: u64,
    pub(crate) video_bandwidth_kbps: u64,
    pub(crate) display_pickup_us: u64,
    pub(crate) display_pickup_max_us: u64,
    pub(crate) rtcp_rr_loss_fraction: u64,
    pub(crate) rtcp_rr_jitter: u64,
    pub(crate) rtcp_sr_ntp_timestamp: u64,
    pub(crate) capture_to_arrival_us: u64,
    pub(crate) capture_to_arrival_max_us: u64,
    // Absolute stream slip metrics (microseconds)
    pub(crate) stream_slip_us: i64,
    pub(crate) stream_slip_max_us: u64,
    pub(crate) stream_slip_reanchor_count: u64,
    // Loss recovery metrics (Stage 3)
    pub(crate) rtcp_nack_sent: u64,
    pub(crate) rtx_packets_received: u64,
    pub(crate) rtx_packets_seen: u64,
    pub(crate) unknown_track_packets: u64,
    // RTX metrics from crate stats accumulator (Task 1)
    pub(crate) rtx_crate_received: u64,
    pub(crate) rtx_crate_bytes: u64,
}

/// Drains all per-second metric atomics exactly once and returns a snapshot.
/// Each atomic is read with the same accessor (swap/load) as the original
/// `video_performance_summary()` used, so the HUD behavior is preserved.
pub fn drain_metrics() -> MetricsSnapshot {
    let rtp_assembly_sum_us = METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed);
    let rtp_assembly_count = METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed);
    let rtp_assembly_max_us = METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed);
    let asm_avg_us = rtp_assembly_sum_us.checked_div(rtp_assembly_count).unwrap_or(0);
    let decode_us = METRICS.decode_us.load(Ordering::Relaxed);
    let pipeline_age_us = METRICS.pipeline_age_us.load(Ordering::Relaxed);
    let decoded = METRICS.decoded.swap(0, Ordering::Relaxed);
    let skipped = METRICS.skipped.swap(0, Ordering::Relaxed);
    let rate_limited = METRICS.rate_limited.swap(0, Ordering::Relaxed);
    let presented = METRICS.presented.swap(0, Ordering::Relaxed);
    let replaced = METRICS.replaced.swap(0, Ordering::Relaxed);
    let queue_full = METRICS.queue_full.swap(0, Ordering::Relaxed);
    let resyncs = METRICS.resyncs.load(Ordering::Relaxed);
    let resets = METRICS.resets.load(Ordering::Relaxed);
    let hw_buffered = METRICS.hw_buffered.swap(0, Ordering::Relaxed);
    let pipeline_age_max_us = METRICS.pipeline_age_max_us.swap(0, Ordering::Relaxed);
    let stream_lag_us = METRICS.stream_lag_us.load(Ordering::Relaxed);
    let stream_lag_max_us = METRICS.stream_lag_max_us.swap(0, Ordering::Relaxed);
    let rtp_hold_saved = METRICS.rtp_hold_saved.swap(0, Ordering::Relaxed);
    let rtp_hold_expired = METRICS.rtp_hold_expired.swap(0, Ordering::Relaxed);
    let rtp_gaps = METRICS.rtp_gaps.swap(0, Ordering::Relaxed);
    let rtp_abandons = METRICS.rtp_abandons.swap(0, Ordering::Relaxed);
    let rtp_wait_drops = METRICS.rtp_wait_drops.swap(0, Ordering::Relaxed);
    let rtp_invalid = METRICS.rtp_invalid.swap(0, Ordering::Relaxed);
    let dec_queue_full = METRICS.dec_queue_full.swap(0, Ordering::Relaxed);
    let flushes = METRICS.flushes.load(Ordering::Relaxed);
    let twcc_sent = METRICS.twcc_sent.load(Ordering::Relaxed);
    let twcc_samples = METRICS.twcc_samples.swap(0, Ordering::Relaxed);
    let rtp_ext_any = METRICS.rtp_ext_any.swap(0, Ordering::Relaxed);
    let twcc_stride = METRICS.twcc_stride.swap(0, Ordering::Relaxed);
    let video_metadata_sent = METRICS.video_metadata_sent.swap(0, Ordering::Relaxed);
    let video_bandwidth_kbps = METRICS.video_bandwidth_kbps.load(Ordering::Relaxed);
    let display_pickup_us = METRICS.display_pickup_us.load(Ordering::Relaxed);
    let display_pickup_max_us = METRICS.display_pickup_max_us.swap(0, Ordering::Relaxed);
    let rtcp_rr_loss_fraction = METRICS.rtcp_rr_loss_fraction.load(Ordering::Relaxed);
    let rtcp_rr_jitter = METRICS.rtcp_rr_jitter.load(Ordering::Relaxed);
    let rtcp_sr_ntp_timestamp = METRICS.rtcp_sr_ntp_timestamp.load(Ordering::Relaxed);
    let capture_to_arrival_us = METRICS.capture_to_arrival_us.load(Ordering::Relaxed);
    let capture_to_arrival_max_us = METRICS.capture_to_arrival_max_us.swap(0, Ordering::Relaxed);
    // Absolute stream slip metrics (Stage 4)
    let stream_slip_us = METRICS.stream_slip_us.load(Ordering::Relaxed);
    let stream_slip_max_us = METRICS.stream_slip_max_us.swap(0, Ordering::Relaxed);
    let stream_slip_reanchor_count = METRICS.stream_slip_reanchor_count.swap(0, Ordering::Relaxed);
    // Loss recovery metrics (Stage 3)
    let rtcp_nack_sent = METRICS.rtcp_nack_sent.swap(0, Ordering::Relaxed);
    let rtx_packets_received = METRICS.rtx_packets_received.swap(0, Ordering::Relaxed);
    let rtx_packets_seen = METRICS.rtx_packets_seen.swap(0, Ordering::Relaxed);
    let unknown_track_packets = METRICS.unknown_track_packets.swap(0, Ordering::Relaxed);
    let rtx_crate_received = METRICS.rtx_crate_received.swap(0, Ordering::Relaxed);
    let rtx_crate_bytes = METRICS.rtx_crate_bytes.swap(0, Ordering::Relaxed);

    MetricsSnapshot {
        rtp_assembly_sum_us,
        rtp_assembly_count,
        rtp_assembly_max_us,
        asm_avg_us,
        decode_us,
        pipeline_age_us,
        decoded,
        skipped,
        rate_limited,
        presented,
        replaced,
        queue_full,
        resyncs,
        resets,
        hw_buffered,
        pipeline_age_max_us,
        stream_lag_us,
        stream_lag_max_us,
        rtp_hold_saved,
        rtp_hold_expired,
        rtp_gaps,
        rtp_abandons,
        rtp_wait_drops,
        rtp_invalid,
        dec_queue_full,
        flushes,
        twcc_sent,
        twcc_samples,
        rtp_ext_any,
        twcc_stride,
        video_metadata_sent,
        video_bandwidth_kbps,
        display_pickup_us,
        display_pickup_max_us,
        rtcp_rr_loss_fraction,
        rtcp_rr_jitter,
        rtcp_sr_ntp_timestamp,
        capture_to_arrival_us,
        capture_to_arrival_max_us,
        stream_slip_us,
        stream_slip_max_us,
        stream_slip_reanchor_count,
        rtcp_nack_sent,
        rtx_packets_received,
        rtx_packets_seen,
        unknown_track_packets,
        rtx_crate_received,
        rtx_crate_bytes,
    }
}

/// Formats the two-line HUD summary from an already-drained snapshot.
/// Output must be byte-identical to the previous inline implementation.
pub fn format_summary(s: &MetricsSnapshot) -> String {
    let line1 = format!(
        "d/p:{}/{} d/a:{}/{} skip:{} cap:{} repl:{} q:{} hwb:{} ageMax:{} lag:{} lagMax:{} bw:{}",
        s.decoded, s.presented, s.decode_us, s.pipeline_age_us, s.skipped, s.rate_limited,
        s.replaced, s.queue_full, s.hw_buffered, s.pipeline_age_max_us, s.stream_lag_us,
        s.stream_lag_max_us, s.video_bandwidth_kbps,
    );
    let line2 = format!(
        "r:{}/{} rs:{} rst:{} wk:{} ab:{} hold:{}/{}/{} iv:{} qf:{} fl:{} tw:{}/{}/{} st:{} md:{} pick:{} pickMax:{}",
        s.asm_avg_us, s.rtp_assembly_max_us, s.resyncs, s.resets, s.rtp_wait_drops,
        s.rtp_abandons, s.rtp_hold_saved, s.rtp_hold_expired, s.rtp_gaps, s.rtp_invalid,
        s.dec_queue_full, s.flushes, s.twcc_sent, s.twcc_samples, s.rtp_ext_any,
        s.twcc_stride, s.video_metadata_sent, s.display_pickup_us, s.display_pickup_max_us,
    );
    format!("{line1}\n{line2}")
}
