//! Direct-present experiment (2026-11-26).
//!
//! The Vita panel latches the framebuffer at the vblank, so an off-vblank
//! `SDL_RenderPresent` is still scanned out at the next vblank. This module
//! writes the drawn frame into the *current* framebuffer and sets it with
//! `SCE_DISPLAY_SETBUF_IMMEDIATE`, which scans it out immediately (tearing).
//!
//! This first version reads the screen back through SDL (slow, ~1 MB/frame)
//! to prove the mechanism. If the win is real, the readback is replaced by
//! writing the decoded frame straight into the framebuffer.
//!
//! Diagnostics: every call increments OK or FALLBACK (with a reason), and a
//! small report is written to `ux0:data/xcloud-rust/direct-present.txt` every
//! 5 s so the FTP pull confirms the direct path is actually firing. Set
//! `STRICT` to true to disable the fallback (a fallback then skips present,
//! leaving the last frame on screen -- a deliberate, visible signal).

use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Master switch for the experiment.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

/// When true, a fallback does NOT present through SDL: the caller is told the
/// frame was "presented" so the last image stays on screen. Use it to prove the
/// direct path is active (the screen freezes if it is not).
pub static STRICT: AtomicBool = AtomicBool::new(false);

static OK: AtomicU64 = AtomicU64::new(0);
static FALLBACK: AtomicU64 = AtomicU64::new(0);
static LAST_REPORT_MS: AtomicU64 = AtomicU64::new(0);

fn report(ok: u64, fallback: u64, reason: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if now.saturating_sub(LAST_REPORT_MS.load(Ordering::Relaxed)) < 5_000 {
        return;
    }
    LAST_REPORT_MS.store(now, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("ux0:data/xcloud-rust");
    let _ = std::fs::write(
        "ux0:data/xcloud-rust/direct-present.txt",
        format!("ok={ok} fallback={fallback} last_reason={reason}\n"),
    );
}

fn note_ok() {
    let ok = OK.fetch_add(1, Ordering::Relaxed) + 1;
    report(ok, FALLBACK.load(Ordering::Relaxed), "-");
}

fn note_fallback(reason: &str) {
    let fb = FALLBACK.fetch_add(1, Ordering::Relaxed) + 1;
    report(OK.load(Ordering::Relaxed), fb, reason);
}

#[cfg(target_os = "vita")]
mod imp {
    use super::{note_fallback, note_ok, ENABLED, STRICT};
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
    /// direct path was used (or STRICT is on), `false` to fall back to SDL.
    pub fn present_immediate(
        canvas: &mut sdl2::render::Canvas<sdl2::video::Window>,
        width: u32,
        height: u32,
    ) -> Result<bool> {
        if !ENABLED.load(super::Ordering::Relaxed) {
            return Ok(false);
        }
        let strict = STRICT.load(super::Ordering::Relaxed);

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
            note_fallback(if got != 0 {
                "getframebuf"
            } else if fb.base.is_null() {
                "null-base"
            } else if fb.pixelformat != SCE_DISPLAY_FRAMEBUF_FORMAT_B5G6R5 {
                "format"
            } else {
                "size"
            });
            return Ok(strict);
        }

        let pixels = match canvas.read_pixels(None, PixelFormatEnum::BGR565) {
            Ok(p) => p,
            Err(_) => {
                note_fallback("readpixels");
                return Ok(strict);
            }
        };

        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), fb.base as *mut u8, pixels.len());
            sceDisplaySetFrameBuf(&fb, SCE_DISPLAY_SETBUF_IMMEDIATE);
        }
        note_ok();
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