use super::decoder::HwVideoDecoder;
use super::metrics;
use super::{DecodedFrame, DecoderConfig, DirectVideoOutput};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select_biased, unbounded};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Keep only one compressed frame queued at the default 30 FPS to minimize glass-to-glass
// latency. Faster streams may still use a larger burst buffer.
const MIN_PENDING_ACCESS_UNITS: usize = 1;
/// Target decoder pace range: 14-20ms (50-71 fps). Wider than the console's
/// typical 58-62 fps so brief excursions don't clip.
const MIN_DECODE_INTERVAL_US: u64 = 14_000;
const MAX_DECODE_INTERVAL_US: u64 = 20_000;
const DEFAULT_DECODE_INTERVAL_US: u64 = 16_667;

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
    decode_queue_depth: usize,
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
        let queue_depth = config.decode_queue_depth;
        let (access_units, worker_access_units) = bounded(queue_depth);
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
            decode_queue_depth: queue_depth,
        })
    }

    pub fn submit_access_unit(&self, data: Vec<u8>, source_frame_duration_us: Option<u64>) -> bool {
        if let Some(duration) = source_frame_duration_us
            .filter(|d| (MIN_DECODE_INTERVAL_US..=MAX_DECODE_INTERVAL_US).contains(d))
        {
            self.last_source_duration_us.store(duration, Ordering::Relaxed);
        }
        let source_fps = source_frame_duration_us
            .filter(|duration| *duration > 0)
            .map(|duration| 1_000_000 / duration)
            .unwrap_or(30);
        let extra_capacity = source_fps
            .saturating_sub(30)
            .min(25)
            .saturating_mul((self.decode_queue_depth - MIN_PENDING_ACCESS_UNITS) as u64)
            .saturating_add(12)
            / 25;
        let pending_limit = MIN_PENDING_ACCESS_UNITS + extra_capacity as usize;
        if self.access_units.len() >= pending_limit {
            metrics::METRICS.queue_full.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let access_unit = QueuedAccessUnit {
            data,
            queued_at: Instant::now(),
            generation: self.generation.load(Ordering::Acquire),
        };
        match self.access_units.try_send(access_unit) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
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
    use std::thread::sleep;
    // Pace the decoder to keep up with the console's encoder rate.
    // This prevents the access-unit queue from filling.
    let pace_sleep = Duration::from_millis(config.decode_sleep_ms as u64);

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
                sleep(pace_sleep);
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
