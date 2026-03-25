//! Thin binary: all logic lives in the `quantum_calibration` library (`demo` module).

#[tokio::main]
async fn main() {
    quantum_calibration::demo::run_all_demos().await;
}
