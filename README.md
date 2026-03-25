# Quantum Calibration & Control (Rust)

A **simulation and architecture reference** for automated calibration of gate-based superconducting qubits. The code models how a production system could **replace repetitive technician workflows** with software: scheduled jobs, parallel per-qubit routines, a hardware abstraction, persistent parameters, and a health layer that escalates only when automation cannot recover.

The design is intentionally analogous to a **hospital monitoring system**: vitals are checked on a policy, automated responses handle known conditions, and humans are notified for serious or persistent problems—not for every routine measurement.

> **Scope:** This repository does **not** drive real instruments. It uses a **mock quantum hardware** implementation so the full control loop is testable in CI and runnable without lab equipment. The same trait boundary is where a real firmware/driver stack would plug in.

---

## Table of contents

- [Requirements](#requirements)
- [Quick start](#quick-start)
- [Architecture overview](#architecture-overview)
- [Layer-by-layer technical description](#layer-by-layer-technical-description)
- [Data flow (one calibration cycle)](#data-flow-one-calibration-cycle)
- [Pulse schedules and crosstalk checks](#pulse-schedules-and-crosstalk-checks)
- [Supporting modules (library)](#supporting-modules-library)
- [Testing](#testing)
- [Design tradeoffs and production notes](#design-tradeoffs-and-production-notes)
- [Repository layout](#repository-layout)

---

## Requirements

- **Rust** toolchain with support for **Edition 2024** (as set in `Cargo.toml`).
- **Tokio** and **async-trait** (declared in `Cargo.toml`); no other third-party dependencies.

---

## Quick start

```bash
cargo build
cargo run
cargo test
```

- **`cargo run`** executes `src/main.rs`: a **two-pass** calibration pipeline (full calibration, then **incremental** pass) on a **two-qubit** mock machine, printing per-qubit parameters, benchmark status, health alerts (if any), and a parameter-store summary.
- **`cargo test`** runs unit tests for spectroscopy sweep accuracy, schedule policy, parallel calibration + store integration, and the synchronous mock suite in `calibration`.

---

## Architecture overview

The library is organized into **five conceptual layers** (documented in `src/lib.rs`):

| Layer | Module(s) | Role |
|-------|-----------|------|
| **1 — Scheduler** | `job_scheduler` | Triggers work on a timer or tick; runs **one logical “round”** by spawning **parallel** per-qubit tasks (Tokio). |
| **2 — Calibration routines** | `routines` | Async steps: frequency sweep, T1/T2, gate tune, crosstalk template validation; each returns `Result`. |
| **3 — Hardware interface** | `hardware` | `QuantumHardware` trait: `send_pulse` / `read_result`; **`MockQuantumHardware`** for tests and demos. |
| **4 — Parameter store** | `parameter_store` | Persists **last known good** `CalibrationParams` per qubit with a **generation** counter; failed runs do **not** delete prior rows. |
| **5 — Health monitor** | `health` | Applies thresholds, emits **drift** warnings, tracks **consecutive failures**, and raises **human escalation** after a fixed streak. |

Additional modules provide shared types, pulse scheduling, and **synchronous** policy/helpers used by tests or future orchestration:

- `calibration` — `QubitId`, `QubitCalibration`, `CalibrationReport`, `MockTransmonSystem` (sync shortcut), `run_validation_benchmark`.
- `pulse` — `Pulse`, validation, temporal **conflict** detection on schedules.
- `schedule` — Pure functions: **when** recalibration is due (periodic vs cold start vs chip-swap priority).
- `controller` — `CalibrationController` tying **policy** to the **sync** mock machine (not used by the default `main` binary).

---

## Layer-by-layer technical description

### 1. Scheduler (`src/job_scheduler.rs`)

- **`run_calibration_cycle_parallel`**  
  - Input: slice of qubit IDs, shared `Arc<dyn QuantumHardware>`, `ParameterStore`, `Arc<Mutex<HealthMonitor>>`, `HealthThresholds`, and an **`incremental`** flag.  
  - For each qubit, spawns a **Tokio task** that runs `calibrate_one_qubit` (see `routines`).  
  - Collects successful `QubitCalibration` values into a `CalibrationReport`. Failed qubits are **omitted** from the report vector; failures are expected to be recorded via the health monitor inside the routine (the store still holds **last known good** parameters for that qubit if a previous commit succeeded).

- **`run_periodic_calibration_scheduler`**  
  - An **infinite** `tokio::time::interval` loop intended as a sketch of a long-running service process. It is **not** invoked by the default binary (to keep `cargo run` finite).

**Validation rule on the report:** `validation_passed` is true only if **every** requested qubit produced a successful calibration **and** per-qubit metrics pass fidelity, crosstalk template, and `T2 ≤ T1` checks.

---

### 2. Calibration routines (`src/routines.rs`)

Routines are **async** and use only the **`QuantumHardware`** trait, not a concrete device:

- **`routine_frequency_sweep`** — Steps a probe frequency across a coarse grid (mock: 4.8–5.4 GHz, 0.02 GHz steps), sends spectroscopy-tagged pulses, reads scalar responses, and picks the peak (Lorentzian-shaped response in the mock).

- **`routine_measure_t1` / `routine_measure_t2`** — Drive mock relaxation/dephasing experiments via tagged pulses and read nanosecond estimates.

- **`routine_gate_tune`** — Returns a mock π-pulse duration and single-qubit fidelity estimate.

- **`calibrate_one_qubit`** — Orchestrates the full pipeline for one qubit:
  - Runs frequency sweep.
  - Optionally **skips T1/T2 remeasurement** when `incremental` is true and stored parameters are already within policy (see [Incremental calibration](#incremental-calibration)).
  - Compares new drive frequency to **previous** store entry; if drift exceeds `HealthThresholds::max_freq_drift_ghz`, records a **drift warning** (still may succeed).
  - Validates fidelity, `T2 ≤ T1`, and a **crosstalk template** (see [Pulse schedules](#pulse-schedules-and-crosstalk-checks)).
  - On success: **`ParameterStore::commit_success`** (bumps generation), **`HealthMonitor::record_success`**.
  - On validation failure: **`HealthMonitor::record_failure`** (may trigger escalation after repeated failures).

**Error type:** `CalibrationError` wraps `HardwareError` or static validation messages.

---

### 3. Hardware interface (`src/hardware.rs`)

**Trait (`async_trait`):**

```rust
async fn send_pulse(&self, qubit_id: u8, pulse: Pulse) -> Result<(), HardwareError>;
async fn read_result(&self, qubit_id: u8) -> Result<Measurement, HardwareError>;
```

**`Measurement`** is an enum covering spectroscopy amplitude, T1/T2, and gate-tune results.

**Mock driver (`MockQuantumHardware`):**

- Holds per-qubit **true resonance** frequencies (GHz) and last-sent `Pulse` per line.
- Uses **`Pulse::start_ns` as a diagnostic opcode** when talking to the mock (this is a simulation convention—not a statement about real hardware APIs):

  | `start_ns` constant | Meaning |
  |---------------------|---------|
  | `diagnostic::SPECTROSCOPY` (0) | Spectroscopy probe at `pulse.frequency_ghz` |
  | `diagnostic::T1` (1) | T1 experiment |
  | `diagnostic::T2` (2) | T2 experiment |
  | `diagnostic::GATE_TUNE` (3) | Gate calibration |

**Important:** Experiment **timeline** pulses (nanosecond start times) used only in `pulse` schedule analysis are separate from this mock encoding; the crosstalk template check in `routines` does not send those pulses through the hardware trait.

**`HardwareError`:** `InvalidQubit`, `NotArmed` (e.g. read without a preceding arm pulse).

---

### 4. Parameter store (`src/parameter_store.rs`)

- **`CalibrationParams`** fields: `drive_frequency_ghz`, `t1_ns`, `t2_ns`, `pi_pulse_duration_ns`, `single_qubit_fidelity`, `generation`.
- **`ParameterStore`** is backed by `Arc<RwLock<HashMap<u8, CalibrationParams>>>` (async-friendly `tokio::sync::RwLock`).
- **`commit_success`** inserts or updates the row and sets **`generation`** to `previous + 1` (or `1` on first success).
- Failed calibration attempts **do not** clear the map entry; operators always have a **last known good** configuration until a new success overwrites it.
- **`snapshot`** returns a copy of the map for inspection or UI.

---

### 5. Health monitor (`src/health.rs`)

- **`HealthThresholds`** (defaults): `min_t1_ns = 30_000`, `max_freq_drift_ghz = 0.08`, `min_fidelity = 0.99`.
- **`Alert`**: `DriftWarning` or `EscalateHuman` (implements `Display` for logging).
- **`HealthMonitor`**:
  - **`record_success(qubit)`** resets consecutive failure count for that qubit.
  - **`record_failure(qubit, reason)`** increments failures; after **`ESCALATE_AFTER_FAILURES` (3)** consecutive failures, appends an **`EscalateHuman`** alert and resets the counter (policy choice to avoid duplicate spam; adjust in production).
  - **`warn_drift`** for frequency moves vs last good.
  - **`take_alerts`** drains the alert vector for logging or downstream notification.

---

## Data flow (one calibration cycle)

1. **Scheduler** receives a tick (or `main` calls `run_calibration_cycle_parallel` once).
2. For each qubit ID, a **task** runs **`calibrate_one_qubit`** with shared `QuantumHardware`, `ParameterStore`, and `HealthMonitor`.
3. Routines call **`send_pulse` / `read_result`** on the trait object.
4. On success, **parameter store** is updated and **health** records success; on failure, **health** records failure; store keeps prior good params.
5. **Scheduler** builds **`CalibrationReport`** from successful qubits and sets **`validation_passed`** according to global checks.

---

## Pulse schedules and crosstalk checks

Module **`pulse`** (`src/pulse.rs`):

- **`Pulse`**: `channel` (control line / AWG channel id), `start_ns` (schedule time), `frequency_ghz`.
- **`validate_pulse`**: channel ≤ 7, frequency in `(0, 10]` GHz (exercise-style bounds).
- **`sort_pulses_by_start_ns`**, **`pulses_conflict`**, **`schedule_has_conflicts`**: two pulses on the **same channel** with start times within **50 ns** are treated as a **temporal conflict** (O(n²) pairwise check; documented as acceptable for small schedules; production would sort by channel and window-scan).
- Used by **`routines`** to assert a **safe template** schedule before trusting crosstalk posture for that mock scenario.

---

## Supporting modules (library)

### `calibration` (`src/calibration.rs`)

- Shared report types used by both the async pipeline and the sync mock.
- **`MockTransmonSystem::run_full_calibration_suite`**: synchronous, all-qubit suite using the same numerical models as the mock hardware (useful for fast tests without Tokio).
- **`run_validation_benchmark`**: returns whether `CalibrationReport::validation_passed` is true.

### `schedule` (`src/schedule.rs`)

- **`CalibrationPolicy`**: `periodic_interval` (`std::time::Duration`).
- **`calibration_due`**: returns `Some(RecalibrationCause)` when:
  - **Chip swap** is pending (highest priority), or
  - No prior calibration exists (**cold start** → periodic), or
  - Elapsed time since last success ≥ policy interval.

Unit tests cover chip-swap priority, cold start, and “recently calibrated → not due.”

### `controller` (`src/controller.rs`)

- **`CalibrationController`**: combines policy with **`std::time::Instant`** timestamps and a **pending chip-swap** flag; **`run_full_calibration_if_due`** drives the **sync** `MockTransmonSystem`.  
- Suitable for simple simulations or bridging to a non-async subsystem; the default **`main`** uses the **async five-layer** path instead.

---

## Incremental calibration

When **`incremental == true`** in `run_calibration_cycle_parallel`, **`maybe_measure_t1_t2`** in `routines` may **reuse** `t1_ns` and `t2_ns` from the parameter store if they already satisfy **`t1_ns ≥ min_t1_ns`** and **`t2_ns ≤ t1_ns`**, avoiding redundant relaxation experiments.

**Production note:** A real system would still run a **cheap sanity check** (short Ramsey or spot-check) before trusting stale T1/T2; the mock documents the *intent* to reduce unnecessary work when vitals remain in range.

---

## Testing

| Test area | Location | What it checks |
|-----------|----------|----------------|
| Spectroscopy peak | `calibration::tests` | Sweep finds frequency near ground truth |
| Sync full suite | `calibration::tests` | `MockTransmonSystem` report validity |
| Schedule policy | `schedule::tests` | Chip swap, cold start, suppression after recent cal |
| Parallel async cycle | `job_scheduler::tests` | Two qubits, store populated, generations ≥ 1, validation passes |

Run all tests with `cargo test`.

---

## Design tradeoffs and production notes

- **Parallelism:** Per-qubit tasks reduce wall-clock time versus strictly sequential calibration; contention on shared real hardware would require **serialization** or **resource locks** not modeled here.
- **O(n²) conflict detection:** Documented in `pulse`; fine for small `n`; scale with **sort + sliding window** per channel.
- **Mock vs real hardware:** The trait is the integration seam; real stacks would add timeouts, DMA, waveform upload, IQ demodulation, and error models.
- **Persistence:** The in-memory `ParameterStore` would become a database or file with **versioning** and audit fields in production.
- **Escalation policy:** The “3 strikes” rule is illustrative; real ops would integrate paging, ticket systems, and optional **automatic rollback** to last good parameters for execution paths.

---

## Repository layout

```
Cargo.toml
Cargo.lock
README.md
src/
  lib.rs              # Crate docs, module exports
  main.rs             # Binary: async calibration pipeline demo
  job_scheduler.rs    # Layer 1: parallel cycle + optional periodic loop
  routines.rs         # Layer 2: async calibration steps + orchestration
  hardware.rs         # Layer 3: trait + mock quantum hardware
  parameter_store.rs  # Layer 4: last known good parameters
  health.rs           # Layer 5: thresholds, alerts, escalation
  calibration.rs      # Shared types + sync mock suite
  pulse.rs            # Pulse schedule + conflict detection
  schedule.rs           # When to recalibrate (pure policy)
  controller.rs         # Sync controller + MockTransmonSystem driver
```

---

## License / naming

The package name in `Cargo.toml` is **`test_rust_project`** (workspace default). The **module and type names** inside `src/` are already domain-oriented (`QuantumHardware`, `CalibrationParams`, etc.).
