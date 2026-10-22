use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Snapshot of per-second metrics drained from METRICS at status() time.
/// Readings are zeroed after drain so the next status() call starts fresh.
#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    pub(crate) rtp_assembly_sum_us: u64,
    pub(crate) rtp_assembly_count: u64,
    pub(crate) rtp_assembly_max_us: u64,
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
    pub(crate) stream_slip_us: u64,
    pub(crate) stream_slip_max_us: u64,
    pub(crate) stream_slip_slope_mpm: i64,
    pub(crate) audio_queue_ms: u64,
    pub(crate) vt_exit_count: u64,
    pub(crate) vt_max_0: u64,
    pub(crate) vt_max_1: u64,
    pub(crate) vt_max_2: u64,
    pub(crate) vt_max_3: u64,
    pub(crate) vt_max_4: u64,
    pub(crate) vt_max_5: u64,
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
}

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
    pub(crate) stream_slip_us: AtomicU64,
    pub(crate) stream_slip_max_us: AtomicU64,
    pub(crate) stream_slip_slope_mpm: AtomicI64,
    pub(crate) audio_queue_ms: AtomicU64,
    pub(crate) vt_exit_count: u64,
    pub(crate) vt_max_0: u64,
    pub(crate) vt_max_1: u64,
    pub(crate) vt_max_2: u64,
    pub(crate) vt_max_3: u64,
    pub(crate) vt_max_4: u64,
    pub(crate) vt_max_5: u64,
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
    stream_slip_us: AtomicU64::new(0),
    stream_slip_max_us: AtomicU64::new(0),
    stream_slip_slope_mpm: AtomicI64::new(0),
    audio_queue_ms: AtomicU64::new(0),
    vt_exit_count: 0,
    vt_max_0: 0,
    vt_max_1: 0,
    vt_max_2: 0,
    vt_max_3: 0,
    vt_max_4: 0,
    vt_max_5: 0,
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
};



/// RAM-buffered CSV log for a streaming session.
/// Rows are accumulated at 1 Hz from status() and flushed once at session end.
/// Owned by the video module; flushed from RtcWorker::drop.
pub(crate) static METRICS_CSV_LOG: LazyLock<Mutex<Option<crate::streaming::video::metrics_log::MetricsCsvLog>>> =
    LazyLock::new(|| Mutex::new(None));

/// Initialize the RAM-buffered CSV log for a new streaming session.
/// Sets the global static to a freshly opened log (header-on-open semantics).
/// Called once from RtcSession::new so the first CSV rows cannot report
/// the previous session's values.
pub(crate) fn init_csv_log() {
    // Flush any previous session's log that never got flushed, then replace with a fresh log.
    if let Some(old) = METRICS_CSV_LOG.lock().unwrap().take() {
        old.flush();
    }
    *METRICS_CSV_LOG.lock().unwrap() = Some(crate::streaming::video::metrics_log::MetricsCsvLog::new());
}

pub fn video_performance_summary() -> String {
    let rtp_sum = METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed);
    // Two lines: the HUD label clips long single-line output on the Vita panel.
    let line1 = format!(
        "d/p:{}/{} d/a:{}/{} skip:{} cap:{} repl:{} q:{} hwb:{} ageMax:{} lag:{} lagMax:{} bw:{} aq:{} slip:{} slipS:{}",
        METRICS.decoded.swap(0, Ordering::Relaxed),
        METRICS.presented.swap(0, Ordering::Relaxed),
        METRICS.stream_slip_us.load(Ordering::Relaxed),
        METRICS.stream_slip_max_us.swap(0, Ordering::Relaxed),
        METRICS.decode_us.load(Ordering::Relaxed),
        METRICS.pipeline_age_us.load(Ordering::Relaxed),
        METRICS.skipped.swap(0, Ordering::Relaxed),
        METRICS.rate_limited.swap(0, Ordering::Relaxed),
        METRICS.replaced.swap(0, Ordering::Relaxed),
        METRICS.queue_full.swap(0, Ordering::Relaxed),
        METRICS.hw_buffered.swap(0, Ordering::Relaxed),
        METRICS.pipeline_age_max_us.swap(0, Ordering::Relaxed),
        METRICS.audio_queue_ms.load(Ordering::Relaxed),
        METRICS.stream_lag_us.load(Ordering::Relaxed),
        METRICS.stream_lag_max_us.swap(0, Ordering::Relaxed),METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed),
    );
    let line2 = format!(
        "r:{rtp_average}/{rtp_max} rs:{} rst:{} wk:{} ab:{} hold:{}/{}/{} iv:{} qf:{} fl:{} tw:{}/{}/{} st:{} md:{} pick:{} pickMax:{}",
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
        METRICS.rtp_wait_drops.swap(0, Ordering::Relaxed),
        METRICS.rtp_abandons.swap(0, Ordering::Relaxed),
        METRICS.rtp_hold_saved.swap(0, Ordering::Relaxed),
        METRICS.rtp_hold_expired.swap(0, Ordering::Relaxed),
        METRICS.rtp_gaps.swap(0, Ordering::Relaxed),
        METRICS.rtp_invalid.swap(0, Ordering::Relaxed),
        METRICS.dec_queue_full.swap(0, Ordering::Relaxed),
        METRICS.flushes.load(Ordering::Relaxed),
        METRICS.twcc_sent.load(Ordering::Relaxed),
        METRICS.twcc_samples.swap(0, Ordering::Relaxed),
        METRICS.rtp_ext_any.swap(0, Ordering::Relaxed),
        METRICS.twcc_stride.swap(0, Ordering::Relaxed),
        METRICS.video_metadata_sent.swap(0, Ordering::Relaxed),
        METRICS.display_pickup_us.load(Ordering::Relaxed),
        METRICS.display_pickup_max_us.swap(0, Ordering::Relaxed),
    );
    // HUD vt segment: vt_max_0..5 and vt_exit_count
    let vt_segment = format!(
        " vt_max:{}/{}/{}/{}/{}/{} vt_exit:{}",
        METRICS.vt_max_0,
        METRICS.vt_max_1,
        METRICS.vt_max_2,
        METRICS.vt_max_3,
        METRICS.vt_max_4,
        METRICS.vt_max_5,
        METRICS.vt_exit_count,
    );
    format!("{line1}\n{line2}{vt_segment}")
}

