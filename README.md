# Quantum Calibration & Control (Rust)

**Crate:** `quantum_calibration`

A **simulation and architecture reference** for automated calibration of gate-based superconducting qubits. The code models how a production system could **replace repetitive technician workflows** with software: scheduled jobs, parallel per-qubit routines, a hardware abstraction, persistent parameters, a health layer, and **AI/ML hooks** for predictive drift and model selection.

The design is intentionally analogous to a **hospital monitoring system**: vitals are checked on a policy, automated responses handle known conditions, and humans are notified for serious or persistent problems—not for every routine measurement. The **predictive drift** layer adds a data-driven notion of *when* to intervene, instead of relying only on a fixed clock.

> **Scope:** This repository does **not** drive real instruments. It uses **mock quantum hardware** so the full control loop is testable in CI and runnable without lab equipment. The **`QuantumHardware`** and **`DriftForecastModel`** (ML trait) boundaries are where real firmware, drivers, or Python ML stacks would plug in.

---

## Table of contents

- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Architecture overview](#architecture-overview)
- [Layer-by-layer technical description](#layer-by-layer-technical-description)
- [AI / ML integration](#ai--ml-integration)
- [Data flow (one calibration cycle)](#data-flow-one-calibration-cycle)
- [Pulse schedules and crosstalk checks](#pulse-schedules-and-crosstalk-checks)
- [Supporting modules (library)](#supporting-modules-library)
- [Incremental calibration](#incremental-calibration)
- [Testing](#testing)
- [Current limitations](#current-limitations)
- [Future improvements](#future-improvements)
- [Repository layout](#repository-layout)

---

## Requirements

- **Rust** toolchain with support for **Edition 2024** (see `Cargo.toml`).
- **Tokio** and **async-trait**; no other third-party dependencies.

---

## Quick start

```bash
cargo build
cargo run
cargo test
```

- **`cargo run`** runs the **`quantum_calibration`** binary (`src/main.rs`), which calls **`quantum_calibration::demo::run_all_demos()`** (`src/demo.rs`):
  1. **Calibration pipeline** — two passes (full, then incremental) on a two-qubit mock; prints parameters, benchmark, health alerts, parameter-store summary.
  2. **AI / ML demo** — `PredictiveDriftDetector` with synthetic frequency history, urgency ranking, and **`ModelEvaluator`** LOOCV rankings plus best-model selection on a steady-drift series.
- **`cargo test`** — 12+ unit tests (calibration, schedule, parallel jobs, drift predictor, ML models).

---

## Architecture overview

Core stack (**five layers** in `src/lib.rs`):

| Layer | Module(s) | Role |
|-------|-----------|------|
| **1 — Scheduler** | `job_scheduler` | Tokio parallel per-qubit calibration cycle; optional periodic loop sketch. |
| **2 — Calibration routines** | `routines` | Async frequency sweep, T1/T2, gate tune, crosstalk template; `Result` per step. |
| **3 — Hardware** | `hardware` | `QuantumHardware` trait + `MockQuantumHardware`. |
| **4 — Parameter store** | `parameter_store` | Last known good `CalibrationParams` + generation; no erase on failure. |
| **5 — Health** | `health` | Thresholds, drift warnings, escalation after repeated failure. |

**AI / ML (cross-cutting):**

| Module | Role |
|--------|------|
| **`drift_predictor`** | Rolling **frequency history** per qubit, **linear regression** drift rate, **time-to-tolerance** estimate, fleet **`qubits_due_within`**. Complements fixed-interval `schedule::calibration_due`. |
| **`ml_models`** | **`DriftForecastModel`** trait (Rust-native models + future PyO3 backends), **LinearRegression**, **ExponentialDecay**, **MovingAverage**, **LOOCV** **`ModelEvaluator`** to rank and **`select_best`**. |

Also: `calibration`, `pulse`, `schedule`, `controller` (see below).

---

## Layer-by-layer technical description

### 1. Scheduler (`src/job_scheduler.rs`)

- **`run_calibration_cycle_parallel`** — Spawns one Tokio task per qubit calling `calibrate_one_qubit`; builds `CalibrationReport`; `validation_passed` requires **all** requested qubits succeeded and pass global checks.
- **`run_periodic_calibration_scheduler`** — Infinite interval loop (not called from default `main`).

### 2. Calibration routines (`src/routines.rs`)

Async steps over **`QuantumHardware`**: spectroscopy grid, T1/T2, gate tune, crosstalk template via `pulse::schedule_has_conflicts`. **Incremental** path can reuse stored T1/T2 when “vitals” look fine. Integrates **health** and **parameter_store** on success/failure.

### 3. Hardware (`src/hardware.rs`)

`QuantumHardware::send_pulse` / `read_result`. Mock uses **`Pulse::start_ns`** as **diagnostic opcodes** (`SPECTROSCOPY`, `T1`, `T2`, `GATE_TUNE`) — simulation-only; real systems would use explicit command types.

### 4. Parameter store (`src/parameter_store.rs`)

`CalibrationParams` + per-qubit generations; **`commit_success`** on pass only.

### 5. Health (`src/health.rs`)

`HealthThresholds`, `Alert` (`DriftWarning`, `EscalateHuman`), consecutive failure counting (`ESCALATE_AFTER_FAILURES = 3`).

---

## AI / ML integration

### Predictive drift (`src/drift_predictor.rs`)

- **`FrequencyObservation`** — `{ timestamp_secs, frequency_ghz }` (lab clock or epoch seconds).
- **`DriftModel::train`** — Least-squares **slope** (GHz/s) over a qubit’s window (max **20** points).
- **`seconds_until_recalibration_needed`** — Linear extrapolation of how long until a **tolerance** budget (GHz) is consumed; returns **`None`** if drift rate is negligible, **`Some(0)`** if overdue.
- **`PredictiveDriftDetector`** — `record_observation`, **`print_predictions`**, **`qubits_due_within(horizon_secs, now_secs)`** sorted by urgency.

**Integration point (production):** After each successful calibration, append **`(qubit_id, drive_frequency_ghz, now)`** to this history and consult **`qubits_due_within`** before or alongside **`schedule::calibration_due`** to prioritize which qubits to calibrate next.

### Model registry (`src/ml_models.rs`)

- **`DriftForecastModel`** trait — `name`, **`fit`**, **`predict(t)`**, optional **`seconds_until_drift`** (forward scan vs tolerance). **Send + Sync** so the same interface can later wrap **PyO3** Python models or remote inference.
- **Built-in models**
  - **LinearRegression** — steady drift; **2+** points.
  - **ExponentialDecay** — three-point heuristic for **settling** toward a baseline (e.g. post thermal transient); **3+** points; uses anchored time \(t - t_0\).
  - **MovingAverage** — last up-to-5 points mean + local slope; conservative baseline when trend is noisy.
- **`ModelEvaluator`**
  - **`evaluate_all`** — **leave-one-out cross-validation (LOOCV)** mean absolute error (MAE) per model; results sorted best-first.
  - **`select_best`** — LOOCV pick, then **refit winner on full history** for deployment.

**`main` demo:** feeds synthetic multi-qubit history into **`PredictiveDriftDetector`**, prints predictions and due-within-2h list; runs **`evaluate_all` / `select_best`** on a steady-drift qubit-0 series.

---

## Data flow (one calibration cycle)

1. Scheduler (or `main`) starts **`run_calibration_cycle_parallel`**.
2. Per-qubit tasks run **`calibrate_one_qubit`** → hardware trait → store + health.
3. Report assembled; **AI** path would additionally **`record_observation`** per successful drive frequency.

---

## Pulse schedules and crosstalk checks

**`pulse`**: `Pulse`, **`validate_pulse`**, sorting, **50 ns** same-channel conflict detection (**O(n²)**; document scaling via sort + sliding window). Used for **template** validation in routines, not for mock diagnostic opcodes.

---

## Supporting modules (library)

- **`calibration`** — Shared `CalibrationReport` / `MockTransmonSystem` (sync).
- **`schedule`** — **`calibration_due`**: chip swap > cold start > periodic interval.
- **`controller`** — Sync **`CalibrationController`** + **`MockTransmonSystem`** (tests / alternate entrypoint; not required by default `main`).

---

## Incremental calibration

With **`incremental == true`**, T1/T2 may be **reused** from the store when values already look healthy. Production should add **short verification** pulses before trusting stale T1/T2.

---

## Testing

| Area | Location |
|------|----------|
| Spectroscopy / sync suite | `calibration::tests` |
| Schedule policy | `schedule::tests` |
| Parallel cycle + store | `job_scheduler::tests` |
| Linear drift + fleet urgency | `drift_predictor::tests` |
| LOOCV, linear / exponential fit | `ml_models::tests` |

---

## Current limitations

- **No real hardware** — Mock responses only; no timing guarantees, noise, or IQ data.
- **Drift model is intentionally simple** — Single linear slope per window in `drift_predictor`; no confidence intervals, no multi-parameter state estimation.
- **ML models are lightweight** — Closed-form or heuristic fits, not trained neural nets; **LOOCV** is **O(n × models)** on small windows only.
- **Exponential fit** — Three-point heuristic; fragile on bad spacing or non-monotonic data (returns **`NumericalInstability`**).
- **AI not wired into the async scheduler yet** — `main` runs calibration then a **separate** AI demo; production would **close the loop** (observations from real commits, schedule decisions from `qubits_due_within` + policy).
- **Parameter store is in-memory** — No durability across process restarts.
- **Health escalation** — In-memory alerts only; no external notification channel.
- **Parallel calibration** — Assumes independent qubit tasks; real shared AWG/LO resources would need **locking** and **serialization** rules.

---

## Future improvements

These are **deliberately out of scope** for the current crate but match how this codebase is **structured to evolve**:

1. **PyO3 bridge** — Implement **`DriftForecastModel`** for wrappers around **scikit-learn** / **PyTorch** models trained offline or online.
2. **Gaussian process regression** — Time-series drift with **uncertainty** (“recalibrate within X hours with 95% confidence”).
3. **Sequence models (LSTM / Transformer)** — Capture **memory** and **non-linear** drift in frequency traces.
4. **Reinforcement learning scheduler** — State = calibration snapshot + queue; actions = which qubit (or which routine) to run; reward = −downtime − unnecessary cal cost; train with **stable-baselines3** or similar, policy served via Rust or Python sidecar.
5. **Anomaly detection on full vectors** — Extend **health** beyond thresholds: **multivariate** unusualness on \((f, T_1, T_2, \text{fid}, \ldots)\).
6. **LLM diagnostics** — Feed structured **`Alert`** + `DriftModel::summary` into an **RAG** or tool-calling agent for operator-facing explanations (non-experts).
7. **Persistent store + audit** — SQLite/Postgres, **versioned** parameters, who/what triggered each cal.
8. **Closed-loop integration** — Single service: **`run_calibration_cycle_parallel`** selects qubit subset from **`PredictiveDriftDetector::qubits_due_within`** merged with **`calibration_due`**.

---

## Repository layout

```
Cargo.toml
Cargo.lock
README.md
src/
  lib.rs
  main.rs              # thin wrapper → `demo::run_all_demos`
  demo.rs              # calibration + AI demos (uses `crate::` internally)
  job_scheduler.rs
  routines.rs
  hardware.rs
  parameter_store.rs
  health.rs
  calibration.rs
  pulse.rs
  schedule.rs
  controller.rs
  drift_predictor.rs   # AI: linear drift + fleet urgency
  ml_models.rs         # DriftForecastModel trait + LOOCV registry
```

---

## License

Add a `LICENSE` file if you distribute the crate; none is bundled by default.
