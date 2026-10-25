use std::sync::{LazyLock, Mutex};
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
    pub(crate) slip_reanchors: u64,
    pub(crate) age_drops: u64,
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
    pub(crate) gap_hole: u64,
    pub(crate) gap_au: u64,
    pub(crate) gap_unknown: u64,
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
    pub(crate) present_us: u64,
    pub(crate) present_max_us: u64,
    /// PLI (Picture Loss Indicator) counters per trigger
    pub(crate) pli_submit: u64,
    pub(crate) pli_gap: u64,
    pub(crate) pli_expired: u64,
    pub(crate) pli_too_large: u64,
    pub(crate) pli_wait_idr: u64,
    pub(crate) pli_decode_err: u64,
    pub(crate) pli_watchdog: u64,
    pub(crate) pli_periodic: u64,
    pub(crate) pli_sent: u64,
    pub(crate) qd_us: u64,
    pub(crate) qd_rel_us: u64,
    pub(crate) ext_ast_id: u64,
    pub(crate) ext_vt_id: u64,
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
    pub(crate) slip_reanchors: AtomicU64,
    pub(crate) age_drops: AtomicU64,
    pub(crate) audio_queue_ms: AtomicU64,
    pub(crate) vt_exit_count: AtomicU64,
    pub(crate) vt_max_0: AtomicU64,
    pub(crate) vt_max_1: AtomicU64,
    pub(crate) vt_max_2: AtomicU64,
    pub(crate) vt_max_3: AtomicU64,
    pub(crate) vt_max_4: AtomicU64,
    pub(crate) vt_max_5: AtomicU64,
    pub(crate) rtp_hold_saved: AtomicU64,
    pub(crate) rtp_hold_expired: AtomicU64,
    pub(crate) rtp_gaps: AtomicU64,
    pub(crate) rtp_abandons: AtomicU64,
    pub(crate) rtp_wait_drops: AtomicU64,
    pub(crate) rtp_invalid: AtomicU64,
    pub(crate) gap_hole: AtomicU64,
    pub(crate) gap_au: AtomicU64,
    pub(crate) gap_unknown: AtomicU64,
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
    pub(crate) present_us: AtomicU64,
    pub(crate) present_max_us: AtomicU64,
    pub(crate) pli_submit: AtomicU64,
    pub(crate) pli_gap: AtomicU64,
    pub(crate) pli_expired: AtomicU64,
    pub(crate) pli_too_large: AtomicU64,
    pub(crate) pli_wait_idr: AtomicU64,
    pub(crate) pli_decode_err: AtomicU64,
    pub(crate) pli_watchdog: AtomicU64,
    pub(crate) pli_periodic: AtomicU64,
    pub(crate) pli_sent: AtomicU64,
    pub(crate) qd_us: AtomicU64,
    pub(crate) qd_rel_us: AtomicU64,
    pub(crate) ext_ast_id: AtomicU64,
    pub(crate) ext_vt_id: AtomicU64,
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
    slip_reanchors: AtomicU64::new(0),
    age_drops: AtomicU64::new(0),
    audio_queue_ms: AtomicU64::new(0),
    vt_exit_count: AtomicU64::new(0),
    vt_max_0: AtomicU64::new(0),
    vt_max_1: AtomicU64::new(0),
    vt_max_2: AtomicU64::new(0),
    vt_max_3: AtomicU64::new(0),
    vt_max_4: AtomicU64::new(0),
    vt_max_5: AtomicU64::new(0),
    rtp_hold_saved: AtomicU64::new(0),
    rtp_hold_expired: AtomicU64::new(0),
    rtp_gaps: AtomicU64::new(0),
    rtp_abandons: AtomicU64::new(0),
    rtp_wait_drops: AtomicU64::new(0),
    rtp_invalid: AtomicU64::new(0),
    gap_hole: AtomicU64::new(0),
    gap_au: AtomicU64::new(0),
    gap_unknown: AtomicU64::new(0),
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
    present_us: AtomicU64::new(0),
    present_max_us: AtomicU64::new(0),
    // PLI (Picture Loss Indicator) counters per trigger
    pli_submit: AtomicU64::new(0),
    pli_gap: AtomicU64::new(0),
    pli_expired: AtomicU64::new(0),
    pli_too_large: AtomicU64::new(0),
    pli_wait_idr: AtomicU64::new(0),
    pli_decode_err: AtomicU64::new(0),
    pli_watchdog: AtomicU64::new(0),
    pli_periodic: AtomicU64::new(0),
    pli_sent: AtomicU64::new(0),
    qd_us: AtomicU64::new(0),
    qd_rel_us: AtomicU64::new(0),
    ext_ast_id: AtomicU64::new(0),
    ext_vt_id: AtomicU64::new(0),
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
pub(crate) fn init_csv_log(
    color_profile: crate::settings::ColorProfile,
    h264_profile: crate::settings::H264Profile,
) {
    // Flush any previous session's log that never got flushed, then replace with a fresh log.
    if let Some(old) = METRICS_CSV_LOG.lock().unwrap().take() {
        old.flush();
    }
    *METRICS_CSV_LOG.lock().unwrap() = Some(
        crate::streaming::video::metrics_log::MetricsCsvLog::new(color_profile, h264_profile)
    );
}

