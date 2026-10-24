//! Append-only CSV logging of the once-per-second metrics snapshot, so a
//! streaming session can be analysed offline.
//!
//! This version buffers rows in RAM and flushes once at session end,
//! avoiding filesystem I/O during the hot path.
//!
//! CSV is written to ux0:data/xcloud-rust/metrics.csv at session teardown.
//! During the session, zero touches ux0:.
//!
//! Build-time flag: when METRICS_CSV_LOGGING is false, no CSV file is
//! created/opened/appended at any point. All in-memory metrics, HUD, and
//! status-string behavior remain exactly as before.

use super::metrics::{MetricsSnapshot, drain_metrics, reset_session_metrics};
use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::flags::METRICS_CSV_LOGGING;

use crate::settings::{ColorProfile, H264Profile};

const GIT_SHA: &str = env!("GIT_SHA");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Extra per-tick values that live on the video receiver rather than in METRICS.
pub(crate) struct RowExtras {
    pub(crate) src_fps: u64,
    pub(crate) enc_width: u32,
    pub(crate) enc_height: u32,
    pub(crate) waiting_keyframe: bool,
    pub(crate) drops: u64,
    pub(crate) decode_errors: u64,
}

const MAX_ROWS: usize = 20_000;

/// RAM-buffered CSV log for a streaming session.
/// Rows are accumulated at 1 Hz from status() and flushed once at session end.
#[derive(Debug)]
pub(crate) struct MetricsCsvLog {
    /// Protected buffer of formatted CSV rows (no file I/O during session).
    rows: Arc<Mutex<Vec<String>>>,
    /// Counter for rows that were dropped due to capacity limit.
    dropped_rows: usize,
    /// Counter for rows that were rejected due to disabled logging.
    rejected_rows: usize,
    /// Session start time, for timestamp computation.
    started_at: std::time::Instant,
    /// When false, write_row/flush are no-ops guaranteeing zero file I/O.
    enabled: bool,
    /// Session metadata (written as leading CSV columns)
    color_profile: &'static str,
    h264_profile: &'static str,
}

impl MetricsCsvLog {
    /// Creates a new RAM-buffered log.
    /// When METRICS_CSV_LOGGING is false, the returned log guarantees zero
    /// file I/O (write_row always returns false, flush is a no-op).
    pub(crate) fn new(color_profile: ColorProfile, h264_profile: H264Profile) -> Self {
        Self {
            rows: Arc::new(Mutex::new(Vec::new())),
            dropped_rows: 0,
            rejected_rows: 0,
            started_at: std::time::Instant::now(),
            enabled: METRICS_CSV_LOGGING,
            color_profile: color_profile.as_str(),
            h264_profile: h264_profile.as_str(),
        }
    }

    /// Write a metrics snapshot as a CSV row into the RAM buffer.
    /// Returns `true` if row was added, `false` if buffer was full or logging is disabled.
    /// When disabled, no row is added and zero file I/O occurs.
    pub(crate) fn write_row(&mut self, snapshot: &MetricsSnapshot, extras: &super::metrics_log::RowExtras) -> bool {
        if !self.enabled {
            return false;
        }
        let mut buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log row mutex poisoned"));
        if buffer.len() >= MAX_ROWS {
            self.dropped_rows += 1;
            return false;
        }
        let t_s = self.started_at.elapsed().as_secs();
        // Fixed-size array so the column count is compiler-enforced and can never
        // drift out of sync with HEADER. Order MUST match HEADER below.
        let cols: [u64; 66] = [
            t_s,
            snapshot.rtp_assembly_sum_us,
            snapshot.rtp_assembly_count,
            snapshot.rtp_assembly_max_us,
            snapshot.decode_us,
            snapshot.pipeline_age_us,
            snapshot.decoded,
            snapshot.skipped,
            snapshot.rate_limited,
            snapshot.presented,
            snapshot.replaced,
            snapshot.queue_full,
            snapshot.resyncs,
            snapshot.resets,
            snapshot.hw_buffered,
            snapshot.pipeline_age_max_us,
            snapshot.stream_lag_us,
            snapshot.stream_lag_max_us,
            snapshot.rtp_hold_saved,
            snapshot.rtp_hold_expired,
            snapshot.rtp_gaps,
            snapshot.rtp_abandons,
            snapshot.rtp_wait_drops,
            snapshot.rtp_invalid,
            snapshot.dec_queue_full,
            snapshot.flushes,
            snapshot.twcc_sent,
            snapshot.twcc_samples,
            snapshot.rtp_ext_any,
            snapshot.twcc_stride,
            snapshot.video_metadata_sent,
            snapshot.video_bandwidth_kbps,
            snapshot.display_pickup_us,
            snapshot.display_pickup_max_us,
            snapshot.present_us,
            snapshot.present_max_us,
            snapshot.stream_slip_us,
            snapshot.stream_slip_max_us,
            snapshot.audio_queue_ms,
            extras.src_fps,
            u64::from(extras.enc_width),
            u64::from(extras.enc_height),
            u64::from(extras.waiting_keyframe),
            extras.drops,
            extras.decode_errors,
            snapshot.vt_exit_count,
            snapshot.vt_max_0,
            snapshot.vt_max_1,
            snapshot.vt_max_2,
            snapshot.vt_max_3,
            snapshot.vt_max_4,
            snapshot.vt_max_5,
            snapshot.pli_submit,
            snapshot.pli_gap,
            snapshot.pli_expired,
            snapshot.pli_too_large,
            snapshot.pli_wait_idr,
            snapshot.pli_decode_err,
            snapshot.pli_watchdog,
            snapshot.pli_periodic,
            snapshot.pli_sent,
            snapshot.slip_reanchors,
            snapshot.qd_us,
            snapshot.qd_rel_us,
            snapshot.ext_ast_id,
            snapshot.ext_vt_id,
        ];
        let meta_prefix = format!(
            "{},{},{},{}",
            self.color_profile, self.h264_profile, GIT_SHA, APP_VERSION
        );
        let numeric_part = cols
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let row = format!("{meta_prefix},{numeric_part}");
        buffer.push(row);
        true
    }

