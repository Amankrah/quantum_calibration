//! Layer 2 — calibration routines: independent async steps per qubit (parallelizable).

use std::sync::Arc;

use crate::calibration::{QubitCalibration, QubitId};
use crate::hardware::diagnostic;
use crate::hardware::{HardwareError, Measurement, QuantumHardware};
use crate::health::{HealthMonitor, HealthThresholds};
use crate::parameter_store::{CalibrationParams, ParameterStore};
use crate::pulse::{schedule_has_conflicts, Pulse};

#[derive(Debug)]
pub enum CalibrationError {
    Hardware(HardwareError),
    Validation(&'static str),
}

impl From<HardwareError> for CalibrationError {
    fn from(e: HardwareError) -> Self {
        CalibrationError::Hardware(e)
    }
}

pub async fn routine_frequency_sweep(
    hw: &dyn QuantumHardware,
    qubit: u8,
) -> Result<f64, CalibrationError> {
    let mut best_f = 4.8_f64;
    let mut best_r = 0.0_f64;
    let mut f = 4.8_f64;
    while f <= 5.4 {
        hw.send_pulse(
            qubit,
            Pulse {
                channel: qubit,
                start_ns: diagnostic::SPECTROSCOPY,
                frequency_ghz: f,
            },
        )
        .await?;
        let Measurement::SpectroscopyResponse { response } = hw.read_result(qubit).await? else {
            return Err(CalibrationError::Validation("expected spectroscopy sample"));
        };
        if response > best_r {
            best_r = response;
            best_f = f;
        }
        f += 0.02;
    }
    Ok(best_f)
}

pub async fn routine_measure_t1(hw: &dyn QuantumHardware, qubit: u8) -> Result<u64, CalibrationError> {
    hw.send_pulse(
        qubit,
        Pulse {
            channel: qubit,
            start_ns: diagnostic::T1,
            frequency_ghz: 0.0,
        },
    )
    .await?;
    let Measurement::RelaxationTimeNs(ns) = hw.read_result(qubit).await? else {
        return Err(CalibrationError::Validation("expected T1"));
    };
    Ok(ns)
}

pub async fn routine_measure_t2(hw: &dyn QuantumHardware, qubit: u8) -> Result<u64, CalibrationError> {
    hw.send_pulse(
        qubit,
        Pulse {
            channel: qubit,
            start_ns: diagnostic::T2,
            frequency_ghz: 0.0,
        },
    )
    .await?;
    let Measurement::DephasingTimeNs(ns) = hw.read_result(qubit).await? else {
        return Err(CalibrationError::Validation("expected T2"));
    };
    Ok(ns)
}

pub async fn routine_gate_tune(
    hw: &dyn QuantumHardware,
    qubit: u8,
) -> Result<(u64, f64), CalibrationError> {
    hw.send_pulse(
        qubit,
        Pulse {
            channel: qubit,
            start_ns: diagnostic::GATE_TUNE,
            frequency_ghz: 0.0,
        },
    )
    .await?;
    let Measurement::GateTune {
        duration_ns,
        fidelity,
    } = hw.read_result(qubit).await?
    else {
        return Err(CalibrationError::Validation("expected gate tune"));
    };
    Ok((duration_ns, fidelity))
}

fn crosstalk_template_ok(qubit: u8) -> bool {
    let ch = qubit;
    let safe = [
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
    !schedule_has_conflicts(&safe)
}

/// Incremental: if we already have sane T1/T2 in the store, skip re-measurement (production would
/// still run a cheap sanity check; here we model “vitals still in range”).
async fn maybe_measure_t1_t2(
    hw: &dyn QuantumHardware,
    qubit: u8,
    store: &ParameterStore,
    incremental: bool,
    thresholds: &HealthThresholds,
) -> Result<(u64, u64), CalibrationError> {
    if incremental {
        if let Some(p) = store.get(qubit).await {
            if p.t1_ns >= thresholds.min_t1_ns && p.t2_ns <= p.t1_ns {
                return Ok((p.t1_ns, p.t2_ns));
            }
        }
    }
    let t1 = routine_measure_t1(hw, qubit).await?;
    let t2 = routine_measure_t2(hw, qubit).await?;
    Ok((t1, t2))
}

/// Full pipeline for one qubit: uses hardware trait, updates store on success, drives health signals.
pub async fn calibrate_one_qubit(
    hw: Arc<dyn QuantumHardware>,
    store: ParameterStore,
    health: Arc<tokio::sync::Mutex<HealthMonitor>>,
    qubit: u8,
    thresholds: &HealthThresholds,
    incremental: bool,
) -> Result<QubitCalibration, CalibrationError> {
    let prev = store.get(qubit).await;

    let drive = routine_frequency_sweep(hw.as_ref(), qubit).await?;

    if let Some(ref p) = prev {
        let drift = (p.drive_frequency_ghz - drive).abs();
        if drift > thresholds.max_freq_drift_ghz {
            health
                .lock()
                .await
                .warn_drift(
                    qubit,
                    format!(
                        "drive moved {:.4} GHz vs last good (threshold {:.4})",
                        drift, thresholds.max_freq_drift_ghz
                    ),
                );
        }
    }

    let (t1_ns, t2_ns) =
        maybe_measure_t1_t2(hw.as_ref(), qubit, &store, incremental, thresholds).await?;

    let (pi_ns, fid) = routine_gate_tune(hw.as_ref(), qubit).await?;

    let crosstalk_ok = crosstalk_template_ok(qubit);

    if t2_ns > t1_ns {
        let msg = "T2 > T1 (unphysical for this mock)";
        health.lock().await.record_failure(qubit, msg);
        return Err(CalibrationError::Validation("T2 > T1"));
    }
    if fid < thresholds.min_fidelity {
        let msg = format!("fidelity {:.4} below {:.4}", fid, thresholds.min_fidelity);
        health.lock().await.record_failure(qubit, msg.clone());
        return Err(CalibrationError::Validation("fidelity below threshold"));
    }
    if !crosstalk_ok {
        health
            .lock()
            .await
            .record_failure(qubit, "crosstalk template conflict");
        return Err(CalibrationError::Validation("crosstalk"));
    }

    let params = CalibrationParams {
        drive_frequency_ghz: drive,
        t1_ns,
        t2_ns,
        pi_pulse_duration_ns: pi_ns,
        single_qubit_fidelity: fid,
        generation: 0,
    };

    store.commit_success(qubit, params).await;
    let saved = store
        .get(qubit)
        .await
        .expect("parameter store must hold row after successful commit");
    health.lock().await.record_success(qubit);

    Ok(QubitCalibration {
        qubit: QubitId(qubit),
        params: saved,
        crosstalk_schedule_ok: crosstalk_ok,
    })
}
