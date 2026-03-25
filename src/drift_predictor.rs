//! AI-assisted scheduling: predict when each qubit will need recalibration from frequency history,
//! complementing fixed-interval policy (`schedule` module) with data-driven urgency.

/// One historical data point: a frequency measurement at a point in time.
#[derive(Debug, Clone)]
pub struct FrequencyObservation {
    /// Seconds since epoch or a monotonic lab clock origin.
    pub timestamp_secs: f64,
    pub frequency_ghz: f64,
}

/// Per-qubit drift summary from simple linear regression on history (explainable baseline).
#[derive(Debug, Clone)]
pub struct DriftModel {
    pub qubit_id: u8,
    /// Drift rate in GHz per second (least-squares slope).
    pub drift_rate_ghz_per_sec: f64,
    pub last_frequency_ghz: f64,
    pub last_calibrated_secs: f64,
}

impl DriftModel {
    /// Train from historical observations using linear regression on frequency vs time.
    pub fn train(qubit_id: u8, history: &[FrequencyObservation]) -> Option<Self> {
        if history.len() < 2 {
            return None;
        }

        let n = history.len() as f64;
        let sum_t: f64 = history.iter().map(|o| o.timestamp_secs).sum();
        let sum_f: f64 = history.iter().map(|o| o.frequency_ghz).sum();
        let sum_tf: f64 = history
            .iter()
            .map(|o| o.timestamp_secs * o.frequency_ghz)
            .sum();
        let sum_t2: f64 = history.iter().map(|o| o.timestamp_secs.powi(2)).sum();

        let denom = n * sum_t2 - sum_t.powi(2);
        if denom.abs() < 1e-18 {
            return None;
        }

        let drift_rate = (n * sum_tf - sum_t * sum_f) / denom;
        let last = history.last()?;

        Some(DriftModel {
            qubit_id,
            drift_rate_ghz_per_sec: drift_rate,
            last_frequency_ghz: last.frequency_ghz,
            last_calibrated_secs: last.timestamp_secs,
        })
    }

    /// Rough seconds until linear drift consumes remaining tolerance (point estimate).
    pub fn seconds_until_recalibration_needed(
        &self,
        tolerance_ghz: f64,
        now_secs: f64,
    ) -> Option<f64> {
        let abs_drift = self.drift_rate_ghz_per_sec.abs();
        if abs_drift < 1e-15 {
            return None;
        }

        let elapsed = now_secs - self.last_calibrated_secs;
        let already_drifted = abs_drift * elapsed;
        let remaining_tolerance = tolerance_ghz - already_drifted;

        if remaining_tolerance <= 0.0 {
            return Some(0.0);
        }

        Some(remaining_tolerance / abs_drift)
    }

    /// Summary string for logs or downstream LLM diagnostics.
    pub fn summary(&self, tolerance_ghz: f64, now_secs: f64) -> String {
        match self.seconds_until_recalibration_needed(tolerance_ghz, now_secs) {
            None => format!(
                "Qubit {}: stable — drift rate negligible ({:.2e} GHz/s)",
                self.qubit_id, self.drift_rate_ghz_per_sec
            ),
            Some(0.0) => format!(
                "Qubit {}: OVERDUE for calibration — drift exceeded tolerance",
                self.qubit_id
            ),
            Some(secs) => format!(
                "Qubit {}: recalibration needed in {:.0}s ({:.1} min) — drift rate {:.2e} GHz/s",
                self.qubit_id,
                secs,
                secs / 60.0,
                self.drift_rate_ghz_per_sec
            ),
        }
    }
}

/// Fleet-level rolling history and urgency ranking.
pub struct PredictiveDriftDetector {
    pub tolerance_ghz: f64,
    histories: std::collections::HashMap<u8, Vec<FrequencyObservation>>,
}

impl PredictiveDriftDetector {
    pub fn new(tolerance_ghz: f64) -> Self {
        Self {
            tolerance_ghz,
            histories: std::collections::HashMap::new(),
        }
    }

    pub fn record_observation(&mut self, qubit_id: u8, frequency_ghz: f64, timestamp_secs: f64) {
        let history = self.histories.entry(qubit_id).or_default();
        history.push(FrequencyObservation {
            timestamp_secs,
            frequency_ghz,
        });
        const MAX_WINDOW: usize = 20;
        if history.len() > MAX_WINDOW {
            history.remove(0);
        }
    }

    /// Qubits predicted to need recal within `horizon_secs`, soonest first.
    pub fn qubits_due_within(&self, horizon_secs: f64, now_secs: f64) -> Vec<(u8, f64)> {
        let mut due: Vec<(u8, f64)> = self
            .histories
            .iter()
            .filter_map(|(&qid, history)| {
                let model = DriftModel::train(qid, history)?;
                let secs = model.seconds_until_recalibration_needed(self.tolerance_ghz, now_secs)?;
                if secs <= horizon_secs {
                    Some((qid, secs))
                } else {
                    None
                }
            })
            .collect();

        due.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        due
    }

    pub fn print_predictions(&self, now_secs: f64) {
        println!("\nAI drift predictions (linear regression on history):");
        for (&qid, history) in &self.histories {
            if let Some(model) = DriftModel::train(qid, history) {
                println!("  {}", model.summary(self.tolerance_ghz, now_secs));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicts_drift_correctly() {
        let history: Vec<FrequencyObservation> = (0..10)
            .map(|i| FrequencyObservation {
                timestamp_secs: i as f64 * 100.0,
                frequency_ghz: 5.0 + i as f64 * 0.001,
            })
            .collect();

        let model = DriftModel::train(0, &history).unwrap();
        assert!((model.drift_rate_ghz_per_sec - 0.00001).abs() < 1e-7);

        let secs = model
            .seconds_until_recalibration_needed(0.08, 900.0)
            .unwrap();
        assert!(secs > 0.0);
    }

    #[test]
    fn detector_flags_urgent_qubits() {
        let mut detector = PredictiveDriftDetector::new(0.08);

        for i in 0..10 {
            detector.record_observation(0, 5.0 + i as f64 * 0.05, i as f64 * 100.0);
        }
        for i in 0..10 {
            detector.record_observation(1, 5.2 + i as f64 * 0.00001, i as f64 * 100.0);
        }

        let due = detector.qubits_due_within(500.0, 900.0);
        assert!(!due.is_empty());
        assert_eq!(due[0].0, 0);
    }
}
