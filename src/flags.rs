//! Build-time feature flags — check/change ALL switches here.

/// When true, offers abs-send-time(2) + video-timing(7) extmaps in SDP video m-section (Phase 1.1).
/// Set false to restore the historical zero-extmap offer for A/B comparisons.
pub const OFFER_VIDEO_EXTMAPS: bool = true;

/// When false, no CSV file is created/opened/appended at any point.  All in-memory metrics, HUD, and
/// status-string behavior remain exactly as before. Default true so existing behavior is unchanged.
pub const METRICS_CSV_LOGGING: bool = true;

/// Revert 55573ba4: metadata loop undone because "minimal values test best"; risk of re-introducing loss-recovery loops.
pub const SEND_RENDER_TIMING_METADATA: bool = false;

/// Cadence for the periodic keyframe (IDR) request while the periodic_keyframe setting is ON. 1200 ms ≈ the effective ~1/s IDR cadence that f1bf114's accidental drop→PLI valve produced (validated low-latency session); 200 was the old flood (5/s saturated the 3.5–5 Mbps hop and made latency 5× worse). 0 disables.
pub const PERIODIC_KEYFRAME_INTERVAL_MS: u64 = 1200;

/// v0.3.0 recipe: decode-loop pacing sleep (ms). 0 = full-speed decode (post-21f0c51 behavior).
/// Restores the paced decode cadence that kept the AU queue shedding at ~1/s keyframes.
/// When >0: the decode loop sleeps this long after each iteration exactly as 21f0c51^ did
/// (same placement/statistics semantics as the original — copy the original code, gate with
/// `if crate::flags::DECODE_PACE_SLEEP_MS > 0 { ... }`).
/// **Now acts as a cap/kill-switch**: effective_sleep = min(table_value, DECODE_PACE_SLEEP_MS),
/// so a future 4ms baseline A/B still works, catch-up 2ms still wins. Set to 0 to disable
/// pacing entirely (full-speed regime).
pub const DECODE_PACE_SLEEP_MS: u64 = 13;

/// Sleep (ms) after each decoded frame indexed by REMAINING AU-queue depth 0..=6.
/// User schedule 2026-10-25: depth1=13, 2=11, 3=10, … 6=7 so we never sit on ~6 frames
/// (6×13=100ms of stored latency) — the queue continuously drains instead.
pub const DECODE_PACE_DEPTH_TABLE_MS: [u64; 7] = [13, 13, 11, 10, 9, 8, 7];

/// Sleep for remaining depth ≥7: aggressive catch-up (12-deep backlog clears in tens of ms,
/// not ~2 s at fixed 13ms).
pub const DECODE_PACE_CATCHUP_MS: u64 = 2;

/// AU-queue age bound (drop-OLDEST). 0 = disabled → v0.3.0 semantics: depth-reject only
/// (reference-preserving). 150 = post-21f0c51 latency guardrail from the open-loop era;
/// drop-oldest breaks H.264 references (mixed colours until next IDR) and is redundant
/// while pace-sleep+depth-6 bound residence ≈100 ms.
pub const AU_QUEUE_MAX_AGE_MS: u64 = 0;

/// v0.3.0 recipe: adaptive AU queue depth limit (1..6 based on source fps) that produced
/// queue-full drops→keyframe requests; false = fixed 8 + age-150ms path (post-21f0c51).
/// When true: restore the original `pending_limit`/adaptive limit logic exactly as in
/// 21f0c51^'s submit path; when false: keep the current code path intact.
/// Preserve the existing AU_QUEUE_MAX_AGE age-drop behavior in BOTH cases (do not remove
/// the 150 ms age bound).
/// AU-queue depth-reject threshold (frames). 6 = exact v0.3.0 adaptive ceiling (1..6).
/// 12 = decode-deeper experiment 2026-10-25: absorb drop-episodes by decoding them
/// (reference chains intact, corruption reduced to wire-loss) at the cost of transient
/// residence ≤ depth×frame-time during backlogs. Must match the bounded() channel capacity.
pub const AU_QUEUE_MAX_DEPTH: usize = 12;

pub const ADAPTIVE_AU_LIMIT: bool = true;