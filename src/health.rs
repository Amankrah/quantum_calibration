//! Layer 5 — health monitor: thresholds, drift warnings, human escalation after repeated failure.

use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug)]
pub struct HealthThresholds {
    pub min_t1_ns: u64,
    pub max_freq_drift_ghz: f64,
    pub min_fidelity: f64,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            min_t1_ns: 30_000,
            max_freq_drift_ghz: 0.08,
            min_fidelity: 0.99,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Alert {
    DriftWarning { qubit: u8, detail: String },
    EscalateHuman { qubit: u8, reason: String },
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Alert::DriftWarning { qubit, detail } => {
                write!(f, "[DRIFT] qubit {}: {}", qubit, detail)
            }
            Alert::EscalateHuman { qubit, reason } => {
                write!(f, "[ESCALATE] qubit {}: {}", qubit, reason)
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct HealthMonitor {
    consecutive_failures: HashMap<u8, u32>,
    pub alerts: Vec<Alert>,
}

impl HealthMonitor {
    pub const ESCALATE_AFTER_FAILURES: u32 = 3;

    pub fn record_success(&mut self, qubit: u8) {
        self.consecutive_failures.insert(qubit, 0);
    }

    pub fn record_failure(&mut self, qubit: u8, reason: impl Into<String>) {
        let reason = reason.into();
        let c = self.consecutive_failures.entry(qubit).or_insert(0);
        *c = c.saturating_add(1);
        if *c >= Self::ESCALATE_AFTER_FAILURES {
            self.alerts.push(Alert::EscalateHuman {
                qubit,
                reason: format!(
                    "{} consecutive calibration failures: {}",
                    Self::ESCALATE_AFTER_FAILURES,
                    reason
                ),
            });
            *c = 0;
        }
    }

    pub fn warn_drift(&mut self, qubit: u8, detail: impl Into<String>) {
        self.alerts
            .push(Alert::DriftWarning {
                qubit,
                detail: detail.into(),
            });
    }

    pub fn take_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.alerts)
    }
}
