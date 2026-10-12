//! Append-only CSV logging of the once-per-second metrics snapshot, so a
//! streaming session can be analysed offline (fetched over the Vita FTP server
//! from `ux0:/data/xcloud-rust/metrics.csv`).

use super::metrics::MetricsSnapshot;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
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

const HEADER: &str = "t_s,rtp_assembly_sum_us,rtp_assembly_count,rtp_assembly_max_us,asm_avg_us,decode_us,pipeline_age_us,decoded,skipped,rate_limited,presented,replaced,queue_full,resyncs,resets,hw_buffered,pipeline_age_max_us,stream_lag_us,stream_lag_max_us,rtp_hold_saved,rtp_hold_expired,rtp_gaps,rtp_abandons,rtp_wait_drops,rtp_invalid,dec_queue_full,flushes,twcc_sent,twcc_samples,rtp_ext_any,twcc_stride,video_metadata_sent,video_bandwidth_kbps,display_pickup_us,display_pickup_max_us,rtcp_rr_loss_fraction,rtcp_rr_jitter,rtcp_sr_ntp_timestamp,src_fps,enc_width,enc_height,waiting_keyframe,drops,decode_errors";

pub(crate) struct MetricsLog {
    writer: BufWriter<File>,
    started_at: Instant,
    rows: u64,
}

impl MetricsLog {
    /// Creates (truncating) the log and writes the header.  Returns `None` when
    /// the path is unavailable (e.g. running off-device), so logging is optional.
    pub(crate) fn open() -> Option<Self> {
        if fs::create_dir_all(LOG_DIR).is_err() {
            return None;
        }
        let file = File::create(LOG_PATH).ok()?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{HEADER}").ok()?;
        writer.flush().ok()?;
        eprintln!("Metrics log: writing {LOG_PATH}");
        Some(Self {
            writer,
            started_at: Instant::now(),
            rows: 0,
        })
    }

    pub(crate) fn write_row(&mut self, s: &MetricsSnapshot, e: &RowExtras) {
        if self.rows >= MAX_ROWS {
            return;
        }
        self.rows += 1;
        let t_s = self.started_at.elapsed().as_secs();
        // Fixed-size array so the column count is compiler-enforced and can never
        // drift out of sync with HEADER (44 columns).  Order MUST match HEADER.
        let cols: [u64; 44] = [
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
            e.src_fps,
            u64::from(e.enc_width),
            u64::from(e.enc_height),
            u64::from(e.waiting_keyframe),
            e.drops,
            e.decode_errors,
        ];
        let row = cols
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = writeln!(self.writer, "{row}");
        let _ = self.writer.flush();
    }
}