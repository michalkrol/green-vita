/// `telemetry` — per-frame FrameRecord ring (§6.12/§7) and session counters.
//
// Ground-truth harness: no new feature merges without its before/after in the
// harness. Telemetry is first-class and allocation-free: fixed-size per-frame
// records in a single-producer single-consumer lock-free ring.
//
// `FrameRecord` (~48–64 bytes, all fixed-size integers): rtp_ts, first_pkt_us,
// complete_us, submit_us, decode_start_us, decode_end_us, latch_us, flip_us,
// q_us, flags (u32 bitfield: complete/damaged/shed/lost_keyframe…). Times are
// LOCAL monotonic microseconds since session start (u64).
//
// `TelemetryRing<const N: usize>` (power-of-two N): SPSC lock-free ring using
// atomics only (head/tail with Acquire/Release), zero allocation after
// construction. `push(&FrameRecord) -> bool` drops silently when full.
// `drain()` returns an iterator of records, plus `dropped()`.
//
// `SessionCounters` — three historic drop classes (D0 damaged-frame/`pli_gap`,
// D0b waiting-for-keyframe, D1 depth-reject/`queue_full`/`pli_submit`) as plain
// AtomicU64 fields — continuity with the CSV.
//
// ring lands in the next task
pub mod ring;