use crate::fixture::{BenchmarkRow, DemoRun};
use crate::model::{EngineMetrics, Reservation, ReservationStatus, WaitlistPolicy};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("could not create report directory: {0}")]
    CreateDir(#[source] std::io::Error),
    #[error("could not write report file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not serialize report: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Serialize)]
pub struct FileDigest {
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub project: &'static str,
    pub files: BTreeMap<String, FileDigest>,
    pub payload_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportSummary {
    pub output_dir: String,
    pub payload_sha256: String,
    pub files: Vec<String>,
}

pub fn write_demo_report(run: &DemoRun, output_dir: &Path) -> Result<ReportSummary, ReportError> {
    fs::create_dir_all(output_dir).map_err(ReportError::CreateDir)?;
    let snapshot = json_bytes(&run.engine.snapshot())?;
    let audit = json_bytes(&run.engine.audit())?;
    let operations = json_bytes(&run.operations)?;
    let metrics = json_bytes(&run.engine.metrics())?;
    let reservations = reservations_csv(run.engine.reservations());
    let metrics_csv = metrics_csv(run.engine.metrics());
    let summary = demo_summary(run);

    let artifacts = [
        ("snapshot.json", snapshot),
        ("audit.json", audit),
        ("operations.json", operations),
        ("metrics.json", metrics),
        ("reservations.csv", reservations.into_bytes()),
        ("metrics.csv", metrics_csv.into_bytes()),
        ("summary.md", summary.into_bytes()),
    ];
    write_manifest(output_dir, artifacts)
}

pub fn write_benchmark_report(
    rows: &[BenchmarkRow],
    output_dir: &Path,
) -> Result<ReportSummary, ReportError> {
    fs::create_dir_all(output_dir).map_err(ReportError::CreateDir)?;
    let benchmark = json_bytes(rows)?;
    let csv = benchmark_csv(rows);
    let summary = benchmark_summary(rows);
    let artifacts = [
        ("benchmark.json", benchmark),
        ("benchmark.csv", csv.into_bytes()),
        ("summary.md", summary.into_bytes()),
    ];
    write_manifest(output_dir, artifacts)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_manifest<const N: usize>(
    output_dir: &Path,
    artifacts: [(&str, Vec<u8>); N],
) -> Result<ReportSummary, ReportError> {
    let mut files = BTreeMap::new();
    for (name, bytes) in artifacts {
        let path = output_dir.join(name);
        fs::write(&path, &bytes).map_err(|source| ReportError::Write { path, source })?;
        files.insert(
            name.to_string(),
            FileDigest {
                bytes: bytes.len(),
                sha256: sha256_hex(&bytes),
            },
        );
    }
    let payload = files
        .iter()
        .map(|(name, digest)| format!("{name}\0{}\n", digest.sha256))
        .collect::<String>();
    let manifest = Manifest {
        schema_version: 1,
        project: "slotforge",
        files,
        payload_sha256: sha256_hex(payload.as_bytes()),
    };
    let payload_sha256 = manifest.payload_sha256.clone();
    let manifest_bytes = json_bytes(&manifest)?;
    let manifest_path = output_dir.join("manifest.json");
    fs::write(&manifest_path, manifest_bytes).map_err(|source| ReportError::Write {
        path: manifest_path,
        source,
    })?;
    let mut file_names: Vec<String> = manifest.files.keys().cloned().collect();
    file_names.push("manifest.json".to_string());
    Ok(ReportSummary {
        output_dir: output_dir.display().to_string(),
        payload_sha256,
        files: file_names,
    })
}

fn json_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, ReportError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn reservations_csv<'a, I>(reservations: I) -> String
where
    I: IntoIterator<Item = &'a Reservation>,
{
    let mut output = String::from(
        "reservation_id,event_id,attendee_id,cohort,status,requested_at,confirmed_at,wait_seconds,checked_in_at\n",
    );
    for reservation in reservations {
        output.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            reservation.reservation_id,
            csv_field(&reservation.event_id),
            csv_field(&reservation.attendee_id),
            csv_field(&reservation.cohort),
            status_name(reservation.status),
            reservation.requested_at,
            optional_i64(reservation.confirmed_at),
            optional_i64(reservation.wait_seconds),
            optional_i64(reservation.checked_in_at),
        ));
    }
    output
}

fn metrics_csv(metrics: EngineMetrics) -> String {
    format!(
        "metric,value\nevents,{}\nreservations_total,{}\nconfirmed,{}\nwaitlisted,{}\nchecked_in,{}\ncancelled,{}\npromotions,{}\nidempotent_replays,{}\ncapacity_slots,{}\noccupied_slots,{}\nutilization,{:.6}\nwaitlist_rate,{:.6}\nfairness_index,{:.6}\nattendee_fairness_index,{:.6}\nmax_wait_seconds,{}\n",
        metrics.events,
        metrics.reservations_total,
        metrics.confirmed,
        metrics.waitlisted,
        metrics.checked_in,
        metrics.cancelled,
        metrics.promotions,
        metrics.idempotent_replays,
        metrics.capacity_slots,
        metrics.occupied_slots,
        metrics.utilization,
        metrics.waitlist_rate,
        metrics.fairness_index,
        metrics.attendee_fairness_index,
        metrics.max_wait_seconds,
    )
}

fn benchmark_csv(rows: &[BenchmarkRow]) -> String {
    let mut output = String::from(
        "policy,requests,reservations_total,confirmed,waitlisted,cancelled,promotions,idempotent_replays,utilization,fairness_index,attendee_fairness_index,max_wait_seconds\n",
    );
    for row in rows {
        output.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{}\n",
            policy_name(row.policy),
            row.requests,
            row.reservations_total,
            row.confirmed,
            row.waitlisted,
            row.cancelled,
            row.promotions,
            row.idempotent_replays,
            row.utilization,
            row.fairness_index,
            row.attendee_fairness_index,
            row.max_wait_seconds,
        ));
    }
    output
}