/// Drain all per-second metrics into a snapshot, zeroing METRICS so the next
/// status() call starts clean.  The snapshot is shared with metrics_log.rs
/// for CSV row generation.
pub(crate) fn drain_metrics() -> MetricsSnapshot {
    MetricsSnapshot {
        rtp_assembly_sum_us: METRICS.rtp_assembly_sum_us.swap(0, Ordering::Relaxed),
        rtp_assembly_count: METRICS.rtp_assembly_count.swap(0, Ordering::Relaxed),
        rtp_assembly_max_us: METRICS.rtp_assembly_max_us.swap(0, Ordering::Relaxed),
        decode_us: METRICS.decode_us.swap(0, Ordering::Relaxed),
        pipeline_age_us: METRICS.pipeline_age_us.swap(0, Ordering::Relaxed),
        decoded: METRICS.decoded.swap(0, Ordering::Relaxed),
        skipped: METRICS.skipped.swap(0, Ordering::Relaxed),
        rate_limited: METRICS.rate_limited.swap(0, Ordering::Relaxed),
        presented: METRICS.presented.swap(0, Ordering::Relaxed),
        replaced: METRICS.replaced.swap(0, Ordering::Relaxed),
        queue_full: METRICS.queue_full.swap(0, Ordering::Relaxed),
        resyncs: METRICS.resyncs.swap(0, Ordering::Relaxed),
        resets: METRICS.resets.swap(0, Ordering::Relaxed),
        hw_buffered: METRICS.hw_buffered.swap(0, Ordering::Relaxed),
        pipeline_age_max_us: METRICS.pipeline_age_max_us.swap(0, Ordering::Relaxed),
        stream_lag_us: METRICS.stream_lag_us.swap(0, Ordering::Relaxed),
        stream_lag_max_us: METRICS.stream_lag_max_us.swap(0, Ordering::Relaxed),
        stream_slip_us: METRICS.stream_slip_us.swap(0, Ordering::Relaxed),
        stream_slip_max_us: METRICS.stream_slip_max_us.swap(0, Ordering::Relaxed),
        stream_slip_slope_mpm: METRICS.stream_slip_slope_mpm.swap(0, Ordering::Relaxed),
        audio_queue_ms: METRICS.audio_queue_ms.swap(0, Ordering::Relaxed),
        vt_exit_count: METRICS.vt_exit_count,
        vt_max_0: METRICS.vt_max_0,
        vt_max_1: METRICS.vt_max_1,
        vt_max_2: METRICS.vt_max_2,
        vt_max_3: METRICS.vt_max_3,
        vt_max_4: METRICS.vt_max_4,
        vt_max_5: METRICS.vt_max_5,
        rtp_hold_saved: METRICS.rtp_hold_saved.swap(0, Ordering::Relaxed),
        rtp_hold_expired: METRICS.rtp_hold_expired.swap(0, Ordering::Relaxed),
        rtp_gaps: METRICS.rtp_gaps.swap(0, Ordering::Relaxed),
        rtp_abandons: METRICS.rtp_abandons.swap(0, Ordering::Relaxed),
        rtp_wait_drops: METRICS.rtp_wait_drops.swap(0, Ordering::Relaxed),
        rtp_invalid: METRICS.rtp_invalid.swap(0, Ordering::Relaxed),
        dec_queue_full: METRICS.dec_queue_full.swap(0, Ordering::Relaxed),
        flushes: METRICS.flushes.swap(0, Ordering::Relaxed),
        twcc_sent: METRICS.twcc_sent.swap(0, Ordering::Relaxed),
        twcc_samples: METRICS.twcc_samples.swap(0, Ordering::Relaxed),
        rtp_ext_any: METRICS.rtp_ext_any.swap(0, Ordering::Relaxed),
        twcc_stride: METRICS.twcc_stride.swap(0, Ordering::Relaxed),
        video_metadata_sent: METRICS.video_metadata_sent.swap(0, Ordering::Relaxed),
        video_bandwidth_kbps: METRICS.video_bandwidth_kbps.swap(0, Ordering::Relaxed),
        display_pickup_us: METRICS.display_pickup_us.swap(0, Ordering::Relaxed),
        display_pickup_max_us: METRICS.display_pickup_max_us.swap(0, Ordering::Relaxed),
    }
}

