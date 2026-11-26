//! Direct-present experiment (2026-11-26).
//!
//! The Vita panel latches the framebuffer at the vblank, so an off-vblank
//! `SDL_RenderPresent` is still scanned out at the next vblank. This module
//! writes the drawn frame into the *current* framebuffer and sets it with
//! `SCE_DISPLAY_SETBUF_IMMEDIATE`, which scans it out immediately (tearing).
//! If the panel latches anyway, the latency does not move and the experiment
//! is a no-op; if it scans continuously, the vblank phase (~10-13 ms) is
//! recovered.
//!
//! This first version reads the screen back through SDL (slow, ~1 MB/frame)
//! to prove the mechanism. If the win is real, the readback is replaced by
//! writing the decoded frame straight into the framebuffer.
//!
//! Safety: every mismatch (format, size, readback error) falls back to the
//! normal SDL present, so a failure can never leave a black screen.

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};

/// Compile-time switch for the experiment. Set to `false` to restore the
/// normal SDL present path without touching the call sites.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

#[cfg(target_os = "vita")]
mod imp {
    use super::{ENABLED, Ordering};
    use anyhow::Result;
    use sdl2::pixels::PixelFormatEnum;

    const SCE_DISPLAY_SETBUF_IMMEDIATE: i32 = 0;
    const SCE_DISPLAY_FRAMEBUF_FORMAT_B5G6R5: u32 = 1;

    #[repr(C)]
    struct SceDisplayFrameBuf {
        size: usize,
        base: *mut std::ffi::c_void,
        pitch: u32,
        pixelformat: u32,
        width: u32,
        height: u32,
    }

    unsafe extern "C" {
        fn sceDisplayGetFrameBuf(p: *mut SceDisplayFrameBuf, sync: i32) -> i32;
        fn sceDisplaySetFrameBuf(p: *const SceDisplayFrameBuf, sync: i32) -> i32;
    }

    /// Present the already-drawn screen immediately. Returns `true` when the
    /// direct path was used, `false` when the caller must fall back to
    /// `canvas.present()`.
    pub fn present_immediate(
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        width: u32,
        height: u32,
    ) -> Result<bool> {
        if !ENABLED.load(Ordering::Relaxed) {
            return Ok(false);
        }

        let mut fb = SceDisplayFrameBuf {
            size: std::mem::size_of::<SceDisplayFrameBuf>(),
            base: std::ptr::null_mut(),
            pitch: 0,
            pixelformat: 0,
            width: 0,
            height: 0,
        };
        let got = unsafe { sceDisplayGetFrameBuf(&mut fb, SCE_DISPLAY_SETBUF_IMMEDIATE) };
        if got != 0
            || fb.base.is_null()
            || fb.pixelformat != SCE_DISPLAY_FRAMEBUF_FORMAT_B5G6R5
            || fb.width != width
            || fb.height != height
        {
            return Ok(false);
        }

        // Read the drawn screen back as RGB565 and copy it into the framebuffer.
        let pixels = canvas
            .read_pixels(None, PixelFormatEnum::BGR565)
            .map_err(anyhow::Error::msg)?;

        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), fb.base as *mut u8, pixels.len());
            sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_IMMEDIATE);
        }
        Ok(true)
    }
}

#[cfg(not(target_os = "vita"))]
mod imp {
    use anyhow::Result;

    pub fn present_immediate(
        _canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        _width: u32,
        _height: u32,
    ) -> Result<bool> {
        Ok(false)
    }
}

pub use imp::present_immediate;