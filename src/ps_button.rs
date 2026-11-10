//! PS (Home) button capture while streaming.
//!
//! Mechanism (vita-moonlight commit `6690a4e`, decoded in
//! HOME-BUTTON-BRAINSTORM.md §2): `sceShellUtilLock(PS_BTN | PS_BTN_2)` makes
//! the shell ignore PS presses, and an fself built with the
//! `0x2800000000000001` / `-at 0x0E` attributes exposes the PS bit
//! (`SCE_CTRL_PSBUTTON`, 0x00010000) in ordinary `sceCtrlPeekBufferPositiveExt2`
//! samples. Pure userland — no hooks, no kernel modules.
//!
//! Gestures — three press tiers (user-specified on-device, 2026-11-09):
//! tap (release <= 350 ms) -> [`PsButtonTick::GuidePressed`] (host draws the
//! Xbox guide); hold >= 600 ms -> unlock + a 2 s passthrough window
//! ([`PsButtonTick::PassthroughArmed`]); the user's *next* press then reaches
//! the Vita shell and opens the home screen. SceShell is edge-triggered — it
//! acts on press rising edges — so we never rely on the currently-held press;
//! the re-lock waits for release and the shell sees a fresh rising edge.
//! Keeping the button held after the unlock hands control to the OS, whose
//! own long-press Quick Menu is the third tier (a deliberate one, per the
//! same device feedback).
//!
//! Resilience (brainstorm §6): the lock fails open. `spawn_watchdog` runs on
//! CPU 1; if the main loop stalls > 5 s while the lock is held it unlocks and
//! latches capture off until the next `set_watchdog_armed(true)` (a stream
//! restart implies a live main thread). `force_unlock` before
//! `sceKernelExitProcess` (Enso quirk: exiting while locked can wedge PS in
//! LiveArea). All state is lock-free atomics; tick/watchdog paths allocate,
//! log and panic nowhere.
//!
//! SDL2 coexistence (brainstorm §2): peeking is read-only and this module
//! NEVER calls `sceCtrlSetSamplingMode[Ext]` — SDL's Vita backend owns the
//! sampling mode (`ANALOG_WIDE` via Ext) and touching it would downgrade
//! SDL's analog range. Ports 0 *and* 1 are polled because on PSTV /
//! ds34vita a DualShock's PS press lands on port 1.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use vitasdk_sys::{
    SceCtrlData, SCE_CTRL_PSBUTTON, SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN,
    SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN_2, sceCtrlPeekBufferPositiveExt2, sceShellUtilInitEvents,
    sceShellUtilLock, sceShellUtilUnlock,
};

/// Shell acts on press rising edges, so gestures are release-edge detected
/// and re-lock defers past any held button (brainstorm §4/§5). Three press
/// tiers (user-specified on-device, 2026-11-09): tap -> host Guide; hold
/// >= PS_HOLD_TO_VITA_MS -> unlock + passthrough window (release and press
/// again for the Vita home screen); keep holding past ~2 s -> the OS's own
/// Quick Menu, which takes over once the button is unlocked and held.
const PS_TAP_MAX_MS: i64 = 350;
const PS_HOLD_TO_VITA_MS: i64 = 600;
const PS_PASS_WINDOW_MS: i64 = 2000;
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(5);
const WATCHDOG_POLL: Duration = Duration::from_millis(500);

/// Both PS lock types in one call, as vita-moonlight `6690a4e` does.
const PS_LOCK_TYPES: u32 = SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN | SCE_SHELL_UTIL_LOCK_TYPE_PS_BTN_2;

/// Result of one poll-window tick of the PS-button gesture machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsButtonTick {
    Idle,
    GuidePressed,
    PassthroughArmed { until: Instant },
}

