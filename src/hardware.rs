//! Layer 3 — hardware interface: real electronics behind a trait, mock for tests.

use async_trait::async_trait;

use crate::pulse::Pulse;

/// When `Pulse::start_ns` is used as a diagnostic tag with [`MockQuantumHardware`].
pub mod diagnostic {
    pub const SPECTROSCOPY: u64 = 0;
    pub const T1: u64 = 1;
    pub const T2: u64 = 2;
    pub const GATE_TUNE: u64 = 3;
}

#[derive(Clone, Debug)]
pub enum Measurement {
    SpectroscopyResponse { response: f64 },
    RelaxationTimeNs(u64),
    DephasingTimeNs(u64),
    GateTune { duration_ns: u64, fidelity: f64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HardwareError {
    InvalidQubit(u8),
    NotArmed,
}

#[async_trait]
pub trait QuantumHardware: Send + Sync {
    async fn send_pulse(&self, qubit_id: u8, pulse: Pulse) -> Result<(), HardwareError>;
    async fn read_result(&self, qubit_id: u8) -> Result<Measurement, HardwareError>;
}

struct HwState {
    resonances: std::collections::HashMap<u8, f64>,
    last_pulse: std::collections::HashMap<u8, Pulse>,
}

/// Simulated control electronics + qubit line (Lorentzian probe, fixed T1/T2 model).
pub struct MockQuantumHardware {
    state: std::sync::Arc<tokio::sync::Mutex<HwState>>,
}

impl MockQuantumHardware {
    pub fn new_two_qubit_lab() -> Self {
        let mut resonances = std::collections::HashMap::new();
        resonances.insert(0, 5.02_f64);
        resonances.insert(1, 5.15);
        Self {
            state: std::sync::Arc::new(tokio::sync::Mutex::new(HwState {
                resonances,
                last_pulse: std::collections::HashMap::new(),
            })),
        }
    }

    fn lorentzian_response(f_probe: f64, f0: f64) -> f64 {
        let det_mhz = (f_probe - f0) * 1_000.0;
        1.0 / (1.0 + det_mhz * det_mhz * 0.02)
    }
}

#[async_trait]
impl QuantumHardware for MockQuantumHardware {
    async fn send_pulse(&self, qubit_id: u8, pulse: Pulse) -> Result<(), HardwareError> {
        let mut g = self.state.lock().await;
        if !g.resonances.contains_key(&qubit_id) {
            return Err(HardwareError::InvalidQubit(qubit_id));
        }
        g.last_pulse.insert(qubit_id, pulse);
        Ok(())
    }

    async fn read_result(&self, qubit_id: u8) -> Result<Measurement, HardwareError> {
        let g = self.state.lock().await;
        if !g.resonances.contains_key(&qubit_id) {
            return Err(HardwareError::InvalidQubit(qubit_id));
        }
        let pulse = g.last_pulse.get(&qubit_id).ok_or(HardwareError::NotArmed)?;
        match pulse.start_ns {
            diagnostic::SPECTROSCOPY => {
                let f0 = g.resonances[&qubit_id];
                Ok(Measurement::SpectroscopyResponse {
                    response: Self::lorentzian_response(pulse.frequency_ghz, f0),
                })
            }
            diagnostic::T1 => Ok(Measurement::RelaxationTimeNs(
                55_000u64 + 1_500 * u64::from(qubit_id),
            )),
            diagnostic::T2 => Ok(Measurement::DephasingTimeNs(
                42_000 + 1_200 * u64::from(qubit_id),
            )),
            diagnostic::GATE_TUNE => {
                let dur = 18 + u64::from(qubit_id) * 2;
                let fid = 0.9992 - f64::from(qubit_id) * 0.00015;
                Ok(Measurement::GateTune {
                    duration_ns: dur,
                    fidelity: fid,
                })
            }
            _ => Err(HardwareError::NotArmed),
        }
    }
}
