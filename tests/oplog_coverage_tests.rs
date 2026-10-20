//! Op-log coverage tests (s3dlio >= 0.9.122: one owner per op-log row)
//!
//! Since s3dlio 0.9.122 only stores built WITH a logger write op-log rows; the
//! storage backends no longer write their own. These tests pin that the storage
//! operations of the prepare, workload and cleanup phases (and the cloud
//! metadata-cache checkpoint PUT/restore GET, which go through the same factory)
//! are recorded exactly once when `--op-log` is active, that the replay helpers stay
//! unlogged, and that nothing is recorded by a store built while no op-log is active.
//! The file:// metadata-cache checkpoint PUT is a direct filesystem write and is not
//! an object-store operation, so it has no row; the restore GET does.
//!
//! NOT covered by the op-log (use stores that bypass the factory, unchanged here):
//! preflight probes, agent gRPC get/list, raw `MultiEndpointStore` pattern LISTs,
//! staged-agent prepare/cleanup that runs before the op-log is initialized, and
//! multiprocess post-run cleanup that runs after finalize. Multi-endpoint stores are
//! deliberately not wrapped (s3dlio logs their bucket-form GET/PUT itself); see
//! `oplog_multi_endpoint_tests.rs`.
//!
//! s3dlio's op-logger is a process-global singleton. `end_to_end_*` tests drive the
//! `sai3-bench` binary (one process per run); `factory_*` tests use the in-process
//! logger and are serialized.

use bytes::Bytes;
use sai3_bench::config::PageCacheMode;
use sai3_bench::workload::{
    create_store_for_uri, create_store_for_uri_with_config, create_store_with_logger_and_config,
    delete_object_no_log, finalize_operation_logger, get_object_no_log, init_operation_logger,
    put_object_no_log,
};
use serial_test::serial;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const HEADER: &str =
    "idx\tthread\top\tclient_id\tn_objects\tbytes\tendpoint\tfile\terror\tstart\tfirst_byte\tend\tduration_ns";

const PREPARED: usize = 6;

/// One data row of an op-log.
#[derive(Debug)]
struct Row {
    op: String,
    file: String,
}

/// Decompress an op-log, check its header, and return its data rows.
fn read_oplog(path: &Path) -> Vec<Row> {
    let compressed = std::fs::read(path)
        .unwrap_or_else(|e| panic!("op-log {} must exist: {}", path.display(), e));
    let raw = zstd::stream::decode_all(&compressed[..]).unwrap_or_else(|e| {
        panic!(
            "op-log {} must be a complete zstd stream: {}",
            path.display(),
            e
        )
    });
    let text = String::from_utf8(raw).expect("op-log must be UTF-8");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(HEADER), "op-log header line");
    lines
        .map(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            assert_eq!(cols.len(), 13, "op-log row must have 13 columns: {l:?}");
            Row {
                op: cols[2].to_string(),
                file: cols[7].to_string(),
            }
        })
        .collect()
}

fn count(rows: &[Row], op: &str) -> usize {
    rows.iter().filter(|r| r.op == op).count()
}

/// File basenames of `prepared-*.dat` rows with the given op.
fn prepared_files(rows: &[Row], op: &str) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| r.op == op)
        .filter_map(|r| r.file.rsplit('/').next())
        .filter(|f| f.starts_with("prepared-") && f.ends_with(".dat"))
        .map(str::to_string)
        .collect()
}

/// Run `sai3-bench --op-log <log> run --config <cfg>` with cwd = `dir`
/// (results directories are written to the cwd). Returns stdout.
fn run_bench(dir: &Path, log: &Path, cfg: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sai3-bench"))
        .current_dir(dir)
        .arg("--op-log")
        .arg(log)
        .arg("run")
        .arg("--config")
        .arg(cfg)
        .output()
        .expect("spawn sai3-bench");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "sai3-bench failed: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Parse the `  GET: <n> ops` line of the "Actual Operation Distribution" block.
fn reported_ops(stdout: &str, op: &str) -> usize {
    let prefix = format!("  {op}: ");
    stdout
        .lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|n| n.replace(',', "").parse::<usize>().expect("op count"))
        .unwrap_or_else(|| panic!("no '{op}' count in sai3-bench output:\n{stdout}"))
}

// =============================================================================
// End to end: prepare + workload + cleanup through the binary
// =============================================================================

#[test]
fn end_to_end_prepare_workload_cleanup_are_all_in_the_oplog() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let uri = format!("file://{}/", data.display());

    let cfg = tmp.path().join("cfg.yaml");
    std::fs::write(
        &cfg,
        format!(
            r#"target: "{uri}"
duration: 1s
concurrency: 2
prepare:
  prepare_strategy: sequential
  ensure_objects:
    - base_uri: "{uri}"
      count: {PREPARED}
      size_spec: 1024
      fill: random
  cleanup: true
workload:
  - op: get
    path: "prepared-*.dat"
    weight: 100
"#
        ),
    )
    .unwrap();

    let log = tmp.path().join("ops.tsv.zst");
    let stdout = run_bench(tmp.path(), &log, &cfg);
    let rows = read_oplog(&log);

    // Prepare: one PUT row per prepared object, no more (no duplicate owner).
    assert_eq!(
        count(&rows, "PUT"),
        PREPARED,
        "one PUT row per prepared object"
    );
    assert_eq!(
        prepared_files(&rows, "PUT").len(),
        PREPARED,
        "PUT rows must name every prepared object"
    );

    // Cleanup: one DELETE row per prepared object.
    assert_eq!(
        count(&rows, "DELETE"),
        PREPARED,
        "one DELETE row per cleaned object"
    );
    assert_eq!(
        prepared_files(&rows, "DELETE"),
        prepared_files(&rows, "PUT"),
        "DELETE rows must name exactly the prepared objects"
    );

    // Workload: exactly the GETs sai3-bench reports (single owner, no double rows).
    let workload_gets = reported_ops(&stdout, "GET");
    assert!(workload_gets > 0, "workload must have run GETs");
    let get_rows_on_prepared = rows
        .iter()
        .filter(|r| r.op == "GET" && r.file.contains("/prepared-"))
        .count();
    assert_eq!(
        get_rows_on_prepared, workload_gets,
        "GET rows on prepared objects must equal the workload GETs sai3-bench reports"
    );
}

