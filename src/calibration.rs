//! Mock transmon calibration pipeline (sync shortcut) + shared report types.
//! The five-layer stack uses [`crate::routines`] and [`crate::hardware`] for the same steps async.

use crate::parameter_store::CalibrationParams;
use crate::pulse::{schedule_has_conflicts, Pulse};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QubitId(pub u8);

#[derive(Clone, Debug)]
pub struct QubitCalibration {
    pub qubit: QubitId,
    pub params: CalibrationParams,
    pub crosstalk_schedule_ok: bool,
}

#[derive(Clone, Debug)]
pub struct CalibrationReport {
    pub qubits: Vec<QubitCalibration>,
    pub validation_passed: bool,
}

struct MockQubit {
    id: QubitId,
    true_resonance_ghz: f64,
}

/// Simulated lab stack (sync): kept for quick tests and the legacy controller path.
pub struct MockTransmonSystem {
    qubits: Vec<MockQubit>,
}

impl MockTransmonSystem {
    pub fn new_two_qubit_lab_setup() -> Self {
        Self {
            qubits: vec![
                MockQubit {
                    id: QubitId(0),
                    true_resonance_ghz: 5.02,
                },
                MockQubit {
                    id: QubitId(1),
                    true_resonance_ghz: 5.15,
                },
            ],
        }
    }

    pub fn run_full_calibration_suite(&mut self) -> CalibrationReport {
        let mut out = Vec::with_capacity(self.qubits.len());
        for q in &self.qubits {
            let drive = simulate_frequency_sweep(q.true_resonance_ghz);
            let (t1, t2) = measure_t1_t2(q.id.0);
            let (dur_ns, fid) = tune_pi_pulse(q.id.0);
            let crosstalk_ok = crosstalk_template_check(q.id.0);
            out.push(QubitCalibration {
                qubit: q.id,
                params: CalibrationParams {
                    drive_frequency_ghz: drive,
                    t1_ns: t1,
                    t2_ns: t2,
                    pi_pulse_duration_ns: dur_ns,
                    single_qubit_fidelity: fid,
                    generation: 0,
                },
                crosstalk_schedule_ok: crosstalk_ok,
            });
        }
        let validation_passed = out.iter().all(|q| {
            q.params.single_qubit_fidelity >= 0.99
                && q.crosstalk_schedule_ok
                && q.params.t2_ns <= q.params.t1_ns
        });
        CalibrationReport {
            qubits: out,
            validation_passed,
        }
    }
}

fn simulate_frequency_sweep(true_ghz: f64) -> f64 {
    let mut best_f = 4.8_f64;
    let mut best_r = 0.0_f64;
    let mut f = 4.8_f64;
    while f <= 5.4 {
        let det_mhz = (f - true_ghz) * 1_000.0;
        let response = 1.0 / (1.0 + det_mhz * det_mhz * 0.02);
        if response > best_r {
            best_r = response;
            best_f = f;
        }
        f += 0.02;
    }
    best_f
}

fn measure_t1_t2(qubit: u8) -> (u64, u64) {
    let t1 = 55_000u64 + 1_500 * u64::from(qubit);
    let t2 = 42_000 + 1_200 * u64::from(qubit);
    (t1, t2)
}

fn tune_pi_pulse(qubit: u8) -> (u64, f64) {
    let dur = 18 + u64::from(qubit) * 2;
    let fid = 0.9992 - f64::from(qubit) * 0.00015;
    (dur, fid)
}

fn crosstalk_template_check(qubit: u8) -> bool {
    let ch = qubit;
    let safe_template = [
        Pulse {
            channel: ch,
            start_ns: 0,
            frequency_ghz: 5.0,
        },
        Pulse {
            channel: ch,
            start_ns: 200,
            frequency_ghz: 5.0,
        },
    ];
    !schedule_has_conflicts(&safe_template)
}

pub fn run_validation_benchmark(report: &CalibrationReport) -> bool {
    report.validation_passed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_finds_resonance_near_ground_truth() {
        let truth = 5.11;
        let found = simulate_frequency_sweep(truth);
        assert!((found - truth).abs() < 0.03);
    }

    #[test]
    fn full_suite_passes_on_default_mock() {
        let mut m = MockTransmonSystem::new_two_qubit_lab_setup();
        let r = m.run_full_calibration_suite();
        assert!(r.validation_passed);
        assert_eq!(r.qubits.len(), 2);
    }
}
