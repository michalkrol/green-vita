/// Shared state for latency test mode.
/// - blink state: toggles each button press, drives the white/black square.
/// - last press timestamp: stored as microseconds since origin.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Time base shared across all calls. Initialized on first button press.
pub(crate) static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Atomic blink state: true = square should be white, false = black.
pub(crate) static BLINK: AtomicBool = AtomicBool::new(false);

/// Atomic timestamp of the last press age in microseconds since ORIGIN.
pub(crate) static LAST_PRESS_US: AtomicU64 = AtomicU64::new(0);

/// Flag indicating whether a button press has been recorded.
pub(crate) static HAS_PRESSED: AtomicBool = AtomicBool::new(false);

/// Record a button press: compute age since origin, store it, and toggle blink state.
///
/// This is intentionally cheap: one OnceLock get-or-init, one atomic store,
/// one atomic fetch-xor. No allocations beyond the OnceLock.
pub(crate) fn note_button_press() {
    let origin = ORIGIN.get_or_init(Instant::now);
    let elapsed = origin.elapsed().as_micros() as u64;
    LAST_PRESS_US.store(elapsed, Ordering::Relaxed);
    HAS_PRESSED.store(true, Ordering::Relaxed);
    let _prev = BLINK.fetch_xor(true, Ordering::Relaxed);
}

/// Return the age since the last button press, in milliseconds.
/// Returns None ONLY when no press has been recorded yet.
///
/// The returned value is: (now - ORIGIN) - LAST_PRESS_US, in milliseconds.
/// Right after a press this reads ~0 ms and then grows in real time;
/// every new press resets it to ~0 ms.
pub(crate) fn last_press_age_ms() -> Option<f32> {
    if !HAS_PRESSED.load(Ordering::Relaxed) {
        return None;
    }
    let origin = ORIGIN.get()?;
    let now_since_origin = Instant::now().duration_since(*origin).as_micros() as u64;
    let age_us_since_press = now_since_origin.saturating_sub(LAST_PRESS_US.load(Ordering::Relaxed));
    Some(age_us_since_press as f32 / 1000.0)
}