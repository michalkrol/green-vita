//! Append-only CSV logging of the once-per-second metrics snapshot, so a
//! streaming session can be analysed offline.
//! 
//! This version buffers rows in RAM and flushes once at session end,
//! avoiding filesystem I/O during the hot path.
//!
//! CSV is written to ux0:data/xcloud-rust/metrics.csv at session teardown.
//! During the session, zero touches ux0:.

use super::metrics::{MetricsSnapshot, drain_metrics, reset_session_metrics};
use std::io::Write;
use std::sync::{Arc, Mutex};



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
    /// Session start time, for timestamp computation.
    started_at: std::time::Instant,
}

impl MetricsCsvLog {
    /// Creates a new RAM-buffered log.
    pub(crate) fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(Vec::new())),
            dropped_rows: 0,
            started_at: std::time::Instant::now(),
        }
    }

    /// Write a metrics snapshot as a CSV row into the RAM buffer.
    /// Returns `true` if row was added, `false` if buffer was full and row was dropped.
    pub(crate) fn write_row(&mut self, snapshot: &MetricsSnapshot, extras: &super::metrics_log::RowExtras) -> bool {
        let mut buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log row mutex poisoned"));
        if buffer.len() >= MAX_ROWS {
            self.dropped_rows += 1;
            return false;
        }
        let t_s = self.started_at.elapsed().as_secs();
        // Fixed-size array so the column count is compiler-enforced and can never
        // drift out of sync with HEADER. Order MUST match HEADER below.
        let cols: [u64; 50] = [
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
            // New M0 fields
            snapshot.stream_slip_us,
            snapshot.stream_slip_max_us,
            snapshot.audio_queue_ms,
            // vt census (extmap id=7)
            snapshot.vt_exit_count,
            snapshot.vt_max_0,
            snapshot.vt_max_1,
            snapshot.vt_max_2,
            snapshot.vt_max_3,
            snapshot.vt_max_4,
            snapshot.vt_max_5,
            // Receiver-level values from extras
            extras.src_fps,
            u64::from(extras.enc_width),
            u64::from(extras.enc_height),
            u64::from(extras.waiting_keyframe),
            extras.drops,
            extras.decode_errors,
        ];
        let row = cols
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        buffer.push(row);
        true
    }

    /// Flush the entire buffer to ux0:data/xcloud-rust/metrics.csv.
    /// Must be called once at session end (from RtcWorker::drop).
    /// Truncates the file, writes header, all rows, and a footer line.
    /// Tolerant of path being unavailable (off-device: silently skip).
    pub(crate) fn flush(&self) {
        let buffer = self.rows.lock().unwrap_or_else(|_| panic!("metrics_csv_log flush mutex poisoned"));
        let path = std::path::Path::new("ux0:data/xcloud-rust/metrics.csv");
        // Ensure directory exists (no-op if already exists on-device; off-device will fail)
        let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
        // Use OpenOptions for flexibility: truncate + write, create if needed
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
        {
            // Write header
            let header = "t_s,rtp_assembly_sum_us,rtp_assembly_count,rtp_assembly_max_us,decode_us,pipeline_age_us,decoded,skipped,rate_limited,presented,replaced,queue_full,resyncs,resets,hw_buffered,pipeline_age_max_us,stream_lag_us,stream_lag_max_us,rtp_hold_saved,rtp_hold_expired,rtp_gaps,rtp_abandons,rtp_wait_drops,rtp_invalid,dec_queue_full,flushes,twcc_sent,twcc_samples,rtp_ext_any,twcc_stride,video_metadata_sent,video_bandwidth_kbps,display_pickup_us,display_pickup_max_us,stream_slip_us,stream_slip_max_us,audio_queue_ms,src_fps,enc_width,enc_height,waiting_keyframe,drops,decode_errors,vt_exit_count,vt_max_0,vt_max_1,vt_max_2,vt_max_3,vt_max_4,vt_max_5";
            let _ = writeln!(file, "{header}");

            // Write all accumulated rows
            for row in buffer.iter() {
                let _ = writeln!(file, "{row}");
            }

            // Write footer with session metadata
            let duration_s = self.started_at.elapsed().as_secs();
            let footer = format!(
                "// session_duration_s={}, total_rows={}, dropped_rows={}",
                duration_s,
                buffer.len(),
                self.dropped_rows
            );
            let _ = writeln!(file, "{footer}");

            let _ = file.sync_data();
            eprintln!("Metrics CSV flushed: {} rows + {} dropped", buffer.len(), self.dropped_rows);
        } else {
            // Path unavailable (off-device): silently skip, log once
            eprintln!("Metrics CSV: path unavailable, skipping flush (off-device build)");
        }
    }
}

