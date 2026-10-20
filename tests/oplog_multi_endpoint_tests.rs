//! Multi-endpoint stores must NOT be wrapped in an op-log `LoggedObjectStore`.
//!
//! s3dlio builds the bucket-form `s3://` endpoints of a `MultiEndpointStore` with
//! `S3ObjectStore::with_helper_rows()`, which writes its own GET/PUT op-log rows and
//! assumes the store is never wrapped (s3dlio #175). Wrapping it in sai3-bench would
//! log every GET/PUT twice. file:// endpoints write no rows of their own, so with an
//! active op-log a multi-endpoint file:// store must produce zero rows. That detects a
//! wrapper without needing S3.
//!
//! s3dlio's op-logger is a process-global singleton that initializes once per
//! process, so this lives in its own test binary with a single test.

use bytes::Bytes;
use sai3_bench::config::Config;
use sai3_bench::workload::{
    create_store_for_uri, create_store_from_config, finalize_operation_logger,
    init_operation_logger,
};
use serial_test::serial;
use tempfile::TempDir;

#[test]
#[serial]
fn multi_endpoint_store_is_not_wrapped_in_the_op_logger() {
    let tmp = TempDir::new().unwrap();
    let ep1 = tmp.path().join("ep1");
    let ep2 = tmp.path().join("ep2");
    std::fs::create_dir(&ep1).unwrap();
    std::fs::create_dir(&ep2).unwrap();
    let log = tmp.path().join("multi.tsv.zst");

    let yaml = format!(
        r#"
duration: 30s
concurrency: 2
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file://{}/"
    - "file://{}/"
workload:
  - op: get
    path: "file://{}/obj"
    weight: 1
"#,
        ep1.display(),
        ep2.display(),
        ep1.display()
    );
    let config: Config = serde_yaml::from_str(&yaml).expect("YAML must parse");

    init_operation_logger(&log).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Multi-endpoint store from the factory: must write no sai3-bench rows.
        let multi = create_store_from_config(&config, None).unwrap();
        // Two PUTs: round-robin sends one to each endpoint.
        for name in ["multi1.dat", "multi2.dat"] {
            multi
                .put(
                    &format!("file://{}/{name}", ep1.display()),
                    Bytes::from_static(b"multi"),
                )
                .await
                .unwrap();
        }

        // Control: a single-URI store from the factory IS logged, so zero rows above
        // is not just a broken logger.
        let single = create_store_for_uri(&format!("file://{}/", ep1.display())).unwrap();
        single
            .put(
                &format!("file://{}/single.dat", ep1.display()),
                Bytes::from_static(b"single"),
            )
            .await
            .unwrap();
    });
    finalize_operation_logger().unwrap();

    let compressed = std::fs::read(&log).unwrap();
    let text = String::from_utf8(zstd::stream::decode_all(&compressed[..]).unwrap()).unwrap();
    let rows: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(
        rows.iter().filter(|r| r.contains("/multi")).count(),
        0,
        "a multi-endpoint store must not be wrapped in the op-logger (double rows): {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|r| r.contains("/single.dat")).count(),
        1,
        "control: the single-URI store must be logged exactly once: {rows:?}"
    );
    assert_eq!(rows.len(), 1, "only the control row expected: {rows:?}");
}
