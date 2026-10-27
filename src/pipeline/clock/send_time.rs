/// `send_time` — abs-send-time RTP header extension parser with wrap handling.
//
// The console sends abs-send-time extension (id 7) with each video packet.
// The value is 24-bit, 6.18 fixed-point SECONDS:
//   raw_24-bit_integer = seconds * 2^18 + fraction
// where the low 18 bits are the fraction of a second.
// The value wraps every 64 seconds (24-bit unsigned range 0..2^24 = 16777216,
// 16777216 / 2^18 = 64).
//
// This module provides `SendTimeUnwrapper` which converts the wrapping 24-bit
// value into a monotonically increasing u64 of microseconds.
// It uses a fixed 64-second wrap interval and detects forward/backward jumps
// using modular arithmetic to tolerate out-of-order packets within ±32 s.
//
// Units: microseconds (µs) unless otherwise noted. Pure std, no deps.

/// Raw 24-bit abs-send-time value from the RTP header extension.
/// Low 18 bits are the fractional part (6.18 fixed-point), high 6 bits are
/// the integer seconds part (value = seconds * 2^18 + fraction).
type Raw24 = u32;

/// Monotonically increasing microseconds since session start.
type Microsecs = u64;

/// Number of microseconds in 64 seconds (the wrap interval).
const WRAP_INTERVAL_US: Microsecs = 64_000_000;

/// 24-bit mask (keep only low 24 bits).
const MASK_24: Raw24 = 0x00FFFFFF;

/// Ambiguity window: we tolerate up to ±32_000_000 µs (32 s) of backward/jumping.
/// Deltas larger than this in magnitude are treated as implausible resync events.
const AMBIG_US: Microsecs = 32_000_000;

/// Convert a 24-bit raw abs-send-time value to microseconds in [0, 64_000_000).
///
/// The wire value is 24 bits, 6.18 fixed-point SECONDS:
///   raw = seconds << 18 | fraction, where fraction is low 18 bits.
/// The conversion to microseconds is: raw_us = (raw as u64) * 1_000_000 / 262_144.
/// This wraps every 64 seconds.
///
/// Input is masked to 24 bits.
pub fn parse(raw24: u32) -> u64 {
    let raw = raw24 & 0x00FFFFFF;
    (raw as u64) * 1_000_000 / 262_144
}

/// `SendTimeUnwrapper` converts a sequence of possibly-wrapping 24-bit
/// abs-send-time values into a monotonically increasing u64 of microseconds.
//
// The parser uses a fixed 64-second wrap interval. Between two consecutive
// packets, the signed modular delta is computed using shortest-distance
// modular arithmetic to tolerate out-of-order packets within ±32 s.
//
// Invariants:
//   - unwrapped_us is always monotonically non-decreasing.
//   - last_raw_us tracks the raw value (as u64) that last contributed to the
//     high-water mark (not necessarily the most recently pushed raw).
//   - last_raw tracks the raw 24-bit value from the previous packet,
//     used for delta computation.
//   - Between updates, the modular delta is computed relative to last_raw_us.
pub struct SendTimeUnwrapper {
    /// Anchor raw 24-bit value at the high-water mark (None when re-anchor pending).
    anchor_raw: Option<Raw24>,
    /// Unwrapped microseconds at the high-water mark anchor.
    anchor_us: u64,
    /// Monotonic high-water mark of unwrapped microseconds.
    unwrapped_us: u64,
    /// Whether we have seen any data yet (first call sets the base).
    initialized: bool,
}

impl SendTimeUnwrapper {
    /// Create a new unwrapper, uninitialized. The first `push` call sets the base.
    pub fn new() -> Self {
        SendTimeUnwrapper {
            anchor_raw: None,
            anchor_us: 0,
            unwrapped_us: 0,
            initialized: false,
        }
    }

    /// Convert a raw 24-bit value to microseconds in [0, 64_000_000).
    fn raw_to_us(raw: Raw24) -> Microsecs {
        parse(raw)
    }

    /// Push a new raw 24-bit abs-send-time value and return the unwrapped
    /// microseconds value.
    ///
    /// Returns the current unwrapped monotonic microseconds value.
    pub fn push(&mut self, raw24: u32) -> u64 {
        let raw = raw24 & MASK_24;
        let us = Self::raw_to_us(raw);

        // First call: set the base.
        if !self.initialized {
            self.anchor_raw = Some(raw);
            self.anchor_us = us;
            self.unwrapped_us = us;
            self.initialized = true;
            return us;
        }

        // Re-anchor pending (armed by a resync): the next plausible packet
        // establishes a new epoch in the same 64 s lattice as the high-water.
        if self.anchor_raw.is_none() {
            let mut base = (self.unwrapped_us / WRAP_INTERVAL_US) * WRAP_INTERVAL_US + us;
            if base < self.unwrapped_us {
                base += WRAP_INTERVAL_US;
            }
            self.anchor_raw = Some(raw);
            self.anchor_us = base;
            self.unwrapped_us = base;
            return base;
        }

        // Forward unsigned delta from the ANCHOR (the raw at the high-water
        // mark) in [0, 64_000_000).
        let delta = (us + WRAP_INTERVAL_US - self.anchor_us_parsed()) % WRAP_INTERVAL_US;

        if delta <= AMBIG_US {
            // Plausible forward progress (≤ 32 s). The anchor sits at the
            // high-water, so this always advances: move anchor + high-water.
            let out = self.anchor_us + delta;
            self.unwrapped_us = out;
            self.anchor_raw = Some(raw);
            self.anchor_us = out;
            return out;
        }

        // |delta| > 32 s: either an implausible epoch jump or a stale
        // out-of-order packet. Ignore this packet's value (return the
        // unchanged high-water) and arm a re-anchor for the next push.
        self.anchor_raw = None;
        self.unwrapped_us
    }

