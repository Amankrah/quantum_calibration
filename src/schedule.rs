use std::time::{Duration, Instant};

/// How often to refresh calibration when the machine is otherwise healthy.
#[derive(Clone, Copy, Debug)]
pub struct CalibrationPolicy {
    /// e.g. 24h in production; shrink in tests / demos.
    pub periodic_interval: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecalibrationCause {
    Periodic,
    ChipSwap,
}

/// Returns why calibration should run now, if at all.
pub fn calibration_due(
    last_success: Option<Instant>,
    now: Instant,
    pending_chip_swap: bool,
    policy: &CalibrationPolicy,
) -> Option<RecalibrationCause> {
    if pending_chip_swap {
        return Some(RecalibrationCause::ChipSwap);
    }
    match last_success {
        None => Some(RecalibrationCause::Periodic),
        Some(last) if now.duration_since(last) >= policy.periodic_interval => {
            Some(RecalibrationCause::Periodic)
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_swap_takes_priority_over_period() {
        let policy = CalibrationPolicy {
            periodic_interval: Duration::from_secs(3600),
        };
        let now = Instant::now();
        let last = now.checked_sub(Duration::from_secs(10));
        assert_eq!(
            calibration_due(last, now, true, &policy),
            Some(RecalibrationCause::ChipSwap)
        );
    }

    #[test]
    fn never_calibrated_triggers_periodic() {
        let policy = CalibrationPolicy {
            periodic_interval: Duration::from_secs(3600),
        };
        let now = Instant::now();
        assert_eq!(
            calibration_due(None, now, false, &policy),
            Some(RecalibrationCause::Periodic)
        );
    }

    #[test]
    fn recent_calibration_suppresses_periodic() {
        let policy = CalibrationPolicy {
            periodic_interval: Duration::from_secs(3600),
        };
        let now = Instant::now();
        let last = now.checked_sub(Duration::from_secs(10)).unwrap();
        assert_eq!(
            calibration_due(Some(last), now, false, &policy),
            None
        );
    }
}
