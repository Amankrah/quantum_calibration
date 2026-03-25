use std::time::Instant;

use crate::calibration::{CalibrationReport, MockTransmonSystem};
use crate::schedule::{calibration_due, CalibrationPolicy, RecalibrationCause};

/// Decides when automated calibration should run (periodic vs post chip-swap).
///
/// A production stack would extend this with: priority queues, interleaved micro-cals,
/// and admission control so user jobs see minimal latency while drift stays bounded.
#[derive(Debug)]
pub struct CalibrationController {
    policy: CalibrationPolicy,
    last_full_calibration: Option<Instant>,
    pending_chip_swap_recal: bool,
}

impl CalibrationController {
    pub fn new(policy: CalibrationPolicy) -> Self {
        Self {
            policy,
            last_full_calibration: None,
            pending_chip_swap_recal: false,
        }
    }

    /// Call after physical chip replacement; forces a full calibration before trusting results.
    pub fn notify_chip_swap(&mut self) {
        self.pending_chip_swap_recal = true;
    }

    pub fn recalibration_cause(&self, now: Instant) -> Option<RecalibrationCause> {
        calibration_due(
            self.last_full_calibration,
            now,
            self.pending_chip_swap_recal,
            &self.policy,
        )
    }

    /// Run the full suite on the mock machine and clear pending chip-swap flag.
    pub fn run_full_calibration_if_due(
        &mut self,
        now: Instant,
        machine: &mut MockTransmonSystem,
    ) -> Option<CalibrationReport> {
        if self.recalibration_cause(now).is_none() {
            return None;
        }
        let report = machine.run_full_calibration_suite();
        self.last_full_calibration = Some(now);
        self.pending_chip_swap_recal = false;
        Some(report)
    }
}
