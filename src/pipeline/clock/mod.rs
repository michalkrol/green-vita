/// `clock` — RTP timestamp → monotonic clock, abs-send-time unwrapper,
/// queue-delay estimator Q (§4.3).
//
// Clock model (§4, §16.3): RTP-timestamp gradient + abs-send-time with a
// FIXED 64-second-wrap parser. The console sends no RTCP (B2), so there is
// no SR/RR. The model uses:
//   • abs-send-time extension (id 7) — 24-bit 6.18 fixed-point seconds,
//     wrapping every 64 s.
//   • RTP-ts gradient for inter-packet delay when abs-send-time is absent.
//   • No lip-sync enforcement; audio clock domain is independent (§7).
//
// All units are local monotonic microseconds since session start, unless
// otherwise noted. Pure std, no deps.

pub mod send_time;
pub mod queue_delay;