//! Layer 1 — scheduler: Tokio timer + queued per-qubit jobs (parallel execution).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::calibration::CalibrationReport;
use crate::hardware::QuantumHardware;
use crate::health::{HealthMonitor, HealthThresholds};
use crate::parameter_store::ParameterStore;
use crate::routines::calibrate_one_qubit;

/// One “hospital round”: all qubits calibrated concurrently (independent async tasks).
pub async fn run_calibration_cycle_parallel(
    qubit_ids: &[u8],
    hw: Arc<dyn QuantumHardware>,
    store: ParameterStore,
    health: Arc<Mutex<HealthMonitor>>,
    thresholds: HealthThresholds,
    incremental: bool,
) -> CalibrationReport {
    let mut handles = Vec::with_capacity(qubit_ids.len());
    for &q in qubit_ids {
        let hw = hw.clone();
        let store = store.clone();
        let health = health.clone();
        let thresholds = thresholds.clone();
        handles.push(tokio::spawn(async move {
            calibrate_one_qubit(hw, store, health, q, &thresholds, incremental).await
        }));
    }

    let mut qubits = Vec::new();
    for h in handles {
        match h.await.expect("calibration task panicked") {
            Ok(q) => qubits.push(q),
            Err(_) => { /* failure recorded in HealthMonitor; keep last-good in store */ }
        }
    }

    qubits.sort_by_key(|q| q.qubit.0);

    let validation_passed = qubits.len() == qubit_ids.len()
        && qubits.iter().all(|q| {
            q.params.single_qubit_fidelity >= thresholds.min_fidelity
                && q.crosstalk_schedule_ok
                && q.params.t2_ns <= q.params.t1_ns
        });

    CalibrationReport {
        qubits,
        validation_passed,
    }
}

/// Background-style scheduler (hospital vitals): tick every `period`, run parallel calibration.
/// For demos, call `run_calibration_cycle_parallel` once instead of this loop.
pub async fn run_periodic_calibration_scheduler(
    period: Duration,
    qubit_ids: Vec<u8>,
    hw: Arc<dyn QuantumHardware>,
    store: ParameterStore,
    health: Arc<Mutex<HealthMonitor>>,
    thresholds: HealthThresholds,
) {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let _ = run_calibration_cycle_parallel(
            &qubit_ids,
            hw.clone(),
            store.clone(),
            health.clone(),
            thresholds.clone(),
            true,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::MockQuantumHardware;

    #[tokio::test]
    async fn parallel_cycle_populates_store_and_passes_validation() {
        let hw: Arc<dyn QuantumHardware> = Arc::new(MockQuantumHardware::new_two_qubit_lab());
        let store = ParameterStore::new();
        let health = Arc::new(Mutex::new(HealthMonitor::default()));
        let thresholds = HealthThresholds::default();
        let report = run_calibration_cycle_parallel(
            &[0, 1],
            hw,
            store.clone(),
            health,
            thresholds,
            false,
        )
        .await;
        assert!(report.validation_passed);
        assert_eq!(report.qubits.len(), 2);
        let snap = store.snapshot().await;
        assert_eq!(snap.len(), 2);
        assert!(snap[&0].generation >= 1);
    }
}