fn demo_summary(run: &DemoRun) -> String {
    let metrics = run.engine.metrics();
    format!(
        "# SlotForge demo\n\nA deterministic reservation ledger with role checks, idempotent retries, and policy-driven waitlist promotion.\n\n## Run facts\n\n- Policy: `{}`\n- Operations recorded: `{}`\n- Expected rejected operations: `{}`\n- Reservations: `{}` total (`{}` confirmed, `{}` waitlisted, `{}` checked in, `{}` cancelled)\n- Promotions: `{}`\n- Idempotent replays: `{}`\n- Capacity utilization: `{:.3}`\n- Cohort Jain fairness index: `{:.3}`\n- Maximum wait: `{}` seconds\n\nThe fixture contains opaque synthetic identifiers only. It is a behavioral demonstration, not a claim about a production campus system.\n",
        policy_name(run.engine.config().policy),
        run.operations.len(),
        run.expected_rejections,
        metrics.reservations_total,
        metrics.confirmed,
        metrics.waitlisted,
        metrics.checked_in,
        metrics.cancelled,
        metrics.promotions,
        metrics.idempotent_replays,
        metrics.utilization,
        metrics.fairness_index,
        metrics.max_wait_seconds,
    )
}

fn benchmark_summary(rows: &[BenchmarkRow]) -> String {
    let mut output = String::from(
        "# SlotForge benchmark\n\nThe same synthetic request stream is replayed under FIFO and fair waitlist policies. Lower wait is not sufficient: the report keeps capacity utilization, promotions, and cohort-level Jain fairness together.\n\n| Policy | Confirmed | Waitlisted | Promotions | Utilization | Cohort fairness | Max wait (s) |\n|---|---:|---:|---:|---:|---:|---:|\n",
    );
    for row in rows {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {:.3} | {:.3} | {} |\n",
            policy_name(row.policy),
            row.confirmed,
            row.waitlisted,
            row.promotions,
            row.utilization,
            row.fairness_index,
            row.max_wait_seconds,
        ));
    }
    output.push_str(
        "\nAll rows are generated from fixed synthetic events and opaque attendee IDs. No real demand, identity, or commercial uplift is inferred.\n",
    );
    output
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn optional_i64(value: Option<i64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn status_name(status: ReservationStatus) -> &'static str {
    match status {
        ReservationStatus::Confirmed => "confirmed",
        ReservationStatus::Waitlisted => "waitlisted",
        ReservationStatus::CheckedIn => "checked_in",
        ReservationStatus::Cancelled => "cancelled",
    }
}

fn policy_name(policy: WaitlistPolicy) -> &'static str {
    match policy {
        WaitlistPolicy::Fifo => "fifo",
        WaitlistPolicy::Fair => "fair",
    }
}
