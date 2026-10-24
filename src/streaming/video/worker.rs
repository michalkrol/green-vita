use super::decoder::HwVideoDecoder;
use super::metrics;
use super::{DecodedFrame, DecoderConfig, DirectVideoOutput};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased, unbounded};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Maximum number of access units queued before rejecting new ones.
const AU_QUEUE_MAX: usize = 8;
/// Maximum age (in ms) an access unit can remain in the queue before being rejected.
const AU_QUEUE_MAX_AGE: u64 = 150;
/// Default decode interval in microseconds (≈60fps).
const DEFAULT_DECODE_INTERVAL_US: u64 = 16_667;
/// Minimum valid source frame duration in microseconds.
const MIN_DECODE_INTERVAL_US: u64 = 14_000;
/// Maximum valid source frame duration in microseconds.
const MAX_DECODE_INTERVAL_US: u64 = 20_000;

struct QueuedAccessUnit {
    data: Vec<u8>,
    queued_at: Instant,
    generation: u64,
}

enum DecoderCommand {
    Reset,
    Stop,
}

pub(crate) type DecodeResult = Result<DecodedFrame, String>;

pub struct VideoDecodeWorker {
    access_units: Sender<QueuedAccessUnit>,
    commands: Sender<DecoderCommand>,
    generation: Arc<AtomicU64>,
    pub(crate) latest_result: Arc<Mutex<Option<DecodeResult>>>,
    pub(crate) result_ready: Arc<tokio::sync::Notify>,
    /// Updated from RTP timestamps to pace the decoder to the console's encoder rate.
    pub(crate) last_source_duration_us: Arc<AtomicU64>,
    /// Marker for the oldest access unit in the queue, used for age-bound rejection.
    /// Set when the queue is empty (pre-push len==0); compared against when queue has AUs.
    oldest_au_marker: std::sync::Mutex<Option<Instant>>,
}

