//! Demo entrypoints for `cargo run`. Keeps the binary as a thin wrapper around the `quantum_calibration` library.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::calibration::run_validation_benchmark;
use crate::drift_predictor::{FrequencyObservation, PredictiveDriftDetector};
use crate::health::{HealthMonitor, HealthThresholds};
use crate::hardware::{MockQuantumHardware, QuantumHardware};
use crate::job_scheduler::run_calibration_cycle_parallel;
use crate::ml_models::ModelEvaluator;
use crate::parameter_store::ParameterStore;

/// Scheduler tick → parallel routines → hardware trait → parameter store → health.
pub async fn run_calibration_pipeline() {
    println!("Quantum calibration & control pipeline (mock hardware)\n");

    let hw: Arc<dyn QuantumHardware> = Arc::new(MockQuantumHardware::new_two_qubit_lab());
    let store = ParameterStore::new();
    let health = Arc::new(Mutex::new(HealthMonitor::default()));
    let thresholds = HealthThresholds::default();
    let qubits = [0_u8, 1_u8];

    println!(
        "Layer 1–2: calibration cycle — parallel jobs for qubits {:?}",
        qubits
    );
    let report = run_calibration_cycle_parallel(
        &qubits,
        hw.clone(),
        store.clone(),
        health.clone(),
        thresholds.clone(),
        false,
    )
    .await;

    for q in &report.qubits {
        println!(
            "  {:?}  gen={}  f={:.3} GHz  T1={} ns  T2={} ns  π={} ns  F≈{:.4}",
            q.qubit,
            q.params.generation,
            q.params.drive_frequency_ghz,
            q.params.t1_ns,
            q.params.t2_ns,
            q.params.pi_pulse_duration_ns,
            q.params.single_qubit_fidelity,
        );
    }
    println!(
        "  Benchmark: {}",
        if run_validation_benchmark(&report) {
            "PASS"
        } else {
            "FAIL"
        }
    );

    {
        let mut h = health.lock().await;
        for alert in h.take_alerts() {
            println!("  Health: {}", alert);
        }
    }

    println!("\nIncremental cycle — reuse T1/T2 when vitals remain in range");
    let report2 = run_calibration_cycle_parallel(
        &qubits,
        hw,
        store.clone(),
        health.clone(),
        thresholds.clone(),
        true,
    )
    .await;
    println!(
        "  Generations: {:?}  |  benchmark: {}",
        report2
            .qubits
            .iter()
            .map(|q| (q.qubit.0, q.params.generation))
            .collect::<Vec<_>>(),
        if run_validation_benchmark(&report2) {
            "PASS"
        } else {
            "FAIL"
        }
    );

    let snap = store.snapshot().await;
    println!(
        "\nParameter store: {} qubit row(s) (last known good retained on failed runs)",
        snap.len()
    );
}

pub fn run_ai_drift_and_model_selection_demo() {
    println!("\n── AI layer: predictive drift (linear history) ──");

    let mut predictor = PredictiveDriftDetector::new(0.08);

    let past_observations = vec![
        (0_u8, 5.000_f64, 0.0_f64),
        (0, 5.008, 3600.0),
        (0, 5.015, 7200.0),
        (0, 5.020, 10800.0),
        (1, 5.150, 0.0),
        (1, 5.153, 3600.0),
        (1, 5.156, 7200.0),
        (1, 5.160, 10800.0),
    ];

    for (qid, freq, time) in past_observations {
        predictor.record_observation(qid, freq, time);
    }

    let now = 14400.0_f64;
    predictor.print_predictions(now);

    let due = predictor.qubits_due_within(7200.0, now);
    if !due.is_empty() {
        println!("\n  Qubits due for recalibration within 2h horizon:");
        for (qid, secs) in due {
            println!("    Qubit {qid} → in {:.0}s ({:.1} min)", secs, secs / 60.0);
        }
    }

    println!("\n── ML model registry: LOOCV ranking on qubit-0 history ──");
    let q0_history: Vec<_> = (0..10)
        .map(|i| FrequencyObservation {
            timestamp_secs: i as f64 * 3600.0,
            frequency_ghz: 5.0 + i as f64 * 0.005,
        })
        .collect();
    let rankings = ModelEvaluator::evaluate_all(&q0_history);
    for r in &rankings {
        println!("  {} — MAE = {:.6} GHz", r.model_name, r.mae);
    }
    if let Some((best_name, _)) = ModelEvaluator::select_best(&q0_history) {
        println!("  Selected for refit on full history: {best_name}");
    }
}

/// Runs calibration pipeline then AI/ML demos (default `cargo run` behavior).
pub async fn run_all_demos() {
    run_calibration_pipeline().await;
    run_ai_drift_and_model_selection_demo();
}
