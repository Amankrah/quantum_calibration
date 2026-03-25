use std::sync::Arc;

use tokio::sync::Mutex;

use test_rust_project::calibration::run_validation_benchmark;
use test_rust_project::health::{HealthMonitor, HealthThresholds};
use test_rust_project::hardware::{MockQuantumHardware, QuantumHardware};
use test_rust_project::job_scheduler::run_calibration_cycle_parallel;
use test_rust_project::parameter_store::ParameterStore;

/// End-to-end path: scheduler tick → parallel routines → trait hardware → parameter store → health.
async fn run_calibration_pipeline() {
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

#[tokio::main]
async fn main() {
    run_calibration_pipeline().await;
}
