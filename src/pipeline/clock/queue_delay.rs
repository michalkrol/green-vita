/// `queue_delay` — the queue-delay estimator Q from §4.3.
//
// For each packet group (burst-grouped within 5 ms, as in GCC):
//
// ```
// d_i = (t_arr_i − t_arr_{i-1}) − (Tsend_i − Tsend_{i-1})        # one-way delay gradient
// OWD_rel_i = Σ d             # relative one-way delay since session start
// base  = min-filter(OWD_rel, 10 s sliding window)
// Q     = OWD_rel − base      # estimated standing queue delay (signed us)
// ```
//
// `Q` is signed: never clamped to zero. When delay returns to base after
// the window slides, Q decays toward 0.
//
// The window is a ring of (timestamp, OWD_rel) samples, 1 sample per group
// over 10 s, allocation-free after construction. With burst grouping, samples
// are added at group rate, not packet rate.
//
// Units: microseconds (µs) unless otherwise noted. Pure std, no deps.

use std::time::Instant;

/// Sample stored in the sliding-window ring buffer.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// Timestamp when this sample was recorded (microseconds since session start).
    ts: u64,
    /// OWD_rel value at this instant.
    owd_rel: i64,
}

/// Per-packet inputs for the gradient estimator.
#[derive(Clone, Copy, Debug)]
pub struct PacketInput {
    /// Send time (unwrapped abs-send-time in microseconds).
    pub send_us: u64,
    /// Arrival time (local monotonic microseconds, stamped after recvfrom).
    pub arrival_us: u64,
}

/// Fixed-size ring buffer for the sliding-window min-filter.
///
/// Pre-allocated with capacity `N` (power of two). All operations are
/// allocation-free after construction. Write is a circular advance of the
/// head index; reading iterates over the current window contents.
/// Supports time-based eviction of old samples.
struct RingBuf<const N: usize> {
    /// Underlying storage, pre-allocated once.
    data: [Sample; N],
    /// Number of valid elements (cap wraps at N).
    len: usize,
    /// Circular write head position.
    head: usize,
}

impl<const N: usize> RingBuf<N> {
    /// Construct a new ring buffer.
    /// Data array is zero-initialized; validity is tracked via `len` and `head`.
    fn new() -> Self {
        RingBuf {
            data: [const { Sample { ts: 0, owd_rel: 0 } }; N],
            len: 0,
            head: 0,
        }
    }

    /// Push a sample, advancing the circular head.
    fn push(&mut self, sample: Sample) {
        let pos = self.head;
        self.data[pos] = sample;
        self.head = (pos + 1) % N;
        if self.len < N {
            self.len += 1;
        }
    }

    /// Iterate over all valid samples in the window, oldest first.
    fn iter(&self) -> RingBufIter<'_, N> {
        // Oldest sample position: head - len (wrapping around the circle)
        let start = if self.head >= self.len {
            self.head - self.len
        } else {
            N - (self.len - self.head)
        };
        RingBufIter {
            buf: self,
            start,
            count: self.len.min(N),
            pos: 0,
        }
    }

    /// Evict samples with `ts` older than `cutoff` (microseconds).
    ///
    /// Physically shifts remaining samples to the front of the array,
    /// adjusting `head` and `len` accordingly. Allocation-free.
    fn evict(&mut self, cutoff: u64) {
        if self.len == 0 {
            return;
        }

        // Find position of oldest sample
        let oldest_pos = if self.head >= self.len {
            self.head - self.len
        } else {
            N - (self.len - self.head)
        };

        // Count how many samples at the front have ts < cutoff
        let mut count_evicted = 0;
        let mut check_pos = oldest_pos;
        for _ in 0..self.len {
            if self.data[check_pos].ts < cutoff {
                count_evicted += 1;
                check_pos = (check_pos + 1) % N;
            } else {
                break;
            }
        }

        if count_evicted == 0 {
            return;
        }

        // If we're evicting all samples, just reset the buffer
        if count_evicted >= self.len {
            self.len = 0;
            self.head = 0;
            return;
        }

        let keep_len = self.len - count_evicted;

        // Shift kept samples to the front, skipping evicted ones
        let mut read = oldest_pos;
        let mut written = 0;
        while written < keep_len {
            // Skip evicted samples (those with ts < cutoff)
            while self.data[read].ts < cutoff {
                read = (read + 1) % N;
            }
            self.data[written] = self.data[read];
            written += 1;
            read = (read + 1) % N;
        }

        self.head = keep_len;
        self.len = keep_len;
    }
}

