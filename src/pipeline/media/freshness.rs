/// Freshness controller per §6.9, §5.2, D6 of the rendering pipeline proposition.
///
/// Decides whether to admit, shed droppable, or shed-to-IDR the next AU the
/// decoder consumes.  Pure decision state machine — caller executes the action.
///
/// Why-comments cite the specification sections that justify each branch.
use crate::pipeline::media::gop::{FrameClass, FrameInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Action to take for the current AU.
pub enum FreshnessAction {
    /// Decode/present this AU now.
    Admit,
    /// Shed this AU only: it is droppable-class (temporal layer). Chain intact.
    ShedDroppable,
    /// Shed this AU AND every subsequent AU until the next clean IDR
    /// (skip-to-IDR, §6.9 rule 3 / D6).  `request_pli` asks the caller to fire
    /// a keyframe request; the caller OWNS rate-limiting/storm guards
    /// (they live in session.rs: 300 ms cooldown, 3-per-2 s guard).
    ShedToIdr {
        request_pli: bool,
    },
}

/// Controls freshness-based shedding of access units.
///
/// Single-writer: call from one thread.
/// Default budget: 60 ms of AU age (§6.9).
#[derive(Debug)]
pub struct FreshnessController {
    budget_us: u64,
    admitted: u64,
    shed_droppable: u64,
    shed_to_idr: u64,
}

impl FreshnessController {
    /// Default budget: 60 ms of AU age (§6.9).
    pub fn new() -> Self {
        Self {
            budget_us: 60_000,
            admitted: 0,
            shed_droppable: 0,
            shed_to_idr: 0,
        }
    }

    /// Create with a custom budget (µs).
    pub fn with_budget_us(budget_us: u64) -> Self {
        Self {
            budget_us,
            admitted: 0,
            shed_droppable: 0,
            shed_to_idr: 0,
        }
    }

    /// Set the age budget at runtime (A/B test knob).
    pub fn set_budget_us(&mut self, budget_us: u64) {
        self.budget_us = budget_us;
    }

    /// Decide what to do with the AU the decoder is about to consume.
    ///
    /// Order (exact, per spec):
    ///   1. `chain_broken && !is_idr` → ShedToIdr { request_pli: true }
    ///      (§5.2: freeze on last good picture, never smear pixels).
    ///   2. `age_us > budget_us`:
    ///        Droppable → ShedDroppable  (cheapest, breaks nothing).
    ///        Keyframe  → Admit           (IDR is the resume point; shedding would freeze forever).
    ///        Reference → ShedToIdr { request_pli: true }  (queued GOP tail over budget).
    ///   3. Else → Admit.
    pub fn decide(&mut self, age_us: u64, info: &FrameInfo) -> FreshnessAction {
        // [Rule 1] — §5.2: a frame whose reference chain is known broken is garbage.
        // Freeze on the last good picture; never present pixels from a broken chain.
        if info.chain_broken && !info.is_idr {
            self.shed_to_idr += 1;
            return FreshnessAction::ShedToIdr { request_pli: true };
        }

        // [Rule 2] — age exceeds budget
        if age_us > self.budget_us {
            match info.class {
                FrameClass::Droppable => {
                    self.shed_droppable += 1;
                    FreshnessAction::ShedDroppable
                }
                FrameClass::Keyframe => {
                    // The IDR IS the resume point of skip-to-IDR.
                    // Shedding the only picture that can repair the chain would freeze forever.
                    self.admitted += 1;
                    FreshnessAction::Admit
                }
                FrameClass::Reference => {
                    self.shed_to_idr += 1;
                    FreshnessAction::ShedToIdr { request_pli: true }
                }
            }
        } else {
            // [Rule 3] — within budget, admit
            self.admitted += 1;
            FreshnessAction::Admit
        }
    }

    /// Number of AUs admitted (decoded/presented).
    pub fn admitted(&self) -> u64 {
        self.admitted
    }

    /// Number of droppable-class AUs shed.
    pub fn shed_droppable(&self) -> u64 {
        self.shed_droppable
    }

    /// Number of AUs shed-to-IDR (including chain-broken and over-budget Reference paths).
    pub fn shed_to_idr(&self) -> u64 {
        self.shed_to_idr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::media::gop::FrameInfo;

    // Helper: build a FrameInfo for testing
    fn frame_info(class: FrameClass, is_idr: bool, chain_broken: bool) -> FrameInfo {
        FrameInfo {
            class,
            is_idr,
            chain_broken,
        }
    }

    #[test]
    fn test_under_budget_reference_admits() {
        let mut controller = FreshnessController::new();
        let info = frame_info(FrameClass::Reference, false, false);
        let action = controller.decide(30_000, &info); // 30ms < 60ms budget
        assert_eq!(action, FreshnessAction::Admit);
        assert_eq!(controller.admitted(), 1);
    }

    #[test]
    fn test_over_budget_reference_shed_to_idr() {
        let mut controller = FreshnessController::new();
        let info = frame_info(FrameClass::Reference, false, false);
        let action = controller.decide(70_000, &info); // 70ms > 60ms budget
        assert_eq!(action, FreshnessAction::ShedToIdr { request_pli: true });
        assert_eq!(controller.shed_to_idr(), 1);
    }

    #[test]
    fn test_over_budget_droppable_shed_droppable() {
        let mut controller = FreshnessController::new();
        let info = frame_info(FrameClass::Droppable, false, false);
        let action = controller.decide(70_000, &info); // 70ms > 60ms budget
        assert_eq!(action, FreshnessAction::ShedDroppable);
        assert_eq!(controller.shed_droppable(), 1);
        // request_pli should NOT fire for Droppable
        // (we verify by checking the action variant)
    }

    #[test]
    fn test_over_budget_keyframe_admit() {
        let mut controller = FreshnessController::new();
        let info = frame_info(FrameClass::Keyframe, true, false);
        let action = controller.decide(70_000, &info); // 70ms > 60ms budget, but Keyframe
        assert_eq!(action, FreshnessAction::Admit);
        assert_eq!(controller.admitted(), 1);
    }

    #[test]
    fn test_chain_broken_non_idr_under_budget_shed_to_idr() {
        let mut controller = FreshnessController::new();
        let info = frame_info(FrameClass::Reference, false, true); // chain_broken=true, not IDR
        let action = controller.decide(30_000, &info); // 30ms < 60ms budget, but chain broken
        assert_eq!(action, FreshnessAction::ShedToIdr { request_pli: true });
        assert_eq!(controller.shed_to_idr(), 1);
    }

    #[test]
    fn test_clean_idr_after_damage_subsequent_admits() {
        let mut controller = FreshnessController::new();
        // First: chain-broken non-IDR over budget → ShedToIdr
        let info1 = frame_info(FrameClass::Reference, false, true);
        let _ = controller.decide(70_000, &info1);

        // Now a clean IDR arrives (chain breaks reset — but we're testing the controller
        // just gets a clean IDR FrameInfo). Under-budget clean IDR admits.
        let info2 = frame_info(FrameClass::Keyframe, true, false);
        let action = controller.decide(30_000, &info2);
        assert_eq!(action, FreshnessAction::Admit);
        assert_eq!(controller.admitted(), 1);
    }

    #[test]
    fn test_budget_enforced_exactly() {
        let mut controller = FreshnessController::new();
        // age == budget exactly → Admit (not > budget)
        let info = frame_info(FrameClass::Reference, false, false);
        let action = controller.decide(60_000, &info);
        assert_eq!(action, FreshnessAction::Admit);
        assert_eq!(controller.admitted(), 1);

        // age == budget + 1 → shed for Reference
        let action2 = controller.decide(60_001, &info);
        assert_eq!(action2, FreshnessAction::ShedToIdr { request_pli: true });
        assert_eq!(controller.shed_to_idr(), 1);
    }

    #[test]
    fn test_set_budget_us_changes_enforcement() {
        let mut controller = FreshnessController::new();
        // Default budget is 60_000; set to 20_000
        controller.set_budget_us(20_000);
        let info = frame_info(FrameClass::Reference, false, false);
        // 40_000 > 20_000 budget → ShedToIdr
        let action = controller.decide(40_000, &info);
        assert_eq!(action, FreshnessAction::ShedToIdr { request_pli: true });
        assert_eq!(controller.shed_to_idr(), 1);

        // Now reset to 80_000; 40_000 < 80_000 → Admit
        controller.set_budget_us(80_000);
        let action2 = controller.decide(40_000, &info);
        assert_eq!(action2, FreshnessAction::Admit);
        assert_eq!(controller.admitted(), 1);
    }

    #[test]
    fn test_counters_track_all_three_paths() {
        let mut controller = FreshnessController::new();

        // Path 1: Admit (under-budget Reference)
        let info1 = frame_info(FrameClass::Reference, false, false);
        controller.decide(30_000, &info1);
        assert_eq!(controller.admitted(), 1);

        // Path 2: ShedDroppable (over-budget Droppable)
        let info2 = frame_info(FrameClass::Droppable, false, false);
        controller.decide(70_000, &info2);
        assert_eq!(controller.shed_droppable(), 1);

        // Path 3: ShedToIdr (over-budget Reference)
        let info3 = frame_info(FrameClass::Reference, false, false);
        controller.decide(70_000, &info3);
        assert_eq!(controller.shed_to_idr(), 1);

        // Verify total counts
        assert_eq!(controller.admitted() + controller.shed_droppable() + controller.shed_to_idr(), 3);
    }
}