impl VideoDecodeWorker {
    pub fn spawn(config: DecoderConfig, direct_output: Arc<DirectVideoOutput>) -> Result<Self> {
        // Session teardown is asynchronous: the previous session's RTC thread must observe
        // channel disconnect, drop its VideoDecodeWorker, and let the decode thread run
        // sceVideodecTermLibrary before a fresh sceVideodecInitLibrary can succeed. A
        // frozen/wedged previous session widens that window, so retry instead of failing
        // the whole session with 0x80620808-class errors.
        const DECODER_INIT_ATTEMPTS: usize = 20;
        const DECODER_INIT_RETRY_DELAY: Duration = Duration::from_millis(250);
        let mut decoder = None;
        for attempt in 1..=DECODER_INIT_ATTEMPTS {
            match HwVideoDecoder::new(config) {
                Ok(created) => {
                    decoder = Some(created);
                    break;
                }
                Err(error) if attempt < DECODER_INIT_ATTEMPTS => {
                    eprintln!(
                        "H264 decoder init failed (attempt {attempt}/{DECODER_INIT_ATTEMPTS}): {error:#}; retrying"
                    );
                    std::thread::sleep(DECODER_INIT_RETRY_DELAY);
                }
                Err(error) => {
                    return Err(error).context("failed to create hardware H264 decoder");
                }
            }
        }
        let decoder = decoder.expect("decoder created after retry loop");
        direct_output.decoder_ready.store(true, Ordering::Release);
        let (access_units, worker_access_units) = bounded(AU_QUEUE_MAX);
        let (commands, worker_commands) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let latest_result = Arc::new(Mutex::new(None));
        let worker_latest_result = Arc::clone(&latest_result);
        let result_ready = Arc::new(tokio::sync::Notify::new());
        let worker_result_ready = Arc::clone(&result_ready);
        let worker_direct_output = Arc::clone(&direct_output);
        let source_duration = Arc::new(AtomicU64::new(DEFAULT_DECODE_INTERVAL_US));
        let worker_source_duration = Arc::clone(&source_duration);

        std::thread::Builder::new()
            .name("green-vita-video-decode".to_owned())
            .spawn(move || {
                #[cfg(target_os = "vita")]
                pin_decoder_thread();
                run_decode_loop(
                    worker_access_units,
                    worker_commands,
                    worker_generation,
                    worker_latest_result,
                    worker_result_ready,
                    decoder,
                    config,
                    worker_direct_output,
                    worker_source_duration,
                )
            })
            .context("failed to spawn video decode worker")?;

        Ok(Self {
            access_units,
            commands,
            generation,
            latest_result,
            result_ready,
            last_source_duration_us: source_duration,
            oldest_au_marker: std::sync::Mutex::new(None),
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool {
        if let Some(duration) = source_frame_duration_us
            .filter(|d| (MIN_DECODE_INTERVAL_US..=MAX_DECODE_INTERVAL_US).contains(d))
        {
            self.last_source_duration_us.store(duration, Ordering::Relaxed);
        }
        let access_unit = QueuedAccessUnit {
            data,
            queued_at: Instant::now(),
            generation: self.generation.load(Ordering::Acquire),
        };

        // Single-thread marker: track oldest AU age for age-bound rejection.
        // Pre-push len==0 → record Instant (this AU becomes oldest).
        // len>0 → compare age vs marker; reset marker when queue observed empty.
        let was_empty = self.access_units.len() == 0;

        // Adaptive AU queue depth limit (v0.3.0 recipe).
        // When ADAPTIVE_AU_LIMIT is true, compute pending_limit from source fps:
        //   pending_limit = 1 + (max(0, source_fps - 30).min(25) * (8 - 1) + 12) / 25
        // capped to 1..=8. When false, use fixed AU_QUEUE_MAX (8).
        let pending_limit = if crate::flags::ADAPTIVE_AU_LIMIT {
            // Original v0.3.0 adaptive range (from 21f0c51^): MIN=1, MAX=6.
            const MIN_PENDING: usize = 1;
            const MAX_PENDING: usize = 6;
            let source_fps = source_frame_duration_us
                .filter(|d| *d > 0)
                .map(|d| 1_000_000 / d)
                .unwrap_or(30);
            let extra_capacity = source_fps
                .saturating_sub(30)
                .min(25)
                .saturating_mul((MAX_PENDING - MIN_PENDING) as u64)
                .saturating_add(12)
                / 25;
            (MIN_PENDING + extra_capacity as usize).clamp(MIN_PENDING, MAX_PENDING)
        } else {
            AU_QUEUE_MAX
        };

        if self.access_units.len() >= pending_limit {
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Check age bound: if oldest AU in queue exceeds AU_QUEUE_MAX_AGE_MS, reject.
        // When the flag is 0 (disabled), the age check never rejects — marker bookkeeping
        // elsewhere may remain untouched. When >0, use the flag value as the threshold.
        let age_exceeded = {
            let marker_guard = self.oldest_au_marker.lock().unwrap_or_else(|e| e.into_inner());
            match *marker_guard {
                Some(marker) => {
                    let age = marker.elapsed().as_millis() as u64;
                    if crate::flags::AU_QUEUE_MAX_AGE_MS == 0 {
                        false
                    } else {
                        age > crate::flags::AU_QUEUE_MAX_AGE_MS
                    }
                }
                None => false,
            }
        };

        if age_exceeded {
            metrics::METRICS.age_drops.fetch_add(1, Ordering::Relaxed);
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            *self.oldest_au_marker.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return false;
        }

        // If queue was empty, this AU becomes the marker.
        if was_empty {
            *self.oldest_au_marker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }

        match self.access_units.try_send(access_unit) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                // Queue filled after our len check - treat as reject
                metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn reset_decoder(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resets.fetch_add(1, Ordering::Relaxed);
        let _ = self.commands.send(DecoderCommand::Reset);
    }

    pub fn begin_resync(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        metrics::METRICS.resyncs.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for VideoDecodeWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(DecoderCommand::Stop);
    }
}

#[cfg(target_os = "vita")]
fn pin_decoder_thread() {
    let thread_id = unsafe { vitasdk_sys::sceKernelGetThreadId() };
    let result = unsafe {
        vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            thread_id,
            vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_2 as i32,
        )
    };
    if result < 0 {
        eprintln!("Failed to pin video decoder thread to user CPU 2: {result:#x}");
    }
}

fn run_decode_loop(
    access_units: Receiver<QueuedAccessUnit>,
    commands: Receiver<DecoderCommand>,
    generation: Arc<AtomicU64>,
    latest_result: Arc<Mutex<Option<DecodeResult>>>,
    result_ready: Arc<tokio::sync::Notify>,
    initial_decoder: HwVideoDecoder,
    config: DecoderConfig,
    direct_output: Arc<DirectVideoOutput>,
    _source_duration: Arc<AtomicU64>,
) {
    let mut decoder = Some(initial_decoder);

    loop {
        select_biased! {
            recv(commands) -> command => match command {
                Ok(DecoderCommand::Reset) => {
                    decoder = None;
                    continue;
                }
                Ok(DecoderCommand::Stop) | Err(_) => break,
            },
            recv(access_units) -> access_unit => {
                let Ok(access_unit) = access_unit else { break };
                decode_queued_access_unit(
                    &mut decoder,
                    config,
                    &generation,
                    &latest_result,
                    &result_ready,
                    access_unit,
                    &direct_output,
                );
                // v0.3.0 decode-pace sleep: throttle the decode loop to prevent AU queue from filling.
                // When DECODE_PACE_SLEEP_MS > 0, sleep after each iteration restoring the paced
                // decode cadence that kept the AU queue shedding at ~1/s keyframes.
                // 0 = full-speed decode (post-21f0c51 behavior).
                if crate::flags::DECODE_PACE_SLEEP_MS > 0 {
                    std::thread::sleep(Duration::from_millis(crate::flags::DECODE_PACE_SLEEP_MS));
                }
            }
        }
    }
}

fn decode_queued_access_unit(
    decoder: &mut Option<HwVideoDecoder>,
    config: DecoderConfig,
    generation: &AtomicU64,
    latest_result: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    access_unit: QueuedAccessUnit,
    direct_output: &DirectVideoOutput,
) {
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    if decoder.is_none() {
        match HwVideoDecoder::new(config) {
            Ok(new_decoder) => *decoder = Some(new_decoder),
            Err(error) => {
                publish_result(
                    latest_result,
                    result_ready,
                    Err(format!("failed to recreate H264 decoder: {error:#}")),
                );
                return;
            }
        }
    }

    let Some(direct_target) = direct_output.lock_decode_target() else {
        // Do not decode until the renderer has registered its two GXM textures. There is no
        // legacy output buffer to copy from anymore.
        metrics::METRICS.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    // Measure the hardware call and contain an unexpected decoder panic inside its worker.
    let decode_started_at = Instant::now();
    let decode_result = catch_unwind(AssertUnwindSafe(|| {
        decoder
            .as_mut()
            .expect("decoder recreated above")
            .decode(&access_unit.data, direct_target.target)
    }));
    metrics::METRICS.decode_us.store(
        decode_started_at.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    if access_unit.generation != generation.load(Ordering::Acquire) {
        return;
    }

    match decode_result {
        Ok(Ok(true)) => {
            metrics::METRICS.decoded.fetch_add(1, Ordering::Relaxed);
            let (texture_index, generation) = direct_target.publish();
            let pipeline_age_us = access_unit.queued_at.elapsed().as_micros() as u64;
            metrics::METRICS
                .pipeline_age_us
                .store(pipeline_age_us, Ordering::Relaxed);
            metrics::METRICS
                .pipeline_age_max_us
                .fetch_max(pipeline_age_us, Ordering::Relaxed);
            publish_result(
                latest_result,
                result_ready,
                Ok(DecodedFrame {
                    texture_index,
                    generation,
                    published_at: Instant::now(),
                }),
            );
        }
        Ok(Ok(false)) => {
            metrics::METRICS.hw_buffered.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Err(error)) => {
            *decoder = None;
            publish_result(latest_result, result_ready, Err(error.to_string()));
        }
        Err(_) => {
            eprintln!("H264 decoder panicked; recreating decoder on next frame");
            *decoder = None;
            publish_result(
                latest_result,
                result_ready,
                Err("H264 decoder panicked and was restarted".to_owned()),
            );
        }
    }
}

fn publish_result(
    slot: &Mutex<Option<DecodeResult>>,
    result_ready: &tokio::sync::Notify,
    result: DecodeResult,
) {
    if let Ok(mut latest) = slot.lock()
        && latest.replace(result).is_some()
    {
        metrics::METRICS.replaced.fetch_add(1, Ordering::Relaxed);
    }
    result_ready.notify_one();
}