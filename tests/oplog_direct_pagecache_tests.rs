//! `direct://` + `page_cache_mode` through the store factory (Blue P1).
//!
//! `page_cache_mode` configures posix_fadvise hints, which only exist for `file://`.
//! A `direct://` store opens its files with O_DIRECT (no page cache), so the mode does
//! not apply to it. The factory must build the proper O_DIRECT store for `direct://`
//! even when a `page_cache_mode` is set, with and without an active op-log (the logged
//! workload path uses the same builder since the op-log coverage fix).
//!
//! O_DIRECT needs a filesystem that supports it (tmpfs does not): the test directory is
//! `$SAI3_TEST_DIRECT_DIR` or the package root, and the test is skipped (with a message)
//! only when O_DIRECT cannot be opened there.
//!
//! The op-logger is a process-global that initializes once, so the logged and unlogged
//! checks live in ONE test function (inactive first, then active).

use bytes::Bytes;
use sai3_bench::config::PageCacheMode;
use sai3_bench::workload::{
    create_store_for_uri_with_config, create_store_with_logger_and_config,
    finalize_operation_logger, init_operation_logger,
};
use serial_test::serial;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const OBJ_LEN: usize = 16 * 1024; // page-aligned

fn base_dir() -> PathBuf {
    std::env::var_os("SAI3_TEST_DIRECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn odirect_supported(dir: &Path) -> bool {
    let probe = dir.join(".odirect-probe");
    let ok = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_DIRECT)
        .open(&probe)
        .is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn payload(seed: u8) -> Bytes {
    Bytes::from(vec![seed; OBJ_LEN])
}

/// PUT then GET one aligned object through `store`; the bytes must round-trip.
async fn put_get(store: &dyn s3dlio::ObjectStore, uri: &str, seed: u8) {
    store
        .put(uri, payload(seed))
        .await
        .unwrap_or_else(|e| panic!("direct:// PUT {uri} failed: {e:#}"));
    let got = store
        .get(uri)
        .await
        .unwrap_or_else(|e| panic!("direct:// GET {uri} failed: {e:#}"));
    assert_eq!(got.len(), OBJ_LEN, "GET length");
    assert!(got.iter().all(|b| *b == seed), "GET bytes round-trip");
}

fn read_rows(path: &Path) -> Vec<(String, String)> {
    let compressed = std::fs::read(path).expect("op-log must exist");
    let raw = zstd::stream::decode_all(&compressed[..]).expect("complete zstd stream");
    let text = String::from_utf8(raw).unwrap();
    text.lines()
        .skip(1)
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[2].to_string(), c[7].to_string())
        })
        .collect()
}

#[test]
#[serial]
fn direct_uri_with_page_cache_mode_works_with_and_without_oplog() {
    let base = base_dir();
    if !odirect_supported(&base) {
        eprintln!("SKIP: O_DIRECT unsupported in {}", base.display());
        return;
    }
    let tmp = TempDir::new_in(&base).unwrap();
    let data = tmp.path().join("d");
    std::fs::create_dir(&data).unwrap();
    let uri = |name: &str| format!("direct://{}/{}", data.display(), name);
    let dir_uri = format!("direct://{}/", data.display());
    let rt = tokio::runtime::Runtime::new().unwrap();

    // ---- op-log inactive: both factories ----
    rt.block_on(async {
        for (i, mode) in [
            PageCacheMode::Auto,
            PageCacheMode::Sequential,
            PageCacheMode::DontNeed,
        ]
        .into_iter()
        .enumerate()
        {
            let s = create_store_for_uri_with_config(&dir_uri, None, Some(mode))
                .unwrap_or_else(|e| panic!("factory (no op-log) {mode:?}: {e:#}"));
            put_get(s.as_ref(), &uri(&format!("plain-{i}.dat")), 1).await;
        }
        let s = create_store_with_logger_and_config(&dir_uri, None, Some(PageCacheMode::Random))
            .unwrap_or_else(|e| panic!("workload factory (no op-log): {e:#}"));
        put_get(s.as_ref(), &uri("wl-nolog.dat"), 2).await;
    });

    // ---- op-log active: exactly one PUT row and one GET row per object ----
    let log = tmp.path().join("direct.tsv.zst");
    init_operation_logger(&log).unwrap();
    rt.block_on(async {
        let s = create_store_for_uri_with_config(&dir_uri, None, Some(PageCacheMode::Sequential))
            .unwrap_or_else(|e| panic!("factory (op-log) : {e:#}"));
        put_get(s.as_ref(), &uri("logged-a.dat"), 3).await;
        let s = create_store_with_logger_and_config(&dir_uri, None, Some(PageCacheMode::DontNeed))
            .unwrap_or_else(|e| panic!("workload factory (op-log): {e:#}"));
        put_get(s.as_ref(), &uri("logged-b.dat"), 4).await;
    });
    finalize_operation_logger().unwrap();

    let rows = read_rows(&log);
    for name in ["logged-a.dat", "logged-b.dat"] {
        for op in ["PUT", "GET"] {
            let n = rows
                .iter()
                .filter(|(o, f)| o == op && f.ends_with(name))
                .count();
            assert_eq!(n, 1, "exactly one {op} row for {name}: {rows:?}");
        }
    }
    assert_eq!(rows.len(), 4, "only the four logged operations: {rows:?}");
}
