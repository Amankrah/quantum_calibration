//! Layer 4 — parameter store: last known good, never leave the machine without a config.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

#[derive(Clone, Debug)]
pub struct CalibrationParams {
    pub drive_frequency_ghz: f64,
    pub t1_ns: u64,
    pub t2_ns: u64,
    pub pi_pulse_duration_ns: u64,
    pub single_qubit_fidelity: f64,
    pub generation: u64,
}

#[derive(Clone, Debug)]
pub struct ParameterStore {
    inner: Arc<RwLock<HashMap<u8, CalibrationParams>>>,
}

impl ParameterStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, qubit: u8) -> Option<CalibrationParams> {
        self.inner.read().await.get(&qubit).cloned()
    }

    /// Successful calibration: bump generation and persist (atomic per qubit in this mock).
    pub async fn commit_success(&self, qubit: u8, mut params: CalibrationParams) {
        let mut w = self.inner.write().await;
        let next_gen = w
            .get(&qubit)
            .map(|p| p.generation.saturating_add(1))
            .unwrap_or(1);
        params.generation = next_gen;
        w.insert(qubit, params);
    }

    /// Snapshot for health / UI (never drops entries on failed cal — old row stays until success).
    pub async fn snapshot(&self) -> HashMap<u8, CalibrationParams> {
        self.inner.read().await.clone()
    }
}
