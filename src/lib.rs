//! Automated calibration and control: **hospital-style monitoring for qubits** — continuous
//! sensing (calibration measurements), automated responses, alerts, human only on escalation.
//!
//! ## Five layers
//! 1. **[`job_scheduler`]** — Tokio timer + parallel per-qubit jobs (all qubits at once).
//! 2. **[`routines`]** — Independent async calibration steps (`Result` per routine).
//! 3. **`hardware`** — [`QuantumHardware`](crate::hardware::QuantumHardware) trait + mock electronics.
//! 4. **[`parameter_store`]** — Last known good parameters; failed runs do not erase the store.
//! 5. **[`health`]** — Thresholds, drift warnings, escalate after repeated failure.
//!
//! Supporting modules: **[`calibration`]** (shared types + sync mock), **[`controller`]**, **[`pulse`]**, **[`schedule`]**.

pub mod calibration;
pub mod controller;
pub mod health;
pub mod hardware;
pub mod job_scheduler;
pub mod parameter_store;
pub mod pulse;
pub mod routines;
pub mod schedule;