/// The kv-cache checkpoint written by run 1 (`sai3-kv-cache.tar.zst`) is read back
/// by run 2: that GET is a storage operation and must be in run 2's op-log.
#[test]
fn end_to_end_kv_cache_checkpoint_get_is_in_the_oplog() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let uri = format!("file://{}/", data.display());

    let cfg = tmp.path().join("cfg.yaml");
    std::fs::write(
        &cfg,
        format!(
            r#"target: "{uri}"
duration: 1s
concurrency: 2
prepare:
  prepare_strategy: sequential
  ensure_objects:
    - base_uri: "{uri}"
      count: {PREPARED}
      size_spec: 1024
      fill: random
  cleanup: false
workload:
  - op: get
    path: "prepared-*.dat"
    weight: 100
"#
        ),
    )
    .unwrap();

    run_bench(tmp.path(), &tmp.path().join("ops1.tsv.zst"), &cfg);
    assert!(
        data.join("sai3-kv-cache.tar.zst").exists(),
        "run 1 must leave a kv-cache checkpoint on storage"
    );

    let log2 = tmp.path().join("ops2.tsv.zst");
    run_bench(tmp.path(), &log2, &cfg);
    let rows = read_oplog(&log2);
    let kv_gets = rows
        .iter()
        .filter(|r| r.op == "GET" && r.file.ends_with("sai3-kv-cache.tar.zst"))
        .count();
    assert_eq!(
        kv_gets, 1,
        "the kv-cache checkpoint restore GET must be recorded exactly once"
    );
}

// =============================================================================
// Factory level (in-process op-log): serialized, ONE init/finalize per process
// =============================================================================

/// Every factory that benchmark phases use must hand out a logged store while the
/// op-log is active (config preserved), and the replay helpers must stay unlogged.
/// One test function: the op-logger is a process-global that initializes once.
#[test]
#[serial]
fn factory_stores_are_logged_when_oplog_active_and_replay_helpers_are_not() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let log = tmp.path().join("factory.tsv.zst");

    // A store built while no op-log is active is the plain store and stays unlogged.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let s = create_store_for_uri(&format!("file://{}/", dir.display())).unwrap();
        s.put(
            &format!("file://{}/before.dat", dir.display()),
            Bytes::from_static(b"before"),
        )
        .await
        .unwrap();
    });

    init_operation_logger(&log).unwrap();
    rt.block_on(async {
        // create_store_for_uri (prepare, cleanup, directory tree, metadata cache)
        let s = create_store_for_uri(&format!("file://{}/", dir.display())).unwrap();
        s.put(
            &format!("file://{}/plain.dat", dir.display()),
            Bytes::from_static(b"plain"),
        )
        .await
        .unwrap();

        // create_store_for_uri_with_config with a page cache mode
        let s = create_store_for_uri_with_config(
            &format!("file://{}/", dir.display()),
            None,
            Some(PageCacheMode::Sequential),
        )
        .unwrap();
        s.put(
            &format!("file://{}/cfg.dat", dir.display()),
            Bytes::from_static(b"cfg"),
        )
        .await
        .unwrap();
        s.get(&format!("file://{}/cfg.dat", dir.display()))
            .await
            .unwrap();

        // workload factory with a page cache mode: logged exactly once, not twice
        let s = create_store_with_logger_and_config(
            &format!("file://{}/", dir.display()),
            None,
            Some(PageCacheMode::DontNeed),
        )
        .unwrap();
        s.put(
            &format!("file://{}/wl.dat", dir.display()),
            Bytes::from_static(b"wl"),
        )
        .await
        .unwrap();
        s.delete(&format!("file://{}/wl.dat", dir.display()))
            .await
            .unwrap();

        // replay helpers: must NOT be logged
        let replay_uri = format!("file://{}/replay.dat", dir.display());
        put_object_no_log(&replay_uri, Bytes::from_static(b"replay"))
            .await
            .unwrap();
        get_object_no_log(&replay_uri).await.unwrap();
        delete_object_no_log(&replay_uri).await.unwrap();
    });
    finalize_operation_logger().unwrap();

    let rows = read_oplog(&log);
    let for_file = |name: &str, op: &str| {
        rows.iter()
            .filter(|r| r.op == op && r.file.ends_with(name))
            .count()
    };
    assert_eq!(for_file("/plain.dat", "PUT"), 1, "create_store_for_uri PUT");
    assert_eq!(for_file("/cfg.dat", "PUT"), 1, "with_config PUT");
    assert_eq!(for_file("/cfg.dat", "GET"), 1, "with_config GET");
    assert_eq!(for_file("/wl.dat", "PUT"), 1, "workload factory PUT (once)");
    assert_eq!(
        for_file("/wl.dat", "DELETE"),
        1,
        "workload factory DELETE (once)"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.file.ends_with("/replay.dat"))
            .count(),
        0,
        "replay (*_no_log) helpers must not write op-log rows"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.file.ends_with("/before.dat"))
            .count(),
        0,
        "a store built before the op-log was active is unlogged"
    );
    assert_eq!(
        rows.len(),
        5,
        "exactly the five logged operations: {rows:?}"
    );
}
