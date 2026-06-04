use slotforge::fixture::{run_benchmark, run_demo};
use slotforge::report::{write_benchmark_report, write_demo_report};
use slotforge::WaitlistPolicy;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn demo_report_contains_hash_bound_artifacts() {
    let directory = tempdir().expect("temporary directory");
    let summary = write_demo_report(
        &run_demo(WaitlistPolicy::Fair).expect("demo fixture is valid"),
        directory.path(),
    )
    .expect("report writes");
    assert!(summary.payload_sha256.len() == 64);
    for name in [
        "snapshot.json",
        "audit.json",
        "operations.json",
        "metrics.json",
        "metrics.csv",
        "reservations.csv",
        "summary.md",
        "manifest.json",
    ] {
        assert!(directory.path().join(name).is_file(), "missing {name}");
    }
    let manifest = fs::read_to_string(directory.path().join("manifest.json")).expect("manifest");
    assert!(manifest.contains(&summary.payload_sha256));
}

#[test]
fn benchmark_report_is_repeatable() {
    let first_dir = tempdir().expect("temporary directory");
    let second_dir = tempdir().expect("temporary directory");
    let rows = run_benchmark().expect("benchmark fixture is valid");
    let first = write_benchmark_report(&rows, first_dir.path()).expect("first report");
    let second = write_benchmark_report(&rows, second_dir.path()).expect("second report");
    assert_eq!(first.payload_sha256, second.payload_sha256);
    assert_eq!(
        fs::read(first_dir.path().join("benchmark.json")).expect("first json"),
        fs::read(second_dir.path().join("benchmark.json")).expect("second json")
    );
}

#[test]
fn csv_exports_have_stable_headers() {
    let directory = tempdir().expect("temporary directory");
    write_demo_report(
        &run_demo(WaitlistPolicy::Fifo).expect("demo fixture is valid"),
        directory.path(),
    )
    .expect("report writes");
    let metrics = fs::read_to_string(directory.path().join("metrics.csv")).expect("metrics csv");
    let reservations =
        fs::read_to_string(directory.path().join("reservations.csv")).expect("reservations csv");
    assert!(metrics.starts_with("metric,value\n"));
    assert!(reservations.starts_with("reservation_id,event_id,attendee_id,cohort,status"));
}

#[test]
fn cli_demo_emits_machine_readable_summary() {
    let directory = tempdir().expect("temporary directory");
    let output = Command::new(env!("CARGO_BIN_EXE_slotforge"))
        .args([
            "demo",
            "--output-dir",
            directory.path().to_str().expect("utf-8 path"),
            "--policy",
            "fair",
        ])
        .output()
        .expect("cli launches");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 summary");
    assert!(stdout.contains("payload_sha256"));
    assert!(directory.path().join("manifest.json").is_file());
}

#[test]
fn cli_benchmark_emits_both_policies() {
    let directory = tempdir().expect("temporary directory");
    let output = Command::new(env!("CARGO_BIN_EXE_slotforge"))
        .args([
            "benchmark",
            "--output-dir",
            directory.path().to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("cli launches");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let benchmark =
        fs::read_to_string(directory.path().join("benchmark.json")).expect("benchmark json");
    assert!(benchmark.contains("\"fifo\""));
    assert!(benchmark.contains("\"fair\""));
}
