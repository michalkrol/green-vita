//! GOP/reference-chain tracker per §6.6 and §16.5 of the proposition.
//!
//! Tracks whether the picture reference chain is broken due to damaged AUs.
//! The chain is broken from the first damaged AU until a clean IDR arrives.
//! A damaged IDR does NOT repair the chain.
//!
//! Design decisions (documented per spec):
//! - Do NOT attempt bitstream `frame_num` parsing (it needs exp-golomb + SPS
//!   log2_max_frame_num; out of scope).
//! - Chain model: the reference chain is BROKEN from the first damaged AU until
//!   a CLEAN IDR arrives. Once broken, it stays broken for every subsequent AU
//!   observed until a clean IDR resets it.
//! - A damaged IDR (itself has holes/no verified start) does NOT repair the
//!   chain: chain stays broken.
//! - Droppable class = `nal_ref_idc == 0` (temporal-layer frames) per §6.6;
//!   everything else non-IDR is Reference; IDR is Keyframe.

use crate::pipeline::media::assembler::AuDescriptor;

/// Classification of a decoded frame per §6.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// IDR frame — random access point, can be displayed independently.
    Keyframe,
    /// Regular reference frame — part of the prediction chain.
    Reference,
    /// Droppable frame (nal_ref_idc == 0) — temporal layer, can be dropped
    /// without affecting reference chain integrity.
    Droppable,
}

/// Information about a frame's classification and chain state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    /// The frame's class: Keyframe (IDR), Reference, or Droppable.
    pub class: FrameClass,
    /// Whether the reference chain is broken (picture must not be trusted/
    /// presented). True when this AU or any AU since the last clean IDR was
    /// damaged.
    pub chain_broken: bool,
    /// Whether this AU is an IDR frame.
    pub is_idr: bool,
}

/// GOP tracker maintains the reference chain state across access units.
///
/// The chain is broken from the first damaged AU until a clean IDR arrives.
/// A damaged IDR does NOT repair the chain.
#[derive(Debug, Default)]
pub struct GopTracker {
    chain_broken: bool,
    frames_since_idr: u32,
    idr_count: u64,
    damaged_count: u64,
}

impl GopTracker {
    /// Create a new GOP tracker with clean chain state.
    pub fn new() -> Self {
        Self {
            chain_broken: false,
            frames_since_idr: 0,
            idr_count: 0,
            damaged_count: 0,
        }
    }

    /// Observe the next assembled AU; returns its classification and updates
    /// chain state.
    ///
    /// Semantics (per design decisions §6.6/§16.5):
    /// 1. Compute `damaged = !au.had_first_slice || au.holes > 0`.
    /// 2. If damaged → `damaged_count += 1`.
    /// 3. If `au.is_idr`:
    ///     - `idr_count += 1`
    ///     - `frames_since_idr = 0`
    ///     - `chain_broken = damaged` (a clean IDR repairs; a damaged IDR
    ///       leaves/sets broken)
    ///     - `class = Keyframe`
    /// 4. Else (non-IDR):
    ///     - `frames_since_idr += 1`
    ///     - if damaged → `chain_broken = true` (propagates; never cleared by
    ///       non-IDR)
    ///     - `class = if au.nal_ref_idc == 0 { Droppable } else { Reference }`
    /// 5. Returned `chain_broken` = state AFTER these updates.
    pub fn observe(&mut self, au: &AuDescriptor) -> FrameInfo {
        let damaged = !au.had_first_slice || au.holes > 0;
        if damaged {
            self.damaged_count += 1;
        }

        let cls = if au.is_idr {
            self.idr_count += 1;
            self.frames_since_idr = 0;
            FrameClass::Keyframe
        } else {
            self.frames_since_idr += 1;
            if au.nal_ref_idc == 0 {
                FrameClass::Droppable
            } else {
                FrameClass::Reference
            }
        };

        let new_chain_broken = if au.is_idr {
            // Clean IDR repairs; damaged IDR leaves/sets broken
            damaged
        } else {
            // If damaged → chain_broken = true (propagates; never cleared by
            // non-IDR). If not damaged, chain_broken stays as-is.
            damaged || self.chain_broken
        };

        self.chain_broken = new_chain_broken;

        FrameInfo {
            class: cls,
            is_idr: au.is_idr,
            chain_broken: new_chain_broken,
        }
    }