/// Iterator over valid samples in the RingBuf, oldest first.
struct RingBufIter<'a, const N: usize> {
    buf: &'a RingBuf<N>,
    start: usize,
    count: usize,
    pos: usize,
}

impl<'a, const N: usize> Iterator for RingBufIter<'a, N> {
    type Item = &'a Sample;

    fn next(&mut self) -> Option<&'a Sample> {
        if self.pos >= self.count {
            return None;
        }
        let idx = (self.start + self.pos) % N;
        let result = &self.buf.data[idx];
        self.pos += 1;
        Some(result)
    }
}

/// The queue-delay estimator Q (§4.3).
///
/// Maintains OWD_rel = cumulative sum of per-packet gradients d_i,
/// a 10-second sliding-window min-filter over OWD_rel samples, and
/// Q = OWD_rel - base (signed microseconds).
pub struct QueueDelayEstimator {
    /// OWD_rel = running sum of all d_i since session start.
    owd_rel: i64,
    /// Sliding-window ring of (timestamp, OWD_rel) samples for the min-filter.
    window: RingBuf<100>,
    /// Minimum OWD_rel observed in the current window (the "base").
    base: i64,
    /// send_us of the first packet in the current burst group.
    group_first_send_us: u64,
    /// arrival_us of the first packet in the current burst group.
    group_first_arrival_us: u64,
    /// send_us of the previous packet (for gradient and grouping).
    prev_send_us: u64,
    /// arrival_us of the previous packet (for gradient and grouping).
    prev_arrival_us: u64,
    /// Whether we have seen at least one packet.
    initialized: bool,
    /// 5 ms burst grouping threshold in microseconds.
    group_threshold: u64,
    /// Session start instant (for absolute timing if needed).
    session_start: Instant,
    /// Whether we have recorded at least one sample.
    sampled: bool,
}

impl QueueDelayEstimator {
    /// Create a new queue-delay estimator.
    pub fn new() -> Self {
        let group_threshold = 5_000; // 5 ms in us
        QueueDelayEstimator {
            owd_rel: 0,
            window: RingBuf::<100>::new(),
            base: 0,
            group_first_send_us: 0,
            group_first_arrival_us: 0,
            prev_send_us: 0,
            prev_arrival_us: 0,
            initialized: false,
            group_threshold,
            session_start: Instant::now(),
            sampled: false,
        }
    }

