//! Append-only CSV logging of the once-per-second metrics snapshot, so a
//! streaming session can be analysed offline (fetched over the Vita FTP server
//! from `ux0:/data/xcloud-rust/metrics.csv`).
//!
//! All per-row file I/O happens on a dedicated writer thread: `write_row` runs on
//! the latency-critical RTC pump thread and must never block on the memory card.

use super::metrics::MetricsSnapshot;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

const LOG_DIR: &str = "ux0:data/xcloud-rust";
const LOG_PATH: &str = "ux0:data/xcloud-rust/metrics.csv";
/// Safety valve: stop appending after this many rows (~5.5 hours at 1 Hz).
const MAX_ROWS: u64 = 20_000;

/// Extra per-tick values that live on the video receiver rather than in METRICS.
pub(crate) struct RowExtras {
    pub(crate) src_fps: u64,
    pub(crate) enc_width: u32,
    pub(crate) enc_height: u32,
    pub(crate) waiting_keyframe: bool,
    pub(crate) drops: u64,
    pub(crate) decode_errors: u64,
}

const HEADER: &str = "t_s,rtp_assembly_sum_us,rtp_assembly_count,rtp_assembly_max_us,asm_avg_us,decode_us,pipeline_age_us,decoded,skipped,rate_limited,presented,replaced,queue_full,resyncs,resets,hw_buffered,pipeline_age_max_us,stream_lag_us,stream_lag_max_us,rtp_hold_saved,rtp_hold_expired,rtp_gaps,rtp_abandons,rtp_wait_drops,rtp_invalid,dec_queue_full,flushes,twcc_sent,twcc_samples,rtp_ext_any,twcc_stride,video_metadata_sent,video_bandwidth_kbps,display_pickup_us,display_pickup_max_us,rtcp_rr_loss_fraction,rtcp_rr_jitter,rtcp_sr_ntp_timestamp,capture_to_arrival_us,capture_to_arrival_max_us,src_fps,enc_width,enc_height,waiting_keyframe,drops,decode_errors,rtcp_nack_sent,rtx_packets_received,rtx_packets_seen";

pub(crate) struct MetricsLog {
    /// Rows are formatted on the RTC pump thread (cheap) and handed to the
    /// writer thread, which owns the file.  The channel is unbounded so `send`
    /// never blocks; the producer is capped at 1 row/s.
    rows_tx: Sender<String>,
    started_at: Instant,
    rows: u64,
}

impl MetricsLog {
    /// Creates (truncating) the log, writes the header, and starts the writer
    /// thread.  Returns `None` when the path is unavailable (e.g. running
    /// off-device), so logging is optional.
    ///
    /// The open/header writes happen once per session on the calling thread; all
    /// subsequent writes are handed to the writer thread.
    pub(crate) fn open() -> Option<Self> {
        if fs::create_dir_all(LOG_DIR).is_err() {
            return None;
        }
        let file = File::create(LOG_PATH).ok()?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{HEADER}").ok()?;
        writer.flush().ok()?;

        let (rows_tx, rows_rx) = mpsc::channel::<String>();
        thread::Builder::new()
            .name("green-vita-metrics-log".to_owned())
            .spawn(move || {
                while let Ok(row) = rows_rx.recv() {
                    if writeln!(writer, "{row}").is_err() {
                        break;
                    }
                    if writer.flush().is_err() {
                        break;
                    }
                }
                let _ = writer.flush();
            })
            .ok()?;

        eprintln!("Metrics log: writing {LOG_PATH} (writer thread)");
        Some(Self {
            rows_tx,
            started_at: Instant::now(),
            rows: 0,
        })
    }

    /// Formats one row and queues it for the writer thread.  Does no file I/O,
    /// so it is safe to call from the RTC pump thread.
    pub(crate) fn write_row(&mut self, s: &MetricsSnapshot, e: &RowExtras) {
        if self.rows >= MAX_ROWS {
            return;
        }
        self.rows += 1;
        let t_s = self.started_at.elapsed().as_secs();

        // Fixed-size array so the column count is compiler-enforced and can never
        // drift out of sync with HEADER (49 columns).  Order MUST match HEADER.
        let cols: [u64; 49] = [
            t_s,
            s.rtp_assembly_sum_us,
            s.rtp_assembly_count,
            s.rtp_assembly_max_us,
            s.asm_avg_us,
            s.decode_us,
            s.pipeline_age_us,
            s.decoded,
            s.skipped,
            s.rate_limited,
            s.presented,
            s.replaced,
            s.queue_full,
            s.resyncs,
            s.resets,
            s.hw_buffered,
            s.pipeline_age_max_us,
            s.stream_lag_us,
            s.stream_lag_max_us,
            s.rtp_hold_saved,
            s.rtp_hold_expired,
            s.rtp_gaps,
            s.rtp_abandons,
            s.rtp_wait_drops,
            s.rtp_invalid,
            s.dec_queue_full,
            s.flushes,
            s.twcc_sent,
            s.twcc_samples,
            s.rtp_ext_any,
            s.twcc_stride,
            s.video_metadata_sent,
            s.video_bandwidth_kbps,
            s.display_pickup_us,
            s.display_pickup_max_us,
            s.rtcp_rr_loss_fraction,
            s.rtcp_rr_jitter,
            s.rtcp_sr_ntp_timestamp,
            s.capture_to_arrival_us,
            s.capture_to_arrival_max_us,
            e.src_fps,
            u64::from(e.enc_width),
            u64::from(e.enc_height),
            u64::from(e.waiting_keyframe),
            e.drops,
            e.decode_errors,
            s.rtcp_nack_sent,
            s.rtx_packets_received,
            s.rtx_packets_seen,
        ];
        let row = cols
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        // Unbounded channel: never blocks the pump thread.  If the writer thread
        // has died the send fails and the row is simply dropped.
        let _ = self.rows_tx.send(row);
    }
}