// Permanent latch: set only by force_unlock (process quitting / forced off).
static DISABLED: AtomicBool = AtomicBool::new(false);
static INIT_DONE: AtomicBool = AtomicBool::new(false);
// Gesture state (main thread only; atomics so the watchdog can share the
// lock bit without a mutex).
static LAST_PRESSED: AtomicBool = AtomicBool::new(false);
static PRESS_START_MS: AtomicI64 = AtomicI64::new(0);
static ARMED_THIS_PRESS: AtomicBool = AtomicBool::new(false);
static PASS_WINDOW_END_MS: AtomicI64 = AtomicI64::new(0);
// Lock bookkeeping: LAST_APPLIED guards against per-tick syscall spam,
// IS_PS_LOCKED reflects the shell only when the syscall succeeded.
static LAST_APPLIED_LOCK: AtomicBool = AtomicBool::new(false);
static IS_PS_LOCKED: AtomicBool = AtomicBool::new(false);
// Watchdog: armed per stream, trips off latched until the next arm.
static WATCHDOG_ARMED: AtomicBool = AtomicBool::new(false);
static WATCHDOG_TRIPPED: AtomicBool = AtomicBool::new(false);
static WATCHDOG_SPAWNED: AtomicBool = AtomicBool::new(false);
static WATCHDOG_PINNED: AtomicBool = AtomicBool::new(false);
static HEARTBEAT_MS: AtomicI64 = AtomicI64::new(0);

static TIME_ANCHOR: OnceLock<Instant> = OnceLock::new();

fn now_millis() -> i64 {
    TIME_ANCHOR.get_or_init(Instant::now).elapsed().as_millis() as i64
}

fn instant_at(ms: i64) -> Instant {
    *TIME_ANCHOR.get_or_init(Instant::now) + Duration::from_millis(ms as u64)
}

/// One-time shell-util setup; idempotent. Called lazily from the first
/// capture-enabled `tick` — NOT at startup — so a build with capture off
/// makes zero shell/ctrl syscalls versus the pre-capture binary (bisect
/// safety after the C1-9654-4 incident; brainstorm §11).
pub fn init() {
    if INIT_DONE.swap(true, Ordering::AcqRel) {
        return;
    }
    let result = unsafe { sceShellUtilInitEvents(0) };
    if result != 0 {
        eprintln!("ps_button: sceShellUtilInitEvents failed: {result:#x}");
    }
}

