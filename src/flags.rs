//! Build-time feature flags — check/change ALL switches here.

/// When true, offers abs-send-time(2) + video-timing(7) extmaps in SDP video m-section (Phase 1.1).
/// Set false to restore the historical zero-extmap offer for A/B comparisons.
pub const OFFER_VIDEO_EXTMAPS: bool = true;

/// When false, no CSV file is created/opened/appended at any point.  All in-memory metrics, HUD, and
/// status-string behavior remain exactly as before. Default true so existing behavior is unchanged.
pub const METRICS_CSV_LOGGING: bool = true;

/// Revert 55573ba4: metadata loop undone because "minimal values test best"; risk of re-introducing loss-recovery loops.
pub const SEND_RENDER_TIMING_METADATA: bool = false;