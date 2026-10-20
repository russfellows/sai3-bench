//! Replay policy for op-log rows that s3dlio-oplog does not model.
//!
//! `DELETE_PREFIX` (what `rmdir` writes on file:// / direct://), `GET_RANGE`,
//! `DELETE_BATCH`, `CREATE_CONTAINER` and `DELETE_CONTAINER` stay unsupported: they are
//! never translated into DELETE. The reader reports each such row as an error naming the
//! file line, the row index and the operation. Replay must treat that error the same way
//! wherever the row is, including as the FIRST row:
//!
//! * with `--continue-on-error`: warn, count it in `skipped_operations`, replay the rest;
//! * without it: stop with an error that names the op, the row and "not supported".
//!
//! There is no pre-scan, so operations before a later unsupported row have already run
//! when replay stops there. Operations scheduled for LATER times than the failing row
//! must not run after the error is returned: strict replay aborts them first.

use sai3_bench::replay_streaming::{replay_workload_streaming, ReplayRunConfig};
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

const HEADER: &str =
    "idx\tthread\top\tclient_id\tn_objects\tbytes\tendpoint\tfile\terror\tstart\tfirst_byte\tend\tduration_ns";

/// One TSV row: `op` on `file`, `n` * 100 ms after the epoch.
fn row(idx: usize, op: &str, file: &str, bytes: u64) -> String {
    let ts = format!("2026-01-01T00:00:00.{:03}000000Z", idx * 100);
    format!("{idx}\t1\t{op}\t\t1\t{bytes}\tfile://\t{file}\t\t{ts}\t\t{ts}\t1000")
}

fn write_trace(dir: &Path, rows: &[String]) -> std::path::PathBuf {
    let path = dir.join("trace.tsv");
    let mut text = String::from(HEADER);
    text.push('\n');
    for r in rows {
        text.push_str(r);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn config(trace: std::path::PathBuf, continue_on_error: bool) -> ReplayRunConfig {
    ReplayRunConfig {
        op_log_path: trace,
        target_uri: None,
        speed: 10.0,
        continue_on_error,
        max_concurrent: Some(8),
        remap_config: None,
        backpressure: None,
    }
}

fn data_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// First row DELETE_PREFIX + `--continue-on-error`: skipped and counted, rest replays.
#[tokio::test]
async fn first_row_unsupported_is_skipped_and_counted_with_continue_on_error() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let d = data.display();
    let trace = write_trace(
        tmp.path(),
        &[
            row(0, "DELETE_PREFIX", &format!("{d}/"), 0),
            row(1, "PUT", &format!("{d}/a.dat"), 1024),
            row(2, "PUT", &format!("{d}/b.dat"), 2048),
        ],
    );

    let stats = replay_workload_streaming(config(trace, true))
        .await
        .expect("first-row unsupported op must honour --continue-on-error");
    assert_eq!(
        stats.skipped_operations, 1,
        "the DELETE_PREFIX row is counted"
    );
    assert_eq!(stats.total_operations, 2, "the two PUT rows are replayed");
    assert_eq!(stats.completed_operations, 2);
    assert_eq!(stats.failed_operations, 0);
    assert_eq!(data_files(&data), vec!["a.dat", "b.dat"]);
}

/// First row DELETE_PREFIX, no `--continue-on-error`: clear error, nothing ran.
#[tokio::test]
async fn first_row_unsupported_without_continue_on_error_names_op_row_and_support() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let d = data.display();
    let trace = write_trace(
        tmp.path(),
        &[
            row(0, "DELETE_PREFIX", &format!("{d}/"), 0),
            row(1, "PUT", &format!("{d}/a.dat"), 1024),
        ],
    );

    let err = replay_workload_streaming(config(trace, false))
        .await
        .expect_err("must stop at an unsupported first row");
    // The outermost message (what is shown without expanding the error chain).
    let shown = err.to_string();
    assert!(shown.contains("DELETE_PREFIX"), "names the op: {shown}");
    assert!(
        shown.contains("not supported"),
        "says it is not supported: {shown}"
    );
    assert!(shown.contains("line 2"), "names the file line: {shown}");
    assert!(
        shown.contains("row index 0"),
        "names the row index: {shown}"
    );
    assert!(
        shown.contains("--continue-on-error"),
        "points at the flag: {shown}"
    );
    assert!(
        data_files(&data).is_empty(),
        "nothing ran before the first row"
    );
}

/// The same row later in the trace behaves the same way (control for the policy).
#[tokio::test]
async fn later_row_unsupported_follows_the_same_policy() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let d = data.display();
    let rows = [
        row(0, "PUT", &format!("{d}/a.dat"), 1024),
        row(1, "DELETE_PREFIX", &format!("{d}/"), 0),
        row(2, "PUT", &format!("{d}/b.dat"), 2048),
    ];
    let trace = write_trace(tmp.path(), &rows);

    let stats = replay_workload_streaming(config(trace.clone(), true))
        .await
        .expect("--continue-on-error skips the row");
    assert_eq!(stats.skipped_operations, 1);
    assert_eq!(stats.total_operations, 2);
    assert_eq!(data_files(&data), vec!["a.dat", "b.dat"]);

    let err = replay_workload_streaming(config(trace, false))
        .await
        .expect_err("stops at the unsupported row");
    let shown = err.to_string();
    assert!(
        shown.contains("DELETE_PREFIX")
            && shown.contains("row index 1")
            && shown.contains("line 3"),
        "{shown}"
    );
}

/// A strict failure must cancel operations that are scheduled for a later time. Row 0
/// (the epoch) runs at once; row 1 is a PUT scheduled 3 s later; row 2 is the unsupported
/// row, read immediately. After the error is returned the PUT must never happen, even
/// though the runtime is still alive past its scheduled time.
#[tokio::test]
async fn strict_failure_cancels_operations_scheduled_for_later() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let d = data.display();
    let at = |idx: usize, op: &str, file: &str, bytes: u64, ms: u32| {
        let ts = format!("2026-01-01T00:00:{:02}.{:03}000000Z", ms / 1000, ms % 1000);
        format!("{idx}\t1\t{op}\t\t1\t{bytes}\tfile://\t{file}\t\t{ts}\t\t{ts}\t1000")
    };
    let trace = write_trace(
        tmp.path(),
        &[
            at(0, "PUT", &format!("{d}/epoch.dat"), 1024, 0),
            at(1, "PUT", &format!("{d}/late.dat"), 1024, 3000),
            at(2, "DELETE_PREFIX", &format!("{d}/"), 0, 3100),
        ],
    );

    let mut cfg = config(trace, false);
    cfg.speed = 1.0;
    let err = replay_workload_streaming(cfg)
        .await
        .expect_err("strict replay must stop at the unsupported row");
    let shown = err.to_string();
    assert!(shown.contains("DELETE_PREFIX"), "{shown}");

    // Wait well past the PUT's scheduled time (3 s after the epoch).
    tokio::time::sleep(Duration::from_secs(5)).await;
    let files = data_files(&data);
    assert!(
        !files.contains(&"late.dat".to_string()),
        "the PUT scheduled after the failing row must not run: {files:?}"
    );
    assert!(
        shown.contains("may have changed storage"),
        "error must say submitted operations may have changed storage: {shown}"
    );
}
