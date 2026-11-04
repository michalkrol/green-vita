/// `pipeline` — pure, host-testable components of the refactored video pipeline
/// (see RENDERING-PIPELINE-ARCHITECTURE-PROPOSITION.md §12).
///
/// Stages land incrementally; `#[allow(dead_code)]` on the crate-root module is
/// removed as wiring completes.
///
/// Clock model (§4, §16.3): RTP-timestamp gradient + abs-send-time with a
/// FIXED 64-second-wrap parser (24-bit 6.18 fixed-point SECONDS). No SR/RR
/// based clock — the console sends no RTCP at all.
///
/// Telemetry is first-class and allocation-free: fixed-size per-frame records
/// in a single-producer single-consumer lock-free ring.

pub mod clock;
pub mod decode;
pub mod media;
pub mod telemetry;