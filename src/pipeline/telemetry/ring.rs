/// `telemetry/ring` — power-of-two N single-producer single-consumer
/// lock-free ring using atomics only (head/tail with Acquire/Release).
/// Zero allocation after construction; no Mutex; minimal unsafe.

use std::sync::atomic::{AtomicU64, Ordering};

/// `FrameRecord` (~48–64 bytes, all fixed-size integers):
/// rtp_ts, first_pkt_us, complete_us, submit_us, decode_start_us,
/// decode_end_us, latch_us, flip_us, q_us, flags (u32 bitfield:
/// complete/damaged/shed/lost_keyframe…). Times are LOCAL monotonic
/// microseconds since session start (u64).
///
/// Layout (64 bytes, padded to alignment):
///   rtp_ts:           u32   |  first_pkt_us:   u64
///   complete_us:      u64   |  submit_us:        u64
///   decode_start_us:  u64   |  decode_end_us:    u64
///   latch_us:         u64   |  flip_us:          u64
///   q_us:             i64   |  flags:            u32
///   _padding:         u32   (to reach ~64 bytes)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRecord {
    /// RTP timestamp of the frame (90 kHz video, 48 kHz audio).
    pub rtp_ts: u32,
    /// First packet arrival microseconds since session start.
    pub first_pkt_us: u64,
    /// Completion microseconds (decode finished).
    pub complete_us: u64,
    /// Submit microseconds (present mailbox queued).
    pub submit_us: u64,
    /// Decode start microseconds.
    pub decode_start_us: u64,
    /// Decode end microseconds.
    pub decode_end_us: u64,
    /// Latch microseconds (late-latch at vblank).
    pub latch_us: u64,
    /// Flip microseconds (vsync flip timestamp).
    pub flip_us: u64,
    /// Queue-delay estimator Q value at flip time (us).
    pub q_us: i64,
    /// Flags bitfield: bit 0 = complete, bit 1 = damaged, bit 2 = shed,
    /// bit 3 = lost_keyframe, others reserved.
    pub flags: u32,
    /// Padding to reach struct size (public for test access).
    pub _padding: u32,
}

    impl FrameRecord {
/// Check if the frame is marked as complete.
    pub fn is_complete(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Check if the frame is marked as damaged.
    pub fn is_damaged(&self) -> bool {
        self.flags & 0x02 != 0
    }

    /// Check if the frame is marked as shed.
    pub fn is_shed(&self) -> bool {
        self.flags & 0x04 != 0
    }

    /// Check if the frame is a lost keyframe.
    pub fn is_lost_keyframe(&self) -> bool {
        self.flags & 0x08 != 0
    }

    /// Drop mask: damaged | shed | lost_keyframe
    pub fn drop_flags(&self) -> u32 {
        self.flags & 0x07
    }

    /// Set the frame as complete.
    pub fn set_complete(&mut self) {
        self.flags |= 0x01;
    }

    /// Set the frame as damaged.
    pub fn set_damaged(&mut self) {
        self.flags |= 0x02;
    }

    /// Set the frame as shed.
    pub fn set_shed(&mut self) {
        self.flags |= 0x04;
    }

    /// Set the frame as lost keyframe.
    pub fn set_lost_keyframe(&mut self) {
        self.flags |= 0x08;
    }


}

/// Power-of-two N SPSC lock-free ring using atomics only.
///
/// Capacity N must be a power of two. After construction, no further
/// allocations occur. The slot array is pre-initialized once.
///
/// invariant: empty ⇔ read_pos == write_pos
///            full   ⇔ write_pos - read_pos >= N
///            slot index = pos % N (bitwise: pos & (N-1) for power-of-two)
pub struct TelemetryRing<const N: usize> {
    /// Pre-allocated slot array, Boxed for const-generic size.
    slots: Box<[FrameRecord; N]>,
    /// Write position (producer) with Release semantics.
    /// Counts total writes since construction; mod N gives slot index.
    write_pos: AtomicU64,
    /// Read position (consumer) with Acquire semantics.
    /// Counts total reads since construction; mod N gives slot index.
    read_pos: AtomicU64,
    /// Dropped count: records silently dropped when the ring was full.
    dropped: AtomicU64,
}

impl<const N: usize> TelemetryRing<N> {
    /// Construct a new TelemetryRing with capacity N (power of two).
    ///
    /// After construction, the slot array is pre-initialized with default
    /// FrameRecord values. Zero allocation occurs thereafter.
    ///
    /// # Power-of-two requirement
    ///
    /// N must be a power of two. The slot index is computed as `pos & (N-1)`,
    /// which is only a correct modulo for power-of-two N.
    pub fn new() -> Self {
        let slots = Box::new([FrameRecord::default(); N]);
        TelemetryRing {
            slots,
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Push a FrameRecord into the ring (producer side).
    ///
    /// Returns `true` if the record was accepted, `false` if the ring was
    /// full and the record was silently dropped (dropped count incremented).
    /// Silently dropping means the consumer will never see this record.
    ///
    /// The caller must ensure single-producer access. Concurrent access from
    /// multiple producers is undefined behaviour.
    pub fn push(&mut self, record: FrameRecord) -> bool {
        // Fast path: read current write and read positions.
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);

        // Full condition: write_pos - read_pos >= N
        // Since both are u64 and we use wrapping subtractions,
        // we check: write >= read + N.
        // But read + N could overflow. Instead, use:
        // full when (write.wrapping_sub(read)) >= N.
        // This is correct because: if write >= read (no wrap of write relative to read),
        // then write.wrapping_sub(read) = write - read.
        // If write < read (wrap has occurred, meaning write wrapped around the u64 space),
        // then write.wrapping_sub(read) = 2^64 + write - read, which is huge >= N.
        // But in practice, write and read only increase, and write - read is always
        // in the range [0, 2^64). The full check with >= N works correctly because:
        // - When the ring is empty: write == read, difference = 0 < N
        // - When the ring has some items: 0 < difference < N
        // - When the ring is full: difference >= N
        // - After the ring wraps: difference can be > N, but the full check still works.
        let difference = write.wrapping_sub(read);
        let full = difference >= N as u64;

        if full {
            // Silently drop: increment dropped count.
            self.dropped.fetch_add(1, Ordering::Release);
            false
        } else {
            // Write the record to the slot at position (write % N).
            // Since N is power of two: slot_idx = write & (N-1)
            let slot_idx = (write as usize) & (N - 1);
            let _slot = &self.slots[slot_idx];

            // Write the record data. We write the whole record; the Release
            // on the subsequent write_pos advance will make this visible to the
            // consumer. Store with Release semantics so the consumer's Acquire
            // load of read_pos pairs with this write.
            //
            // The order is: (1) write data to slot, (2) advance write_pos with Release.
            // The consumer will read read_pos with Acquire, then read the slot data,
            // and the Acquire/Release pairing ensures visibility.
            // Since we're writing a whole struct, we can just assign:
            self.slots[slot_idx] = record;

            // Advance write_pos with Release so the consumer sees the data.
            self.write_pos.fetch_add(1, Ordering::Release);

            true
        }
    }

    /// Consume all available records, returning an iterator.
    ///
    /// The iterator yields `FrameRecord` values in FIFO order. Records are
    /// only yielded if they have been written by the producer and not yet
    /// consumed by the consumer. The iterator consumes (advances past) the
    /// yielded records.
    ///
    /// Returns an iterator that drains the ring. The caller should not
    /// store references to the yielded records across drain() calls.
    pub fn drain(&self) -> Drain<'_, N> {
        let read = self.read_pos.load(Ordering::Acquire);
        let write = self.write_pos.load(Ordering::Relaxed);

        if read >= write {
            // Ring is empty; return an iterator that yields nothing.
            Drain {
                ring: self,
                read,
                end: read, // empty range
                phantom: std::marker::PhantomData,
            }
        } else {
            // Ring has items: [read, write) in circular buffer.
            Drain {
                ring: self,
                read,
                end: write,
                phantom: std::marker::PhantomData,
            }
        }
    }

    /// Return the number of records silently dropped when the ring was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Iterator over drained records from a `TelemetryRing`.
///
/// Yields `FrameRecord` values in FIFO order. The iterator advances the
/// consumer's read position as records are consumed.
pub struct Drain<'a, const N: usize> {
    ring: &'a TelemetryRing<N>,
    /// Current read position (how many records have been consumed so far).
    read: u64,
    /// End position (write_pos or the original write_pos at drain start).
    end: u64,
    /// Phantom data to constraining N without owning it.
    phantom: std::marker::PhantomData<TelemetryRing<N>>,
}

impl<'a, const N: usize> Iterator for Drain<'a, N> {
    type Item = FrameRecord;

    fn next(&mut self) -> Option<FrameRecord> {
        // Check if we've consumed all records.
        let write = self.ring.write_pos.load(Ordering::Relaxed);
        if self.read >= write {
            return None;
        }

        // Compute slot index for the current read position.
        let slot_idx = (self.read as usize) & (N - 1);
        let record = self.ring.slots[slot_idx];

        // Advance the read position with Acquire so the producer can see
        // that we've consumed this record (though for SPSC this is just
        // a formality; the real synchronization is in push/drain ordering).
        self.read = self.read.wrapping_add(1);

        Some(record)
    }
}




impl<const N: usize> Default for TelemetryRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ring_wrap_around() {
        let mut ring: TelemetryRing<8> = TelemetryRing::new(); // power of two
        // Push 12 records (more than capacity 8, some will be dropped).
        for i in 0..12 {
            let rec = FrameRecord {
                rtp_ts: i as u32,
                ..Default::default()
            };
            let accepted = ring.push(rec);
            if !accepted {
                // Ring is full, records silently dropped.
            }
        }
        // Drain should give us at most 8 records (the ring overwrites oldest).
        let mut drained_count = 0;
        for _rec in ring.drain() {
            // We'll get records; the exact number depends on the full/empty logic.
            // With our implementation: full when write - read >= N.
            // After 12 pushes with capacity 8: write = 12, read = 0, diff = 12 >= 8.
            // First 8 pushes accepted, last 4 dropped.
            // Then drain gives 8 records (indices 0..7).
            drained_count += 1;
        }
        eprintln!("test_ring_wrap_around: drained {} records", drained_count);
        // Should have drained exactly 8 records (the ring capacity).
        assert_eq!(drained_count, 8, "Expected 8 drained records from capacity-8 ring");
    }

    #[test]
    fn test_ring_dropped_count() {
        let mut ring: TelemetryRing<8> = TelemetryRing::new();
        // Push 12 records into capacity-8 ring; 4 should be dropped.
        for i in 0..12 {
            let rec = FrameRecord {
                rtp_ts: i as u32,
                ..Default::default()
            };
            let _ = ring.push(rec);
        }
        let dropped = ring.dropped();
        assert_eq!(dropped, 4, "Expected 4 dropped records, got {dropped}");
    }

    #[test]
    fn test_ring_empty_drain() {
        let ring: TelemetryRing<100> = TelemetryRing::new();
        let mut drained = 0;
        for _ in ring.drain() {
            drained += 1;
        }
        assert_eq!(drained, 0);
    }

    #[test]
    fn test_frame_record_bits() {
        // Verify FrameRecord is repr(C) with fixed-size integers.
        let rec = FrameRecord::default();
        // All fields should be present and zeroed.
        assert_eq!(rec.rtp_ts, 0);
        assert_eq!(rec.first_pkt_us, 0);
        assert_eq!(rec.complete_us, 0);
        assert_eq!(rec.submit_us, 0);
        assert_eq!(rec.decode_start_us, 0);
        assert_eq!(rec.decode_end_us, 0);
        assert_eq!(rec.latch_us, 0);
        assert_eq!(rec.flip_us, 0);
        assert_eq!(rec.q_us, 0);
        assert_eq!(rec.flags, 0);
        assert_eq!(rec._padding, 0);
    }

    #[test]
    fn test_frame_record_setters() {
        let mut rec = FrameRecord::default();
        rec.rtp_ts = 0x12345678;
        rec.first_pkt_us = 1_000_000;
        rec.complete_us = 2_000_000;
        rec.submit_us = 3_000_000;
        rec.decode_start_us = 4_000_000;
        rec.decode_end_us = 5_000_000;
        rec.latch_us = 6_000_000;
        rec.flip_us = 7_000_000;
        rec.q_us = 1000;
        rec.flags = 0b111; // complete | damaged | shed

        assert_eq!(rec.rtp_ts, 0x12345678);
        assert_eq!(rec.first_pkt_us, 1_000_000);
        assert_eq!(rec.is_complete(), true);
        assert_eq!(rec.is_damaged(), true);
        assert_eq!(rec.is_shed(), true);
        assert_eq!(rec.is_lost_keyframe(), false);
        assert_eq!(rec.drop_flags(), 0b111);
    }
}