    /// Flush the entire buffer to ux0:data/xcloud-rust/metrics.csv.
    /// Must be called once at session end (from RtcWorker::drop).
    /// Tolerant of path being unavailable (off-device: silently skip).
    /// When disabled, this is a no-op (zero file I/O).
    pub(crate) fn flush(&self) {
        if !self.enabled {
            return;
        }
        let buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log flush mutex poisoned"));
        let path = std::path::Path::new("ux0:data/xcloud-rust/metrics.csv");
        // Ensure directory exists (no-op if already exists on-device; off-device will fail)
        let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
        // Build the full CSV content once: header + all rows + footer
        let header = format!(
            "color_profile,h264_profile,git_sha,app_version,{header_orig}",
            header_orig = "t_s,rtp_assembly_sum_us,rtp_assembly_count,rtp_assembly_max_us,decode_us,pipeline_age_us,decoded,skipped,rate_limited,presented,replaced,queue_full,resyncs,resets,hw_buffered,pipeline_age_max_us,stream_lag_us,stream_lag_max_us,rtp_hold_saved,rtp_hold_expired,rtp_gaps,rtp_abandons,rtp_wait_drops,rtp_invalid,dec_queue_full,flushes,twcc_sent,twcc_samples,rtp_ext_any,twcc_stride,video_metadata_sent,video_bandwidth_kbps,display_pickup_us,display_pickup_max_us,present_us,present_max_us,stream_slip_us,stream_slip_max_us,audio_queue_ms,src_fps,enc_width,enc_height,waiting_keyframe,drops,decode_errors,vt_exit_count,vt_max_0,vt_max_1,vt_max_2,vt_max_3,vt_max_4,vt_max_5,pli_submit,pli_gap,pli_expired,pli_too_large,pli_wait_idr,pli_decode_err,pli_watchdog,pli_periodic,pli_sent,slip_reanchors,qd_us,qd_rel_us,ast_id,vt_id"
        );
        let rows_content: String = buffer
            .iter()
            .map(|row| format!("{row}\n"))
            .collect();
        let duration_s = self.started_at.elapsed().as_secs();
        let footer = format!(
            "// session_duration_s={}, total_rows={}, dropped_rows={}",
            duration_s,
            buffer.len(),
            self.dropped_rows
        );
        let content = format!("{header}\n{rows_content}{footer}\n");
        // Try writing the full content; on failure, fallback to OpenOptions then eprintln
        match std::fs::write(path, &content) {
            Ok(()) => {
                eprintln!("Metrics CSV flushed: {} rows + {} dropped", buffer.len(), self.dropped_rows);
            }
            Err(e1) => {
                // Fallback: try OpenOptions path
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(path)
                {
                    let _ = writeln!(file, "{header}");
                    for row in buffer.iter() {
                        let _ = writeln!(file, "{row}");
                    }
                    let _ = writeln!(file, "// session_duration_s={}, total_rows={}, dropped_rows={}", duration_s, buffer.len(), self.dropped_rows);
                    let _ = file.sync_data();
                    eprintln!("Metrics CSV flushed: {} rows + {} dropped (via fallback)", buffer.len(), self.dropped_rows);
                } else {
                    eprintln!("Metrics CSV: path unavailable, skipping flush (off-device build); write error: {e1}");
                }
            }
        }
    }
}

// end of metrics_log.rs