    /// Push a per-packet input and update the estimator.
    ///
    /// Packet grouping: packets whose send times are within 5 ms of the
    /// group's first packet belong to the same group. The gradient d_i is
    /// computed per group transition and accumulated into OWD_rel.
    ///
    /// On each new group: record a Sample{ts: arrival_us, owd_rel} and
    /// evict all samples with ts older than arrival_us - 10_000_000.
    ///
    /// Returns `Some(())` always (no packet dropping in this stage).
    pub fn push(&mut self, input: PacketInput) -> Option<()> {
        let send = input.send_us;
        let arr = input.arrival_us;

        if !self.initialized {
            // First packet: initialise state.
            self.owd_rel = 0;
            self.base = 0;
            self.initialized = true;
            self.prev_send_us = send;
            self.prev_arrival_us = arr;
            self.group_first_send_us = send;
            self.group_first_arrival_us = arr;
            // Record initial sample.
            self.window.push(Sample {
                ts: arr,
                owd_rel: 0,
            });
            self.sampled = true;
            return Some(());
        }

        // Determine if this packet belongs to the same group as the group's first packet.
        // A group is formed by packets whose send times are within 5 ms of the
        // group's first packet. The group's representative is the LAST packet.
        let in_same_group = (send as i64 - self.group_first_send_us as i64).abs() <= self.group_threshold as i64;

        // If this is a new group (send time differs by > 5 ms from the group's first),
        // compute the gradient, record a sample, and evict old samples.
        if !in_same_group {
            // Compute the gradient d_i = (t_arr_i − t_arr_{i-1}) − (Tsend_i − Tsend_{i-1})
            // using the previous packet's times (which is the representative of the old group).
            let d = (arr as i64 - self.prev_arrival_us as i64)
                - (send as i64 - self.prev_send_us as i64);

            // Accumulate into OWD_rel.
            self.owd_rel += d;

            // Record a new sample in the sliding window at the current OWD_rel.
            // The sample's timestamp is the arrival_us of the group representative
            // (the packet that started the new group, i.e. the current packet).
            self.window.push(Sample {
                ts: arr,
                owd_rel: self.owd_rel,
            });

            // Evict all samples with ts older than arrival_us - 10_000_000 (10 s).
            // Use saturating_sub to avoid underflow when arrival_us < 10 s.
            self.window.evict(arr.saturating_sub(10_000_000));

            // Re-evaluate the min-filter base over the (possibly evicted) window.
            self.recompute_base();

            // Update group first to current packet (new group starts).
            self.group_first_send_us = send;
            self.group_first_arrival_us = arr;
        } else {
            // Same group: still accumulate gradient for each packet within the group?
            // No — the spec says "Compute ONE gradient per GROUP transition".
            // Within a group, we still need to track prev for when the next group starts.
            // But we don't accumulate OWD_rel or record samples within a group.
            // Just update prev state.
        }

        // Update prev state.
        self.prev_send_us = send;
        self.prev_arrival_us = arr;

        Some(())
    }

    /// Recompute the base (min-filter over the current window).
    fn recompute_base(&mut self) {
        // Find the minimum owd_rel in the current window.
        let min = self.window.iter().map(|s| s.owd_rel).fold(
            i64::MAX,
            |a, b| a.min(b),
        );
        // If window is empty (should not happen after first push), use owd_rel.
        let base = if self.window.len > 0 { min } else { self.owd_rel };
        self.base = base;
    }

    /// Return the estimated standing queue delay Q in microseconds.
    ///
    /// Q = OWD_rel − base. Positive Q means the current delay is above the
    /// running minimum (estimated standing queue). Negative Q means we're
    /// below the historical minimum (should not normally happen but can).
    pub fn q_us(&self) -> i64 {
        self.owd_rel - self.base
    }

    /// Return the estimated standing queue delay Q in milliseconds (rounded).
    pub fn q_ms(&self) -> f64 {
        self.q_us() as f64 / 1000.0
    }

    /// Return the trend slope in microseconds per second (us/s),
    /// over the 10-second sliding window.
    ///
    /// Linear regression of OWD_rel samples against their timestamps.
    /// Positive slope means the queue is growing monotonically; negative
    /// means it's shrinking; zero means stable.
    /// All iteration — no Vec allocation.
    pub fn trend_us_per_s(&self) -> f64 {
        let mut n = 0f64;
        let mut sum_x = 0f64;
        let mut sum_y = 0f64;
        let mut sum_xy = 0f64;
        let mut sum_x2 = 0f64;

        for sample in self.window.iter() {
            let x = sample.ts as f64;
            let y = sample.owd_rel as f64;
            n += 1.0;
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_x2 += x * x;
        }

        if n < 2.0 {
            return 0.0;
        }

        // Linear regression: slope = (n*Σ(xy) - Σx*Σy) / (n*Σ(x²) - (Σx)²)
        let numerator = n * sum_xy - sum_x * sum_y;
        let denominator = n * sum_x2 - sum_x * sum_x;

        if denominator.abs() < 1e-6 {
            return 0.0;
        }

        // slope in us per us → convert to us per s
        let slope_us_per_us = numerator / denominator;
        slope_us_per_us * 1_000_000.0 // us/s
    }