pub fn video_performance_summary() -> String {
    // Use load() instead of swap(0) to read current metrics without draining.
    // The actual draining is handled by drain_metrics() in the status() method.
    let rtp_sum = METRICS.rtp_assembly_sum_us.load(Ordering::Relaxed);
    let rtp_count = METRICS.rtp_assembly_count.load(Ordering::Relaxed);
    let rtp_average = rtp_sum.checked_div(rtp_count).unwrap_or(0);
    let rtp_max = METRICS.rtp_assembly_max_us.load(Ordering::Relaxed);
    let present_us = METRICS.present_us.load(Ordering::Relaxed);
    let present_max_us = METRICS.present_max_us.load(Ordering::Relaxed);
    let qd_us = METRICS.qd_us.load(Ordering::Relaxed);
    let qd_rel_us = METRICS.qd_rel_us.load(Ordering::Relaxed);
    // Two lines: the HUD label clips long single-line output on the Vita panel.
    let line1 = format!(
        "d/p:{}/{} d/a:{}/{} skip:{} cap:{} repl:{} q:{} hwb:{} ageMax:{} lag:{} lagMax:{} bw:{} aq:{} slip:{} slipS:{} sa:{}",
        METRICS.decoded.load(Ordering::Relaxed),
        METRICS.presented.load(Ordering::Relaxed),
        METRICS.decode_us.load(Ordering::Relaxed) / 1000,
        METRICS.pipeline_age_us.load(Ordering::Relaxed) / 1000,
        METRICS.skipped.load(Ordering::Relaxed),
        METRICS.rate_limited.load(Ordering::Relaxed),
        METRICS.replaced.load(Ordering::Relaxed),
        METRICS.queue_full.load(Ordering::Relaxed),
        METRICS.hw_buffered.load(Ordering::Relaxed),
        METRICS.pipeline_age_max_us.load(Ordering::Relaxed),
        METRICS.stream_lag_us.load(Ordering::Relaxed) / 1000,
        METRICS.stream_lag_max_us.load(Ordering::Relaxed) / 1000,
        METRICS.video_bandwidth_kbps.load(Ordering::Relaxed),
        METRICS.audio_queue_ms.load(Ordering::Relaxed),
        METRICS.stream_slip_us.load(Ordering::Relaxed),
        METRICS.stream_slip_slope_mpm.load(Ordering::Relaxed),
        METRICS.slip_reanchors.load(Ordering::Relaxed),
    );
    let line2 = format!(
        "r:{rtp_average}/{rtp_max} rs:{} rst:{} wk:{} ab:{} hold:{}/{}/{} iv:{} qf:{} fl:{} tw:{}/{}/{} stride:{} md:{} pick:{} pickMax:{} ps:{present_us} psPk:{present_max_us} qd:{qd_us} qd_rel:{qd_rel_us} pli:{} plf:{} pv:{} pvM:{}",
        METRICS.resyncs.load(Ordering::Relaxed),
        METRICS.resets.load(Ordering::Relaxed),
        METRICS.rtp_wait_drops.load(Ordering::Relaxed),
        METRICS.rtp_abandons.load(Ordering::Relaxed),
        METRICS.rtp_hold_saved.load(Ordering::Relaxed),
        METRICS.rtp_hold_expired.load(Ordering::Relaxed),
        METRICS.rtp_gaps.load(Ordering::Relaxed),
        METRICS.rtp_invalid.load(Ordering::Relaxed),
        METRICS.dec_queue_full.load(Ordering::Relaxed),
        METRICS.flushes.load(Ordering::Relaxed),
        METRICS.twcc_sent.load(Ordering::Relaxed),
        METRICS.twcc_samples.load(Ordering::Relaxed),
        METRICS.rtp_ext_any.load(Ordering::Relaxed),
        METRICS.twcc_stride.load(Ordering::Relaxed),
        METRICS.video_metadata_sent.load(Ordering::Relaxed),
        METRICS.display_pickup_us.load(Ordering::Relaxed),
        METRICS.display_pickup_max_us.load(Ordering::Relaxed),
        METRICS.pli_sent.load(Ordering::Relaxed),
        METRICS.pli_periodic.load(Ordering::Relaxed),
        METRICS.pli_gap.load(Ordering::Relaxed),
        METRICS.pli_expired.load(Ordering::Relaxed),
    );
    // HUD vt segment: vt_max_0..5 and vt_exit_count
    let vt_segment = format!(
        " vt_max:{}/{}/{}/{}/{}/{} vt_exit:{}",
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
        slip_reanchors: METRICS.slip_reanchors.swap(0, Ordering::Relaxed),
        age_drops: METRICS.age_drops.swap(0, Ordering::Relaxed),
        audio_queue_ms: METRICS.audio_queue_ms.swap(0, Ordering::Relaxed),
        vt_exit_count: METRICS.vt_exit_count.swap(0, Ordering::Relaxed),
        vt_max_0: METRICS.vt_max_0.swap(0, Ordering::Relaxed),
        vt_max_1: METRICS.vt_max_1.swap(0, Ordering::Relaxed),
        vt_max_2: METRICS.vt_max_2.swap(0, Ordering::Relaxed),
        vt_max_3: METRICS.vt_max_3.swap(0, Ordering::Relaxed),
        vt_max_4: METRICS.vt_max_4.swap(0, Ordering::Relaxed),
        vt_max_5: METRICS.vt_max_5.swap(0, Ordering::Relaxed),
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
        present_us: METRICS.present_us.swap(0, Ordering::Relaxed),
        present_max_us: METRICS.present_max_us.load(Ordering::Relaxed),
        qd_us: METRICS.qd_us.load(Ordering::Relaxed),
        qd_rel_us: METRICS.qd_rel_us.load(Ordering::Relaxed),
        ext_ast_id: METRICS.ext_ast_id.load(Ordering::Relaxed),
        ext_vt_id: METRICS.ext_vt_id.load(Ordering::Relaxed),
        pli_submit: METRICS.pli_submit.swap(0, Ordering::Relaxed),
        pli_gap: METRICS.pli_gap.swap(0, Ordering::Relaxed),
        pli_expired: METRICS.pli_expired.swap(0, Ordering::Relaxed),
        pli_too_large: METRICS.pli_too_large.swap(0, Ordering::Relaxed),
        pli_wait_idr: METRICS.pli_wait_idr.swap(0, Ordering::Relaxed),
        pli_decode_err: METRICS.pli_decode_err.swap(0, Ordering::Relaxed),
        pli_watchdog: METRICS.pli_watchdog.swap(0, Ordering::Relaxed),
        pli_periodic: METRICS.pli_periodic.swap(0, Ordering::Relaxed),
        pli_sent: METRICS.pli_sent.swap(0, Ordering::Relaxed),
        gap_hole: METRICS.gap_hole.swap(0, Ordering::Relaxed),
        gap_au: METRICS.gap_au.swap(0, Ordering::Relaxed),
        gap_unknown: METRICS.gap_unknown.swap(0, Ordering::Relaxed),
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
    METRICS.stream_slip_slope_mpm.store(0, Ordering::Relaxed);
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
    METRICS.present_us.store(0, Ordering::Relaxed);
    METRICS.present_max_us.store(0, Ordering::Relaxed);
    METRICS.qd_us.store(0, Ordering::Relaxed);
    METRICS.qd_rel_us.store(0, Ordering::Relaxed);
    METRICS.ext_ast_id.store(0, Ordering::Relaxed);
    METRICS.ext_vt_id.store(0, Ordering::Relaxed);
    METRICS.slip_reanchors.store(0, Ordering::Relaxed);
    METRICS.age_drops.store(0, Ordering::Relaxed);
    METRICS.pli_submit.store(0, Ordering::Relaxed);
    METRICS.pli_gap.store(0, Ordering::Relaxed);
    METRICS.pli_expired.store(0, Ordering::Relaxed);
    METRICS.pli_too_large.store(0, Ordering::Relaxed);
    METRICS.pli_wait_idr.store(0, Ordering::Relaxed);
    METRICS.pli_decode_err.store(0, Ordering::Relaxed);
    METRICS.pli_watchdog.store(0, Ordering::Relaxed);
    METRICS.pli_periodic.store(0, Ordering::Relaxed);
    METRICS.pli_sent.store(0, Ordering::Relaxed);
    METRICS.gap_hole.store(0, Ordering::Relaxed);
    METRICS.gap_au.store(0, Ordering::Relaxed);
    METRICS.gap_unknown.store(0, Ordering::Relaxed);
    METRICS.vt_exit_count.store(0, Ordering::Relaxed);
    METRICS.vt_max_0.store(0, Ordering::Relaxed);
    METRICS.vt_max_1.store(0, Ordering::Relaxed);
    METRICS.vt_max_2.store(0, Ordering::Relaxed);
    METRICS.vt_max_3.store(0, Ordering::Relaxed);
    METRICS.vt_max_4.store(0, Ordering::Relaxed);
    METRICS.vt_max_5.store(0, Ordering::Relaxed);
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

