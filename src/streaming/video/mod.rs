mod decoder;
pub(crate) mod memory;
pub(crate) mod metrics;
mod worker;

pub const STREAM_WIDTH: u32 = 1280;
pub const STREAM_HEIGHT: u32 = 720;
pub const HW_OUTPUT_WIDTH: u32 = 960;
pub const HW_OUTPUT_HEIGHT: u32 = 544;
pub(crate) const DEFAULT_VIDEO_FPS: u32 = 30;
pub(crate) const UNLOCKED_VIDEO_FPS: u32 = 60;
pub(crate) const NUM_TEXTURES: usize = 3;

pub use memory::reserve_decoder_cdram;
pub use metrics::video_performance_summary;
pub use worker::VideoDecodeWorker;

use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// With double buffering (2 textures), the decoder writes into one texture while
// the renderer displays the other.  It must wait up to one VSYNC interval for the
// renderer to finish with the pending texture.  16.7ms matches 60 Hz VSYNC.
const MAX_PENDING_TEXTURE_WAIT: Duration = Duration::from_micros(16_700);

#[derive(Clone, Copy)]
pub(crate) struct VideoTextureTarget {
    pub(crate) ptr: usize,
    pub(crate) pitch: u32,
    pub(crate) capacity: u32,
}

struct DirectVideoOutputState {
    targets: Option<[VideoTextureTarget; NUM_TEXTURES]>,
    displayed: Option<usize>,
    last_displayed: Option<usize>,
    pending: Option<(usize, u64)>,
    next_generation: u64,
}

/// Synchronizes the decoder thread with the two SDL/GXM textures owned by the render thread.
/// Pointers are stored as integers so the platform-specific unsafe boundary stays in the code
/// that registers and consumes the textures.
pub(crate) struct DirectVideoOutput {
    state: Mutex<DirectVideoOutputState>,
    frame_displayed: Condvar,
    pub(crate) decoder_ready: AtomicBool,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl DirectVideoOutput {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            state: Mutex::new(DirectVideoOutputState {
                targets: None,
                displayed: None,
                last_displayed: None,
                pending: None,
                next_generation: 0,
            }),
            frame_displayed: Condvar::new(),
            decoder_ready: AtomicBool::new(false),
            width,
            height,
        }
    }

    pub(crate) fn set_targets(&self, targets: [VideoTextureTarget; NUM_TEXTURES]) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = Some(targets);
            state.displayed = None;
            state.last_displayed = None;
            state.pending = None;
        }
    }

    pub(crate) fn clear_targets(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.targets = None;
            state.displayed = None;
            state.pending = None;
        }
        self.frame_displayed.notify_all();
    }

    pub(crate) fn mark_displayed(&self, index: usize, generation: u64) {
        let mut cleared_pending = false;
        if let Ok(mut state) = self.state.lock() {
            state.last_displayed = state.displayed;
            state.displayed = Some(index);
            if state.pending == Some((index, generation)) {
                state.pending = None;
                cleared_pending = true;
            }
        }
        if cleared_pending {
            self.frame_displayed.notify_one();
        }
    }

    /// Releases a decoded frame intentionally omitted by the presentation-rate limiter.
    /// The currently displayed texture remains unchanged, so the decoder cannot overwrite it.
    pub(crate) fn discard_pending(&self, index: usize, generation: u64) {
        let mut discarded = false;
        if let Ok(mut state) = self.state.lock()
            && state.pending == Some((index, generation))
        {
            state.pending = None;
            discarded = true;
        }
        if discarded {
            self.frame_displayed.notify_one();
        }
    }

    pub(super) fn lock_decode_target(&self) -> Option<DirectVideoTargetGuard<'_>> {
        let mut state = self.state.lock().ok()?;
        let targets = state.targets?;

        // Fast path: find a texture that is neither displayed nor pending.
        // Also skip the most-recently-displayed texture: the GXM GPU may still
        // be DMA-reading it from the previous VSYNC cycle.  Waiting one full
        // display interval before recycling that slot prevents texture-race
        // artifacts that appear as blocky corruption.
        let free = (0..NUM_TEXTURES).find(|&i| {
            state.displayed != Some(i)
                && state.last_displayed != Some(i)
                && state.pending.as_ref().map(|(idx, _)| *idx) != Some(i)
        });

        let index = if let Some(i) = free {
            i
        } else {
            // All NUM_TEXTURES are busy (should be impossible with > 2).
            // Fall through to the old condvar wait path.
            let (waited, _) = self
                .frame_displayed
                .wait_timeout_while(state, MAX_PENDING_TEXTURE_WAIT, |s| {
                    s.targets.is_some() && s.pending.is_some()
                })
                .ok()?;
            state = waited;
            (0..NUM_TEXTURES).find(|&i| {
                state.displayed != Some(i)
                    && state.pending.as_ref().map(|(idx, _)| *idx) != Some(i)
            })?
        };

        Some(DirectVideoTargetGuard {
            state,
            target: targets[index],
            index,
        })
    }
}

pub(super) struct DirectVideoTargetGuard<'a> {
    state: MutexGuard<'a, DirectVideoOutputState>,
    target: VideoTextureTarget,
    index: usize,
}

impl DirectVideoTargetGuard<'_> {
    pub(super) fn publish(mut self) -> (usize, u64) {
        self.state.next_generation = self.state.next_generation.wrapping_add(1);
        let generation = self.state.next_generation;
        self.state.pending = Some((self.index, generation));
        (self.index, generation)
    }
}

pub struct DecodedFrame {
    pub texture_index: usize,
    pub generation: u64,
    pub published_at: Instant,
}

#[derive(Clone, Copy)]
pub struct DecoderConfig {
    pub decode_width: u32,
    pub decode_height: u32,
    pub output_width: u32,
    pub output_height: u32,
}
