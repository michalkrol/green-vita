use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

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
    vt_exit_count: u64::new(0),
    vt_max_0: u64::new(0),
    vt_max_1: u64::new(0),
    vt_max_2: u64::new(0),
    vt_max_3: u64::new(0),
    vt_max_4: u64::new(0),
    vt_max_5: u64::new(0),
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
static METRICS_CSV_LOG: LazyLock<Mutex<Option<crate::streaming::video::MetricsCsvLog>>> =
    LazyLock::new(|| Mutex::new(None));

/// Initialize the RAM-buffered CSV log for a new streaming session.
/// Sets the global static to a freshly opened log (header-on-open semantics).
/// Called once from RtcSession::new so the first CSV rows cannot report
/// the previous session's values.
pub(crate) fn init_csv_log() {
    *METRICS_CSV_LOG.get_or_init(|| Mutex::new(Some(crate::streaming::video::MetricsCsvLog::new()))) = {
        crate::streaming::video::Mutex::new(Some(crate::streaming::video::MetricsCsvLog::new()))
    };
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
        " vt_max:{}/{}/{}/{}/{}/{}/{} vt_exit:{}",
        METRICS.vt_max_0.load(Ordering::Relaxed),
        METRICS.vt_max_1.load(Ordering::Relaxed),
        METRICS.vt_max_2.load(Ordering::Relaxed),
        METRICS.vt_max_3.load(Ordering::Relaxed),
        METRICS.vt_max_4.load(Ordering::Relaxed),
        METRICS.vt_max_5.load(Ordering::Relaxed),
        METRICS.vt_exit_count.load(Ordering::Relaxed),
    );
    format!("{line1}\n{line2}{vt_segment}")
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
        let log = METRICS_CSV_LOG.lock().unwrap();
        if let Some(log) = log.as_ref() {
            log.flush();
        }
        *METRICS_CSV_LOG.get_mut().unwrap() = None;
    }

