//! Model registry for frequency drift: pluggable `DriftForecastModel` trait, several built-in estimators,
//! and LOOCV-based comparison. Same trait boundary can host PyO3-backed Python models in production.

use crate::drift_predictor::FrequencyObservation;

// ─────────────────────────────────────────────
//  CORE TRAIT — extension point for Python / DL / RL
// ─────────────────────────────────────────────

/// Any frequency-drift forecast model implements this trait (Rust-native or bridged via PyO3).
pub trait DriftForecastModel: Send + Sync {
    fn name(&self) -> &'static str;

    fn fit(&mut self, history: &[FrequencyObservation]) -> Result<(), ModelError>;

    fn predict(&self, future_timestamp_secs: f64) -> Result<f64, ModelError>;

    /// Scan forward in time for first crossing of `tolerance_ghz` from `current_freq_ghz`.
    fn seconds_until_drift(
        &self,
        current_freq_ghz: f64,
        tolerance_ghz: f64,
        now_secs: f64,
        horizon_secs: f64,
    ) -> Option<f64> {
        const STEPS: usize = 100;
        let step_size = horizon_secs / STEPS as f64;

        for i in 0..=STEPS {
            let t = now_secs + i as f64 * step_size;
            if let Ok(predicted) = self.predict(t) {
                if (predicted - current_freq_ghz).abs() >= tolerance_ghz {
                    return Some((t - now_secs).max(0.0));
                }
            }
        }
        None
    }
}

#[derive(Debug)]
pub enum ModelError {
    InsufficientData { required: usize, got: usize },
    NotFitted,
    NumericalInstability,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::InsufficientData { required, got } => {
                write!(f, "Need {required} observations, got {got}")
            }
            ModelError::NotFitted => write!(f, "Model not fitted yet"),
            ModelError::NumericalInstability => write!(f, "Numerical instability in fit"),
        }
    }
}

impl std::error::Error for ModelError {}

// ─────────────────────────────────────────────
//  MODEL 1: LINEAR REGRESSION
// ─────────────────────────────────────────────

pub struct LinearRegressionModel {
    slope: f64,
    intercept: f64,
    fitted: bool,
}

impl Default for LinearRegressionModel {
    fn default() -> Self {
        Self {
            slope: 0.0,
            intercept: 0.0,
            fitted: false,
        }
    }
}

impl DriftForecastModel for LinearRegressionModel {
    fn name(&self) -> &'static str {
        "LinearRegression"
    }

    fn fit(&mut self, history: &[FrequencyObservation]) -> Result<(), ModelError> {
        if history.len() < 2 {
            return Err(ModelError::InsufficientData {
                required: 2,
                got: history.len(),
            });
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
            return Err(ModelError::NumericalInstability);
        }

        self.slope = (n * sum_tf - sum_t * sum_f) / denom;
        self.intercept = (sum_f - self.slope * sum_t) / n;
        self.fitted = true;
        Ok(())
    }

    fn predict(&self, t: f64) -> Result<f64, ModelError> {
        if !self.fitted {
            return Err(ModelError::NotFitted);
        }
        Ok(self.slope * t + self.intercept)
    }
}

// ─────────────────────────────────────────────
//  MODEL 2: EXPONENTIAL APPROACH TO BASELINE
// ─────────────────────────────────────────────

pub struct ExponentialModel {
    amplitude: f64,
    decay_rate: f64,
    baseline: f64,
    t_anchor: f64,
    fitted: bool,
}

impl Default for ExponentialModel {
    fn default() -> Self {
        Self {
            amplitude: 0.0,
            decay_rate: 0.0,
            baseline: 0.0,
            t_anchor: 0.0,
            fitted: false,
        }
    }
}

impl DriftForecastModel for ExponentialModel {
    fn name(&self) -> &'static str {
        "ExponentialDecay"
    }

    fn fit(&mut self, history: &[FrequencyObservation]) -> Result<(), ModelError> {
        if history.len() < 3 {
            return Err(ModelError::InsufficientData {
                required: 3,
                got: history.len(),
            });
        }
        let first = &history[0];
        let mid = &history[history.len() / 2];
        let last = history.last().unwrap();

        self.t_anchor = first.timestamp_secs;
        self.baseline = last.frequency_ghz;
        self.amplitude = first.frequency_ghz - self.baseline;

        let t_span = mid.timestamp_secs - first.timestamp_secs;
        if t_span.abs() < f64::EPSILON {
            return Err(ModelError::NumericalInstability);
        }
        if self.amplitude.abs() < 1e-12 {
            self.decay_rate = 0.0;
            self.fitted = true;
            return Ok(());
        }

        let mid_diff = mid.frequency_ghz - self.baseline;
        let ratio = mid_diff / self.amplitude;
        if ratio <= 0.0 || ratio >= 1.0 {
            return Err(ModelError::NumericalInstability);
        }
        self.decay_rate = -ratio.ln() / t_span;
        self.fitted = true;
        Ok(())
    }

    fn predict(&self, t: f64) -> Result<f64, ModelError> {
        if !self.fitted {
            return Err(ModelError::NotFitted);
        }
        let dt = t - self.t_anchor;
        Ok(self.baseline + self.amplitude * (-self.decay_rate * dt).exp())
    }
}

// ─────────────────────────────────────────────
//  MODEL 3: MOVING AVERAGE + LOCAL SLOPE
// ─────────────────────────────────────────────

pub struct MovingAverageModel {
    predicted_value: f64,
    window_slope: f64,
    last_ts: f64,
    fitted: bool,
}

impl Default for MovingAverageModel {
    fn default() -> Self {
        Self {
            predicted_value: 0.0,
            window_slope: 0.0,
            last_ts: 0.0,
            fitted: false,
        }
    }
}