/// PS-button gesture machine, called from the main input poll window.
pub fn tick(capture_enabled: bool, streaming_unpaused: bool) -> PsButtonTick {
    if DISABLED.load(Ordering::Relaxed) {
        return PsButtonTick::Idle;
    }
    if capture_enabled {
        init();
    } else {
        // Capture off: forget any in-flight gesture so a stale hold can't
        // fire across a pause or a setting flip, release the lock if we held
        // one, and touch no syscalls at all (peek below is gated on this).
        LAST_PRESSED.store(false, Ordering::Relaxed);
        PRESS_START_MS.store(0, Ordering::Relaxed);
        ARMED_THIS_PRESS.store(false, Ordering::Relaxed);
        PASS_WINDOW_END_MS.store(0, Ordering::Relaxed);
        if LAST_APPLIED_LOCK.swap(false, Ordering::Relaxed) {
            apply_lock(false);
        }
        return PsButtonTick::Idle;
    }

    let now = now_millis();
    let pressed = peek_pressed();

    // Passthrough window expiry: the window outlives its deadline while the
    // button is still held (re-lock must wait for release, brainstorm §5
    // edge-safety) and clears once it expires into a released button.
    let window_end = PASS_WINDOW_END_MS.load(Ordering::Relaxed);
    if window_end != 0 && now >= window_end && !pressed {
        PASS_WINDOW_END_MS.store(0, Ordering::Relaxed);
    }
    let pass_active = PASS_WINDOW_END_MS.load(Ordering::Relaxed) != 0;

    let desired_lock = capture_enabled
        && streaming_unpaused
        && !pass_active
        && !WATCHDOG_TRIPPED.load(Ordering::Relaxed);
    let applied = LAST_APPLIED_LOCK.load(Ordering::Relaxed);
    // Apply only on desired CHANGE, and never lock under a held PS button —
    // the edge-triggered shell would see the release edge of a hold that
    // predates the lock (defer to a later tick instead).
    if desired_lock != applied && (!desired_lock || !pressed) {
        apply_lock(desired_lock);
    }

    if pressed {
        if !LAST_PRESSED.load(Ordering::Relaxed) {
            // Rising edge: press begins.
            PRESS_START_MS.store(now, Ordering::Relaxed);
            ARMED_THIS_PRESS.store(false, Ordering::Relaxed);
        } else {
            let start = PRESS_START_MS.load(Ordering::Relaxed);
            let held_ms = now.saturating_sub(start);
            if start != 0
                && held_ms >= PS_HOLD_TO_VITA_MS
                && !ARMED_THIS_PRESS.load(Ordering::Relaxed)
                && IS_PS_LOCKED.load(Ordering::Relaxed)
            {
                // Mid-hold tier: unlock and open the passthrough window —
                // the user releases and presses again for the Vita home
                // screen (a clean rising edge the edge-triggered shell
                // acts on). Keep holding instead and the OS's own long-press
                // Quick Menu takes over (that shell behavior is the third,
                // intentional tier), while the window expires on release and
                // the lock is restored on a later tick.
                ARMED_THIS_PRESS.store(true, Ordering::Relaxed);
                let until_ms = now + PS_PASS_WINDOW_MS;
                PASS_WINDOW_END_MS.store(until_ms, Ordering::Relaxed);
                apply_lock(false);
                return PsButtonTick::PassthroughArmed {
                    until: instant_at(until_ms),
                };
            }
        }
    } else if LAST_PRESSED.load(Ordering::Relaxed) {
        // Release edge: a short tap with nothing armed fires the host Guide.
        let start = PRESS_START_MS.load(Ordering::Relaxed);
        let held_ms = now.saturating_sub(start);
        let armed = ARMED_THIS_PRESS.load(Ordering::Relaxed);
        PRESS_START_MS.store(0, Ordering::Relaxed);
        ARMED_THIS_PRESS.store(false, Ordering::Relaxed);
        if start != 0
            && held_ms <= PS_TAP_MAX_MS
            && streaming_unpaused
            && !pass_active
            && !armed
            // Fail-open guard: if the shell lock was never acquired (e.g. a
            // non-attributed fself makes sceShellUtilLock fail), the button
            // is native — the OS already saw this tap, so we must not also
            // fire the host Guide.
            && is_ps_locked()
        {
            LAST_PRESSED.store(false, Ordering::Relaxed);
            return PsButtonTick::GuidePressed;
        }
    }
    LAST_PRESSED.store(pressed, Ordering::Relaxed);
    PsButtonTick::Idle
}

/// Main loop calls this every iteration; silence for > 5 s while the lock is
/// held makes the watchdog release it.
pub fn bump_heartbeat() {
    HEARTBEAT_MS.store(now_millis(), Ordering::Relaxed);
}

/// Arm the watchdog (stream live). Re-arming also clears a previous
/// watchdog-trip latch — a stream restart implies a live main thread — and
/// refreshes the heartbeat so a stale one can't trip instantly.
pub fn set_watchdog_armed(armed: bool) {
    WATCHDOG_ARMED.store(armed, Ordering::Relaxed);
    if armed {
        WATCHDOG_TRIPPED.store(false, Ordering::Relaxed);
        HEARTBEAT_MS.store(now_millis(), Ordering::Relaxed);
    }
}

/// Unlock both PS lock types and permanently disable this module (tick
/// becomes a no-op `Idle`). Must run before `sceKernelExitProcess` — Enso
/// 3.60/3.65 can leave PS unresponsive in LiveArea when exiting while locked.
pub fn force_unlock() {
    let _ = unsafe { sceShellUtilUnlock(PS_LOCK_TYPES) };
    IS_PS_LOCKED.store(false, Ordering::Relaxed);
    LAST_APPLIED_LOCK.store(false, Ordering::Relaxed);
    DISABLED.store(true, Ordering::Relaxed);
}

pub fn is_ps_locked() -> bool {
    IS_PS_LOCKED.load(Ordering::Relaxed)
}

/// Whether the PS button is currently held. Reflects the LAST_PRESSED sample
/// from the last `tick` (the main loop polls every 4 ms), not a live interrupt.
pub fn is_pressed() -> bool {
    LAST_PRESSED.load(Ordering::Relaxed)
}