    /// Whether the reference chain is currently broken.
    pub fn chain_broken(&self) -> bool {
        self.chain_broken
    }

    /// Number of frames since the last clean IDR.
    pub fn frames_since_idr(&self) -> u32 {
        self.frames_since_idr
    }

    /// Number of IDR frames observed.
    pub fn idr_count(&self) -> u64 {
        self.idr_count
    }

    /// Number of damaged AUs observed.
    pub fn damaged_count(&self) -> u64 {
        self.damaged_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::media::assembler::AuDescriptor;

    /// Build a clean IDR AuDescriptor.
    fn idr(_damaged: bool, had_first_slice: bool) -> AuDescriptor {
        AuDescriptor {
            rtp_ts: 0,
            data: vec![],
            is_idr: true,
            nal_ref_idc: 0,
            had_first_slice,
            holes: 0,
            packet_count: 1,
            first_seq: 0,
            last_seq: 0,
        }
    }

    /// Build a non-IDR AuDescriptor.
    fn non_idr(damaged: bool, nal_ref_idc: u8, had_first_slice: bool) -> AuDescriptor {
        AuDescriptor {
            rtp_ts: 0,
            data: vec![],
            is_idr: false,
            nal_ref_idc,
            had_first_slice,
            holes: if damaged { 1 } else { 0 },
            packet_count: 1,
            first_seq: 0,
            last_seq: 0,
        }
    }

    #[test]
    fn test_clean_idr_resets() {
        let mut tracker = GopTracker::new();
        let au = idr(false, true); // clean IDR, had_first_slice=true
        let info = tracker.observe(&au);
        assert_eq!(info.class, FrameClass::Keyframe);
        assert!(!info.chain_broken, "Clean IDR should not have broken chain");
        assert_eq!(tracker.frames_since_idr(), 0);
        assert_eq!(tracker.idr_count(), 1);
        assert_eq!(tracker.damaged_count(), 0);
    }

    #[test]
    fn test_consecutive_clean_p_frames() {
        let mut tracker = GopTracker::new();
        // First a clean IDR
        let au1 = idr(false, true);
        tracker.observe(&au1);

        // Then two clean P-frames
        let au2 = non_idr(false, 1, true); // Reference, no damage
        let info2 = tracker.observe(&au2);
        assert_eq!(info2.class, FrameClass::Reference);
        assert!(!info2.chain_broken);
        assert_eq!(tracker.frames_since_idr(), 1);

        let au3 = non_idr(false, 1, true);
        let info3 = tracker.observe(&au3);
        assert_eq!(info3.class, FrameClass::Reference);
        assert!(!info3.chain_broken);
        assert_eq!(tracker.frames_since_idr(), 2);
    }

    #[test]
    fn test_nal_ref_idc_0_droppable_without_breaking_chain() {
        let mut tracker = GopTracker::new();
        // First a clean IDR
        let au1 = idr(false, true);
        tracker.observe(&au1);

        // Then a damaged-free AU with nal_ref_idc=0 → Droppable
        let au2 = non_idr(false, 0, true); // no damage, nal_ref_idc=0
        let info2 = tracker.observe(&au2);
        assert_eq!(info2.class, FrameClass::Droppable);
        assert!(!info2.chain_broken, "Droppable without damage should not break chain");
        assert_eq!(tracker.frames_since_idr(), 1);
    }

    #[test]
    fn test_damaged_au_chain_broken_for_that_and_next_clean_p() {
        let mut tracker = GopTracker::new();
        // Clean IDR first
        let au1 = idr(false, true);
        tracker.observe(&au1);

        // Damaged AU (holes=1)
        let au2 = non_idr(true, 1, true); // damaged, nal_ref_idc=1 → Reference
        let info2 = tracker.observe(&au2);
        assert_eq!(info2.class, FrameClass::Reference);
        assert!(info2.chain_broken, "Damaged AU should have chain_broken=true");
        assert!(tracker.chain_broken(), "Tracker chain_broken should be true after damage");
        assert_eq!(tracker.damaged_count(), 1);

        // Next clean P-frame: chain_broken stays true (not damaged, so stays as-is)
        let au3 = non_idr(false, 1, true); // clean, nal_ref_idc=1 → Reference
        let info3 = tracker.observe(&au3);
        assert_eq!(info3.class, FrameClass::Reference);
        assert!(info3.chain_broken, "Next clean P-frame after damage should have chain_broken=true");
        assert!(tracker.chain_broken(), "Tracker should still be broken");
        assert_eq!(tracker.frames_since_idr(), 2, "frames_since_idr should be 2 after damaged AU + clean P-frame");
    }

    #[test]
    fn test_clean_idr_after_damage_repairs() {
        let mut tracker = GopTracker::new();

        // Damaged AU breaks the chain
        let au1 = non_idr(true, 1, true); // damaged
        let info1 = tracker.observe(&au1);
        assert!(info1.chain_broken, "Damaged AU should break chain");
        assert!(tracker.chain_broken(), "Tracker chain_broken should be true");

        // Clean IDR repairs the chain
        let au2 = idr(false, true); // clean IDR
        let info2 = tracker.observe(&au2);
        assert_eq!(info2.class, FrameClass::Keyframe);
        assert!(!info2.chain_broken, "Clean IDR should repair the chain");
        assert!(!tracker.chain_broken(), "Tracker chain_broken should be false after clean IDR");
        assert_eq!(tracker.frames_since_idr(), 0);
        assert_eq!(tracker.idr_count(), 1);
    }

    #[test]
    fn test_damaged_idr_chain_stays_broken() {
        let mut tracker = GopTracker::new();

        // Damaged IDR (had_first_slice=false, is_idr=true) does NOT repair
        let au1 = idr(false, false); // damaged IDR, had_first_slice=false
        let info1 = tracker.observe(&au1);
        assert_eq!(info1.class, FrameClass::Keyframe);
        assert!(info1.chain_broken, "Damaged IDR should keep chain broken");
        assert!(tracker.chain_broken(), "Tracker should stay broken after damaged IDR");
        assert_eq!(tracker.damaged_count(), 1);
        assert_eq!(tracker.idr_count(), 1);

        // Next AU should still have chain_broken=true
        let au2 = non_idr(false, 1, true);
        let info2 = tracker.observe(&au2);
        assert!(!info2.is_idr);
        assert!(info2.chain_broken, "Next AU after damaged IDR should have chain_broken=true");
    }

    #[test]
    fn test_counters_idr_count_damaged_count() {
        let mut tracker = GopTracker::new();

        // Clean IDR
        let au1 = idr(false, true);
        tracker.observe(&au1);
        assert_eq!(tracker.idr_count(), 1);
        assert_eq!(tracker.damaged_count(), 0);

        // Damaged non-IDR
        let au2 = non_idr(true, 1, true);
        tracker.observe(&au2);
        assert_eq!(tracker.idr_count(), 1);
        assert_eq!(tracker.damaged_count(), 1);

        // Another clean IDR
        let au3 = idr(false, true);
        tracker.observe(&au3);
        assert_eq!(tracker.idr_count(), 2);
        assert_eq!(tracker.damaged_count(), 1);

        // Damaged IDR (counts toward damaged_count, idr_count still increments)
        let au4 = idr(false, false); // damaged IDR
        tracker.observe(&au4);
        assert_eq!(tracker.idr_count(), 3);
        assert_eq!(tracker.damaged_count(), 2);
    }

    #[test]
    fn test_frames_since_idr_resets_at_each_idr() {
        let mut tracker = GopTracker::new();

        // Clean IDR
        let au1 = idr(false, true);
        tracker.observe(&au1);
        assert_eq!(tracker.frames_since_idr(), 0);

        // Two clean P-frames
        let au2 = non_idr(false, 1, true);
        tracker.observe(&au2);
        assert_eq!(tracker.frames_since_idr(), 1);

        let au3 = non_idr(false, 1, true);
        tracker.observe(&au3);
        assert_eq!(tracker.frames_since_idr(), 2);

        // Another clean IDR resets frames_since_idr
        let au4 = idr(false, true);
        tracker.observe(&au4);
        assert_eq!(tracker.frames_since_idr(), 0);

        // Now one more P-frame
        let au5 = non_idr(false, 1, true);
        tracker.observe(&au5);
        assert_eq!(tracker.frames_since_idr(), 1);
    }
}