impl DriftForecastModel for MovingAverageModel {
    fn name(&self) -> &'static str {
        "MovingAverage"
    }

    fn fit(&mut self, history: &[FrequencyObservation]) -> Result<(), ModelError> {
        if history.is_empty() {
            return Err(ModelError::InsufficientData { required: 1, got: 0 });
        }
        let window = &history[history.len().saturating_sub(5)..];
        self.predicted_value =
            window.iter().map(|o| o.frequency_ghz).sum::<f64>() / window.len() as f64;

        if window.len() >= 2 {
            let first = window.first().unwrap();
            let last = window.last().unwrap();
            let dt = last.timestamp_secs - first.timestamp_secs;
            self.window_slope = if dt > 0.0 {
                (last.frequency_ghz - first.frequency_ghz) / dt
            } else {
                0.0
            };
        } else {
            self.window_slope = 0.0;
        }
        self.last_ts = window.last().unwrap().timestamp_secs;
        self.fitted = true;
        Ok(())
    }

    fn predict(&self, t: f64) -> Result<f64, ModelError> {
        if !self.fitted {
            return Err(ModelError::NotFitted);
        }
        Ok(self.predicted_value + self.window_slope * (t - self.last_ts))
    }
}

// ─────────────────────────────────────────────
//  MODEL EVALUATOR — LOOCV MAE
// ─────────────────────────────────────────────

pub struct ModelEvaluator;

#[derive(Debug)]
pub struct EvaluationResult {
    pub model_name: String,
    pub mae: f64,
}

type ModelFactory = Box<dyn Fn() -> Box<dyn DriftForecastModel> + Send + Sync>;

impl ModelEvaluator {
    pub fn factories() -> Vec<(&'static str, ModelFactory)> {
        vec![
            (
                "LinearRegression",
                Box::new(|| {
                    Box::new(LinearRegressionModel::default()) as Box<dyn DriftForecastModel>
                }),
            ),
            (
                "ExponentialDecay",
                Box::new(|| Box::new(ExponentialModel::default()) as Box<dyn DriftForecastModel>),
            ),
            (
                "MovingAverage",
                Box::new(|| Box::new(MovingAverageModel::default()) as Box<dyn DriftForecastModel>),
            ),
        ]
    }

    pub fn evaluate_all(history: &[FrequencyObservation]) -> Vec<EvaluationResult> {
        let mut results = Vec::new();
        for (name, factory) in Self::factories() {
            let mae = Self::loocv(history, factory.as_ref());
            results.push(EvaluationResult {
                model_name: (*name).to_string(),
                mae,
            });
        }
        results.sort_by(|a, b| {
            a.mae
                .partial_cmp(&b.mae)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results
    }

    fn loocv(
        history: &[FrequencyObservation],
        constructor: &dyn Fn() -> Box<dyn DriftForecastModel>,
    ) -> f64 {
        if history.len() < 3 {
            return f64::INFINITY;
        }

        let mut total_error = 0.0;
        let mut count = 0usize;

        for i in 1..history.len() {
            let train: Vec<_> = history[..i].to_vec();
            let test = &history[i];

            let mut model = constructor();
            if model.fit(&train).is_err() {
                continue;
            }
            if let Ok(predicted) = model.predict(test.timestamp_secs) {
                total_error += (predicted - test.frequency_ghz).abs();
                count += 1;
            }
        }

        if count == 0 {
            f64::INFINITY
        } else {
            total_error / count as f64
        }
    }

    /// Refit the lowest-LOOCV-MAE model on the full history.
    pub fn select_best(history: &[FrequencyObservation]) -> Option<(String, Box<dyn DriftForecastModel>)> {
        if history.len() < 3 {
            return None;
        }

        let mut best: Option<(String, f64, Box<dyn DriftForecastModel>)> = None;

        for (name, factory) in Self::factories() {
            let mae = Self::loocv(history, factory.as_ref());
            let mut model = factory();
            if model.fit(history).is_err() {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((_, m, _)) => mae < *m,
            };
            if replace {
                best = Some(((*name).to_string(), mae, model));
            }
        }

        best.map(|(n, _, m)| (n, m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady_drift_history() -> Vec<FrequencyObservation> {
        (0..10)
            .map(|i| FrequencyObservation {
                timestamp_secs: i as f64 * 3600.0,
                frequency_ghz: 5.0 + i as f64 * 0.005,
            })
            .collect()
    }

    #[test]
    fn linear_model_fits_steady_drift() {
        let history = steady_drift_history();
        let mut model = LinearRegressionModel::default();
        assert!(model.fit(&history).is_ok());
        let predicted = model.predict(36000.0).unwrap();
        assert!((predicted - 5.05).abs() < 0.02);
    }

    #[test]
    fn evaluator_ranks_models() {
        let history = steady_drift_history();
        let results = ModelEvaluator::evaluate_all(&history);
        assert!(!results.is_empty());
        assert!(results.first().unwrap().mae <= results.last().unwrap().mae);
    }

    #[test]
    fn select_best_fits_full_history() {
        let history = steady_drift_history();
        let (name, m) = ModelEvaluator::select_best(&history).expect("best");
        assert!(!name.is_empty());
        assert!(m.predict(history.last().unwrap().timestamp_secs).is_ok());
    }

    #[test]
    fn exponential_model_fits_settling_curve() {
        let history: Vec<FrequencyObservation> = (0..8)
            .map(|i| {
                let t = i as f64 * 600.0;
                FrequencyObservation {
                    timestamp_secs: t,
                    frequency_ghz: 5.2 + 0.1 * (-0.001 * t).exp(),
                }
            })
            .collect();

        let mut model = ExponentialModel::default();
        assert!(model.fit(&history).is_ok());
    }
}