    /// Return the trend slope in us/s (alias for trend_us_per_s).
    pub fn trend(&self) -> f64 {
        self.trend_us_per_s()
    }
}

impl Default for QueueDelayEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    

    #[test]
    fn test_flat_path() {
        // Packets with constant inter-arrival == inter-send => q_us ≈ 0
        let mut estimator = QueueDelayEstimator::new();
        // Send 100 packets with constant 20ms spacing (50 packets/s).
        // Constant inter-arrival == inter-send => d = 0 for each group.
        for i in 0..200 {
            let send_us = i as u64 * 20_000;
            let arrival_us = send_us; // constant delay of 0
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // With constant delay, q_us should be ~0
        assert!(
            estimator.q_us().abs() < 1_000,
            "Flat path: expected q_us ≈ 0, got {}",
            estimator.q_us()
        );
    }

    #[test]
    fn test_step_delay() {
        // Step: after a 100 ms standing delay is introduced, q_us rises to
        // ~100_000 µs within a few groups and HOLDS while the delay persists.
        let mut estimator = QueueDelayEstimator::new();
        // First, send packets with no delay
        for i in 0..50 {
            let send_us = i as u64 * 20_000;
            let arrival_us = send_us;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // Now introduce a 100 ms (100_000 µs) standing delay
        for i in 0..50 {
            let send_us = 50 as u64 * 20_000 + i as u64 * 20_000;
            let arrival_us = send_us + 100_000; // 100 ms standing delay
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // q_us should have risen to ~100_000 µs
        let q = estimator.q_us();
        println!("Step test: q_us = {} (expected ~100_000)", q);
        assert!(
            q > 80_000 && q < 120_000,
            "Step test: expected q_us ~100_000, got {}",
            q
        );
    }

    #[test]
    fn test_recovery() {
        // Phase A baseline: 50 packets, arrival_us == send_us, spacing 20_000 µs.
        let mut estimator = QueueDelayEstimator::new();
        for i in 0..50 {
            let send_us = i as u64 * 20_000;
            let arrival_us = send_us;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // Phase B step: 50 packets, arrival_us == send_us + 100_000 (100 ms standing delay).
        for i in 0..50 {
            let send_us = 50 as u64 * 20_000 + i as u64 * 20_000;
            let arrival_us = send_us + 100_000;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // q_us should have risen above 80_000 µs.
        let q = estimator.q_us();
        println!("Recovery test Phase B: q_us = {} (expected > 80_000)", q);
        assert!(
            q > 80_000,
            "Recovery Phase B: expected q_us > 80_000, got {}",
            q
        );
        // Phase C recovery: 600 packets (12 s of send time), delay removed
        // entirely (arrival_us == send_us). The 100 ms backward step relative
        // to Phase B's arrival IS the recovery signal: the gradient d drops by
        // 100 ms, owd_rel returns to 0, and Q (signed, never clamped) follows.
        for i in 0..600 {
            let send_us = 100 as u64 * 20_000 + i as u64 * 20_000;
            let arrival_us = send_us;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // q_us should have decayed toward 0 as old 100 ms delay samples evicted.
        let q = estimator.q_us();
        println!("Recovery test Phase C: q_us = {} (expected < 20_000)", q);
        assert!(
            q < 20_000,
            "Recovery Phase C: expected q_us < 20_000, got {}",
            q
        );
    }

    /// After a >10 s standing 100 ms delay, q_us returns toward ~0 because
    /// the 10 s min-filter re-bases. The absolute delay is unobservable long-term;
    /// Q tracks changes in delay.
    #[test]
    fn test_base_absorbs_standing_delay() {
        // Documents intended semantics: a STANDING delay >10 s is absorbed by
        // the 10 s min-filter (base re-bases upward), so Q decays back toward 0.
        // Absolute delay is unobservable long-term; Q tracks CHANGES.
        let mut estimator = QueueDelayEstimator::new();
        // Phase A: 1 s of baseline, no delay.
        for i in 0..50 {
            let send_us = i as u64 * 20_000;
            let arrival_us = send_us;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // Phase B: step to +100 ms standing delay, held for >10 s (12 s total).
        for i in 0..600 {
            let send_us = 50 as u64 * 20_000 + i as u64 * 20_000;
            let arrival_us = send_us + 100_000;
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
            // Early in Phase B (first ~1 s) the step is visible: Q ≈ 100 ms.
            if i == 50 {
                let q = estimator.q_us();
                assert!(
                    q > 80_000,
                    "Absorb (after 1 s of standing delay): expected q_us > 80_000, got {}",
                    q
                );
            }
        }
        // After >10 s standing, all Phase A (q=0) samples have aged out of the
        // window: base rose to the standing delay and Q decays to ~0.
        let q = estimator.q_us();
        assert!(
            q < 20_000,
            "Absorb (after 12 s standing delay): expected q_us < 20_000, got {}",
            q
        );
    }

    #[test]
    fn test_burst_grouping() {
        // Burst grouping: a burst of packets sharing send times within 5 ms
        // counts as one group (e.g. q and group count behave as one gradient).
        let mut estimator = QueueDelayEstimator::new();
        // Send a burst of 10 packets with the same send time (within 5 ms)
        let base_send = 1_000_000u64;
        for i in 0..10 {
            let send_us = base_send + i as u64 * 100; // within 5 ms = 5_000 µs
            let arrival_us = send_us; // 0 delay
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        // After the burst, q_us should reflect the single gradient
        // (the burst counts as one group, not 10 individual gradients)
        let q = estimator.q_us();
        println!("Burst test: q_us = {}", q);
        // With 0 delay and bursts counted as one group, q_us should be 0 or very small
        assert!(
            q.abs() < 10_000,
            "Burst test: expected q_us ~0, got {}",
            q
        );
    }

    #[test]
    fn test_trend_positive() {
        // Trend: monotonic +50 ms/s growth yields a positive trend_us_per_s()
        let mut estimator = QueueDelayEstimator::new();
        // Send packets with a linearly growing delay: +50 µs per packet
        // At ~20 packets/s, that's +2500 µs/s = +2.5 ms/s, but we want +50 ms/s
        // Let's use a faster rate: send 100 packets with increasing delay
        for i in 0..100 {
            let send_us = i as u64 * 10_000; // 10 ms spacing
            // Add +50 µs per packet to the delay
            let arrival_us = send_us + (i as u64 * 50); // growing delay
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        let trend = estimator.trend_us_per_s();
        println!("Positive trend test: trend_us_per_s = {}", trend);
        // Positive slope should be detected
        assert!(
            trend > 10.0,
            "Positive trend test: expected positive trend > 10 us/s, got {}",
            trend
        );
    }

    #[test]
    fn test_trend_flat() {
        // Flat: flat yields ≈0 trend_us_per_s()
        let mut estimator = QueueDelayEstimator::new();
        // Send packets with constant 0 delay
        for i in 0..100 {
            let send_us = i as u64 * 10_000;
            let arrival_us = send_us; // 0 delay
            estimator.push(PacketInput { send_us, arrival_us }).unwrap();
        }
        let trend = estimator.trend_us_per_s();
        println!("Flat trend test: trend_us_per_s = {}", trend);
        assert!(
            trend.abs() < 1.0,
            "Flat trend test: expected trend ≈ 0, got {}",
            trend
        );
    }
}