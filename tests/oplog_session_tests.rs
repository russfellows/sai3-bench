//! Process-wide op-log session (`sai3_bench::oplog_session`), used by the agent.
//!
//! One test function: the session (and s3dlio's op-logger under it) is a process
//! global that starts once and finalizes once.

use sai3_bench::oplog_session;
use tempfile::TempDir;

#[test]
fn one_session_per_process_same_path_continues_other_path_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a.tsv.zst");
    let b = tmp.path().join("b.tsv.zst");
    // Same file, spelled differently (`.` and a redundant separator).
    let a_alias = tmp
        .path()
        .join(".")
        .join("sub")
        .join("..")
        .join("a.tsv.zst");

    assert!(oplog_session::active_path().is_none());
    oplog_session::check(&a).expect("nothing active: any path is acceptable");

    oplog_session::begin(&a).expect("first request starts the session");
    assert_eq!(oplog_session::active_path().as_deref(), Some(a.as_path()));
    assert!(a.exists(), "session file created");

    // Same resolved path (any spelling) continues in the active session.
    oplog_session::begin(&a).expect("same path continues");
    oplog_session::begin(&a_alias).expect("same resolved path continues");
    oplog_session::check(&a).unwrap();

    // A different path is rejected, naming both, and creates nothing.
    let err = oplog_session::begin(&b).unwrap_err().to_string();
    assert!(
        err.contains(a.to_str().unwrap()),
        "names the active path: {err}"
    );
    assert!(
        err.contains(b.to_str().unwrap()),
        "names the requested path: {err}"
    );
    assert!(!b.exists(), "rejected request must not create its file");
    assert!(oplog_session::check(&b).is_err());
    assert_eq!(oplog_session::active_path().as_deref(), Some(a.as_path()));

    // Finalize once: the file is a complete zstd stream. A second end() is a no-op.
    oplog_session::end().expect("finalize");
    oplog_session::end().expect("finalize again is a no-op");
    let header = zstd::stream::decode_all(&std::fs::read(&a).unwrap()[..])
        .expect("finalized log is a complete zstd stream");
    assert!(String::from_utf8(header)
        .unwrap()
        .starts_with("idx\tthread\top"));
    assert!(oplog_session::active_path().is_none());

    // After finalize nothing can start: not the old path, not a new one.
    let e1 = oplog_session::begin(&a).unwrap_err().to_string();
    assert!(
        e1.contains("finalized") && e1.contains(a.to_str().unwrap()),
        "{e1}"
    );
    let e2 = oplog_session::begin(&b).unwrap_err().to_string();
    assert!(
        e2.contains("finalized") && e2.contains(b.to_str().unwrap()),
        "{e2}"
    );
    assert!(!b.exists());
}