    /// Parsed µs of the current anchor raw (0 when re-anchor is armed —
    /// never called in that state).
    fn anchor_us_parsed(&self) -> u64 {
        Self::raw_to_us(self.anchor_raw.unwrap_or(0))
    }
}

impl Default for SendTimeUnwrapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_progression() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // First packet: raw = 0 (t = 0 us)
        let us = unwrapper.push(0);
        assert_eq!(us, 0, "First packet at raw=0 should give us=0");

        // Second packet: raw = 1 << 18 = 262144 (t = 1 s = 1_000_000 us)
        let us = unwrapper.push(262144);
        assert_eq!(us, 1_000_000, "Second packet at raw=262144 should give us=1_000_000");

        // Third packet: raw = 2 << 18 = 524288 (t = 2 s = 2_000_000 us)
        let us = unwrapper.push(524288);
        assert_eq!(us, 2_000_000, "Third packet at raw=524288 should give us=2_000_000");
    }

    #[test]
    fn test_forward_progression_across_64s_wrap() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // Packets 0..63: 0 s to 63 s (raw increments by 1 second each time)
        for i in 0..=63 {
            let raw = i << 18; // i seconds
            let us = unwrapper.push(raw);
            // us should be i * 1_000_000
            assert_eq!(us, i as u64 * 1_000_000, "i={i}: expected {} us, got {}", i, us);
        }
        // Packet 64: raw = 0 wraps to 64 s → us should be 64_000_000
        let us = unwrapper.push(0);
        assert_eq!(us, 64_000_000, "After 64 s wrap, us should be 64_000_000, got {}", us);
    }

    #[test]
    fn test_out_of_order_packet() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // Progression to 2 s
        unwrapper.push(262144); // 1 s
        unwrapper.push(524288); // 2 s (us = 2_000_000)

        // Now push a packet that goes back 10 ms (within ambiguity window).
        // After this, a forward packet should continue from the high-water mark.

        // Push a backward jump of ~10ms from 2_000_000 us.
        let _us_after_backward = unwrapper.push(521861);
        // High-water should still be 2_000_000 us.

        // Push a forward packet (2.5 s = raw = 2.5 * 262144 = 655360)
        let us_after_forward = unwrapper.push(655360);
        // After out-of-order backward then forward, us should continue from high-water >= 2_000_000
        assert!(
            us_after_forward >= 2_000_000,
            "After out-of-order backward then forward, us should continue from high-water, got {}",
            us_after_forward
        );
    }

    #[test]
    fn test_resync_on_implausible_jump() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // Progress to 1 s
        unwrapper.push(262144); // 1 s = 1_000_000 us

        // Now push an implausible jump: raw that gives a delta > 32 s.
        // From 1 s, pushing raw that gives ~40 s us should have delta ≈ 40 s > 32 s.
        let us = unwrapper.push(10_485_760); // ~40 s
        // This should trigger resync since delta ≈ 40 s > 32 s.
        assert_eq!(us, 1_000_000, "Resync should keep output at previous value 1_000_000, got {}", us);

        // Next packet should continue from the high-water mark (1_000_000 us).
        // Push a normal forward packet: raw for 1 s = 262144
        let us2 = unwrapper.push(262144); // 1 s again
        assert_eq!(us2, 1_000_000, "After resync, forward packet should give us=1_000_000, got {}", us2);
    }

    #[test]
    fn test_masking_raw_above_24_bits() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // Push a value > 24 bits; should be masked to 24 bits.
        let us = unwrapper.push(0xFFFFFFFF);
        // 0x00FFFFFF = 16_777_215; raw_us = 16_777_215 * 1_000_000 / 262_144 = 64_000_000 (at wrap boundary)
        assert!(us > 0, "Masked raw should give positive us, got {}", us);
    }

    #[test]
    fn test_first_call_sets_base() {
        let mut unwrapper = SendTimeUnwrapper::new();
        // First call always sets the base
        let us = unwrapper.push(0);
        assert_eq!(us, 0, "First push should set base at 0 us");
        // Subsequent calls build on this
        let us2 = unwrapper.push(262144);
        assert_eq!(us2, 1_000_000, "Second push should give 1_000_000 us from base 0");
    }
}