/// Spawn the fail-open watchdog once (idempotent). Pinned to CPU 1 — the RTC
/// core — because the main thread lives on CPU 0 and a same-core watchdog
/// would starve if the main thread spin-hangs (brainstorm §6). The pin is
/// applied lazily on the first armed iteration so an unarmed watchdog makes
/// no syscalls at all.
pub fn spawn_watchdog() {
    if WATCHDOG_SPAWNED.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("ps-watchdog".into())
        .spawn(|| {
            loop {
                std::thread::sleep(WATCHDOG_POLL);
                if !WATCHDOG_ARMED.load(Ordering::Relaxed) {
                    continue;
                }
                if !WATCHDOG_PINNED.swap(true, Ordering::AcqRel) {
                    pin_watchdog_thread();
                }
                if !IS_PS_LOCKED.load(Ordering::Relaxed) {
                    continue;
                }
                let now = now_millis();
                let heartbeat = HEARTBEAT_MS.load(Ordering::Relaxed);
                if now.saturating_sub(heartbeat) <= WATCHDOG_TIMEOUT.as_millis() as i64 {
                    continue;
                }
                // Main loop stalled while capture is on: release the lock and
                // latch capture off until the next arm. Latching (not
                // re-arming) means a possibly-frozen main loop can never
                // re-lock — the re-lock race is eliminated by construction.
                let _ = unsafe { sceShellUtilUnlock(PS_LOCK_TYPES) };
                IS_PS_LOCKED.store(false, Ordering::Relaxed);
                LAST_APPLIED_LOCK.store(false, Ordering::Relaxed);
                WATCHDOG_TRIPPED.store(true, Ordering::Relaxed);
            }
        });
    if let Err(error) = spawned {
        eprintln!("ps_button: watchdog spawn failed: {error}");
        WATCHDOG_SPAWNED.store(false, Ordering::Relaxed);
    }
}

fn pin_watchdog_thread() {
    let thread_id = unsafe { vitasdk_sys::sceKernelGetThreadId() };
    let result = unsafe {
        vitasdk_sys::sceKernelChangeThreadCpuAffinityMask(
            thread_id,
            vitasdk_sys::SCE_KERNEL_CPU_MASK_USER_1 as i32,
        )
    };
    if result < 0 {
        eprintln!("ps_button: failed to pin watchdog to CPU 1: {result:#x}");
    }
}

/// PS bit from ctrl port 0 (handheld) or port 1 (PSTV / ds34vita pads).
/// Ext2 peek only — plain Peek filters system buttons on several firmwares,
/// and sampling mode is never touched (SDL owns it).
fn peek_pressed() -> bool {
    unsafe {
        let mut port0: SceCtrlData = std::mem::zeroed();
        let mut port1: SceCtrlData = std::mem::zeroed();
        let ok0 = sceCtrlPeekBufferPositiveExt2(0, &mut port0, 1) >= 0;
        let ok1 = sceCtrlPeekBufferPositiveExt2(1, &mut port1, 1) >= 0;
        (ok0 && port0.buttons & SCE_CTRL_PSBUTTON != 0)
            || (ok1 && port1.buttons & SCE_CTRL_PSBUTTON != 0)
    }
}

/// Apply the desired lock state. The desired state is marked applied even if
/// the syscall fails (a non-attributed fself makes sceShellUtilLock fail with
/// 0x801011xx): retrying per tick would spam the shell, and a failed lock
/// fails open — PS stays native and IS_PS_LOCKED stays false, so neither the
/// hold-to-arm path nor the watchdog ever act on a lock we don't hold.
fn apply_lock(locked: bool) {
    let result = unsafe {
        if locked {
            sceShellUtilLock(PS_LOCK_TYPES)
        } else {
            sceShellUtilUnlock(PS_LOCK_TYPES)
        }
    };
    LAST_APPLIED_LOCK.store(locked, Ordering::Relaxed);
    IS_PS_LOCKED.store(locked && result == 0, Ordering::Relaxed);
}