/// Reset all per-second session metrics to zero.  Called at session start
/// (from RtcSession::new) so values do not leak across sessions.
pub(crate) fn reset_session_metrics() {
    METRICS.rtp_assembly_sum_us.store(0, Ordering::Relaxed);
    METRICS.rtp_assembly_count.store(0, Ordering::Relaxed);
    METRICS.rtp_assembly_max_us.store(0, Ordering::Relaxed);
    METRICS.decode_us.store(0, Ordering::Relaxed);
    METRICS.pipeline_age_us.store(0, Ordering::Relaxed);
    METRICS.decoded.store(0, Ordering::Relaxed);
    METRICS.skipped.store(0, Ordering::Relaxed);
    METRICS.rate_limited.store(0, Ordering::Relaxed);
    METRICS.presented.store(0, Ordering::Relaxed);
    METRICS.replaced.store(0, Ordering::Relaxed);
    METRICS.queue_full.store(0, Ordering::Relaxed);
    METRICS.resyncs.store(0, Ordering::Relaxed);
    METRICS.resets.store(0, Ordering::Relaxed);
    METRICS.hw_buffered.store(0, Ordering::Relaxed);
    METRICS.pipeline_age_max_us.store(0, Ordering::Relaxed);
    METRICS.stream_lag_us.store(0, Ordering::Relaxed);
    METRICS.stream_lag_max_us.store(0, Ordering::Relaxed);
    METRICS.stream_slip_us.store(0, Ordering::Relaxed);
    METRICS.stream_slip_max_us.store(0, Ordering::Relaxed);
    METRICS.audio_queue_ms.store(0, Ordering::Relaxed);
    METRICS.rtp_hold_saved.store(0, Ordering::Relaxed);
    METRICS.rtp_hold_expired.store(0, Ordering::Relaxed);
    METRICS.rtp_gaps.store(0, Ordering::Relaxed);
    METRICS.rtp_abandons.store(0, Ordering::Relaxed);
    METRICS.rtp_wait_drops.store(0, Ordering::Relaxed);
    METRICS.rtp_invalid.store(0, Ordering::Relaxed);
    METRICS.dec_queue_full.store(0, Ordering::Relaxed);
    METRICS.flushes.store(0, Ordering::Relaxed);
    METRICS.twcc_sent.store(0, Ordering::Relaxed);
    METRICS.twcc_samples.store(0, Ordering::Relaxed);
    METRICS.rtp_ext_any.store(0, Ordering::Relaxed);
    METRICS.twcc_stride.store(0, Ordering::Relaxed);
    METRICS.video_metadata_sent.store(0, Ordering::Relaxed);
    METRICS.video_bandwidth_kbps.store(0, Ordering::Relaxed);
    METRICS.display_pickup_us.store(0, Ordering::Relaxed);
    METRICS.display_pickup_max_us.store(0, Ordering::Relaxed);
}

    /// Flush the RAM-buffered metrics CSV log to disk once at session end.
    /// Called from RtcWorker::drop after session teardown; no filesystem I/O during session.
    /// Sets the global log to None after flush so a new session starts clean.
    pub(crate) fn flush_csv_log() {
        let mut log = METRICS_CSV_LOG.lock().unwrap();
        if let Some(log) = log.as_ref() {
            log.flush();
        }
        *log = None;
    }

