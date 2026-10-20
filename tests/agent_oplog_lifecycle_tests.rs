//! Agent op-log lifecycle: ONE op-log session per agent PROCESS.
//!
//! s3dlio 0.10.0 allows one op-log session per process (init after finalize fails;
//! init while active keeps the original file). The agent therefore must start the
//! session before the first storage work of the first job that asks for an op-log,
//! keep it active across stages and jobs, reject a job that asks for a DIFFERENT path
//! before doing any storage I/O, and finalize it once at shutdown.
//!
//! These tests run the real `sai3bench-agent` (one process, several jobs) and drive it
//! with the real `sai3bench-ctl`. Each controller run has a fixed ~10 s coordinated
//! start delay, so a test takes tens of seconds; they use separate agents and ports and
//! run in parallel.
//!
//! Every job here has at least two stages: a single-stage distributed job leaves the
//! agent's stats writer waiting for a "workload start" that never comes (the job hangs
//! at the final barrier hand-off); that is independent of the op-log and not exercised.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const HEADER: &str =
    "idx\tthread\top\tclient_id\tn_objects\tbytes\tendpoint\tfile\terror\tstart\tfirst_byte\tend\tduration_ns";
const PREPARED: usize = 6;
const AGENT_ID: &str = "agent-1";

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// A running `sai3bench-agent` process with its own working/tmp directory.
struct Agent {
    child: Option<Child>,
    port: u16,
    work: PathBuf,
    log: PathBuf,
}

impl Agent {
    fn start(work: &Path) -> Agent {
        let port = free_port();
        let tmp = work.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let log = work.join("agent.log");
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(assert_cmd::cargo::cargo_bin!("sai3bench-agent"))
            .args(["--listen", &format!("127.0.0.1:{port}"), "-v"])
            .current_dir(work)
            .env("TMPDIR", &tmp)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn sai3bench-agent");
        let agent = Agent {
            child: Some(child),
            port,
            work: work.to_path_buf(),
            log,
        };
        let t0 = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "agent did not listen"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        agent
    }

    /// Run one job through `sai3bench-ctl` (cwd = a fresh dir, results go there).
    fn run_job(&self, cfg: &Path, tag: &str) -> Output {
        let cwd = self.work.join(format!("ctl-{tag}"));
        std::fs::create_dir_all(&cwd).unwrap();
        let mut child = Command::new(assert_cmd::cargo::cargo_bin!("sai3bench-ctl"))
            .args(["run", "--config"])
            .arg(cfg)
            .args(["--start-delay", "0"])
            .current_dir(&cwd)
            .env("TMPDIR", self.work.join("tmp"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sai3bench-ctl");
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let h1 = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.read_to_end(&mut b);
            b
        });
        let h2 = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.read_to_end(&mut b);
            b
        });
        let t0 = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if t0.elapsed() > Duration::from_secs(180) {
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
                panic!("sai3bench-ctl timed out\nagent log:\n{}", self.agent_log());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        Output {
            status,
            stdout: h1.join().unwrap(),
            stderr: h2.join().unwrap(),
        }
    }

    /// Wait until the agent has returned to Idle after `jobs` finished jobs (it stays in
    /// `Completed`/`Failed` for a few seconds and rejects a START that arrives earlier).
    fn wait_idle_after(&self, jobs: usize) {
        let t0 = Instant::now();
        while self.agent_log().matches("\u{2192} Idle").count() < jobs {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "agent did not return to Idle after job {jobs}\n{}",
                self.agent_log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn agent_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Send `sig`, wait for a clean exit (the shutdown path must finalize the op-log).
    fn stop(&mut self, sig: i32) {
        let mut child = self.child.take().unwrap();
        unsafe { libc::kill(child.id() as i32, sig) };
        let t0 = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                // Not a SIGKILL: SIGTERM again, then fail the test.
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                let _ = child.wait();
                panic!(
                    "agent did not exit after signal {sig}\n{}",
                    self.agent_log()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(status.success(), "agent exit status {status}");
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            unsafe { libc::kill(c.id() as i32, libc::SIGTERM) };
            let _ = c.wait();
        }
    }
}

/// Stage YAML helpers.
const PREP: &str = "    - {name: prep, order: 1, type: prepare, completion: tasks_done}\n";
fn exec(order: u32) -> String {
    format!(
        "    - {{name: exec{order}, order: {order}, type: execute, duration: 1s, completion: duration}}\n"
    )
}
fn cleanup(order: u32) -> String {
    format!("    - {{name: cleanup, order: {order}, type: cleanup, completion: tasks_done}}\n")
}

fn write_cfg(path: &Path, agent: &Agent, data: &Path, oplog_base: &Path, stages: &str) -> PathBuf {
    let uri = format!("file://{}/", data.display());
    std::fs::write(
        path,
        format!(
            r#"target: "{uri}"
duration: 1s
concurrency: 2
op_log_path: "{oplog}"
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
distributed:
  agents:
    - address: "127.0.0.1:{port}"
      id: "{AGENT_ID}"
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: partitioned
  barrier_sync:
    enabled: true
  stages:
{stages}"#,
            oplog = oplog_base.display(),
            port = agent.port,
        ),
    )
    .unwrap();
    path.to_path_buf()
}

/// The file the staged agent writes for a configured `op_log_path`.
fn agent_oplog_file(base: &Path) -> PathBuf {
    let name = base.file_name().unwrap().to_str().unwrap();
    let stem = name.strip_suffix(".tsv.zst").unwrap();
    base.with_file_name(format!("{stem}_{AGENT_ID}.tsv.zst"))
}

struct Row {
    op: String,
    file: String,
}

/// Decode a COMPLETE zstd op-log (a truncated stream fails here) and return its rows.
fn read_oplog(path: &Path) -> Vec<Row> {
    let compressed =
        std::fs::read(path).unwrap_or_else(|e| panic!("op-log {} must exist: {e}", path.display()));
    let raw = zstd::stream::decode_all(&compressed[..]).unwrap_or_else(|e| {
        panic!(
            "op-log {} must be a complete zstd stream: {e}",
            path.display()
        )
    });
    let text = String::from_utf8(raw).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(HEADER));
    lines
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            assert_eq!(c.len(), 13, "13 columns: {l:?}");
            Row {
                op: c[2].to_string(),
                file: c[7].to_string(),
            }
        })
        .collect()
}

fn rows_of(rows: &[Row], op: &str, file_part: &str) -> usize {
    rows.iter()
        .filter(|r| r.op == op && r.file.contains(file_part))
        .count()
}

/// Sum of the GET ops the agent reported for stages whose name starts with `exec`.
/// Each is also asserted > 0 so a stage that did not run cannot hide behind a sum.
fn exec_get_ops(agent_log: &str) -> Vec<u64> {
    let mut v = Vec::new();
    for line in agent_log.lines() {
        if let Some(i) = line.find("Stage 'exec") {
            let rest = &line[i..];
            if let Some(j) = rest.find("summary: ") {
                let n: u64 = rest[j + 9..]
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                v.push(n);
            }
        }
    }
    v
}

fn assert_job_ok(out: &Output, agent: &Agent) {
    assert!(
        out.status.success(),
        "job failed: {}\nstdout:\n{}\nstderr:\n{}\nagent log:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        agent.agent_log()
    );
}

/// Several EXECUTE stages in ONE job: prepare, both executes and cleanup are all in
/// ONE complete log, finalized by SIGTERM at shutdown.
#[test]
fn multiple_execute_stages_in_one_job_are_in_one_complete_log() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path();
    let data = work.join("data");
    std::fs::create_dir(&data).unwrap();
    let base = work.join("ops.tsv.zst");

    let mut agent = Agent::start(work);
    let stages = format!("{PREP}{}{}{}", exec(2), exec(3), cleanup(4));
    let cfg = write_cfg(&work.join("job.yaml"), &agent, &data, &base, &stages);
    let out = agent.run_job(&cfg, "a");
    assert_job_ok(&out, &agent);
    let log_text = agent.agent_log();
    agent.stop(libc::SIGTERM);

    let gets = exec_get_ops(&log_text);
    assert_eq!(gets.len(), 2, "two execute stage summaries: {gets:?}");
    assert!(
        gets.iter().all(|n| *n > 0),
        "both stages ran GETs: {gets:?}"
    );

    let rows = read_oplog(&agent_oplog_file(&base));
    assert_eq!(
        rows_of(&rows, "GET", "/prepared-"),
        gets.iter().sum::<u64>() as usize,
        "GET rows must cover BOTH execute stages (stage GETs: {gets:?})"
    );
    assert_eq!(
        rows_of(&rows, "PUT", "/prepared-"),
        PREPARED,
        "prepare PUTs"
    );
    assert_eq!(
        rows_of(&rows, "DELETE", "/prepared-"),
        PREPARED,
        "cleanup DELETEs"
    );
}

/// Two sequential jobs on ONE agent process with the same op-log path continue in the
/// same session; SIGINT finalizes it.
#[test]
fn two_sequential_jobs_on_one_agent_share_one_complete_log() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path();
    let data = work.join("data");
    std::fs::create_dir(&data).unwrap();
    let base = work.join("ops.tsv.zst");

    let mut agent = Agent::start(work);
    let cfg1 = write_cfg(
        &work.join("job1.yaml"),
        &agent,
        &data,
        &base,
        &format!("{PREP}{}", exec(2)),
    );
    assert_job_ok(&agent.run_job(&cfg1, "1"), &agent);
    agent.wait_idle_after(1);
    let cfg2 = write_cfg(
        &work.join("job2.yaml"),
        &agent,
        &data,
        &base,
        &format!("{}{}", exec(1), exec(2)),
    );
    assert_job_ok(&agent.run_job(&cfg2, "2"), &agent);
    let log_text = agent.agent_log();
    agent.stop(libc::SIGINT);

    let gets = exec_get_ops(&log_text);
    assert_eq!(
        gets.len(),
        3,
        "job 1 one execute stage, job 2 two: {gets:?}"
    );
    assert!(
        gets.iter().all(|n| *n > 0),
        "every execute stage ran GETs: {gets:?}"
    );

    let rows = read_oplog(&agent_oplog_file(&base));
    assert_eq!(
        rows_of(&rows, "GET", "/prepared-"),
        gets.iter().sum::<u64>() as usize,
        "GET rows must cover BOTH jobs (job GETs: {gets:?})"
    );
    assert_eq!(
        rows_of(&rows, "PUT", "/prepared-"),
        PREPARED,
        "job 1 prepare"
    );
}

/// A job asking for a DIFFERENT op-log path is rejected before any storage I/O, with an
/// error naming both paths; the agent stays usable and the first log stays intact.
#[test]
fn job_with_a_different_oplog_path_is_rejected_before_any_io() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path();
    let data = work.join("data");
    std::fs::create_dir(&data).unwrap();
    let base = work.join("ops.tsv.zst");

    let mut agent = Agent::start(work);
    let cfg1 = write_cfg(
        &work.join("job1.yaml"),
        &agent,
        &data,
        &base,
        &format!("{PREP}{}", exec(2)),
    );
    assert_job_ok(&agent.run_job(&cfg1, "1"), &agent);
    agent.wait_idle_after(1);

    // Job 2: other op-log path AND an (empty) data dir that must stay untouched.
    let other_base = work.join("other.tsv.zst");
    let data2 = work.join("data2");
    std::fs::create_dir(&data2).unwrap();
    let cfg2 = write_cfg(
        &work.join("job2.yaml"),
        &agent,
        &data2,
        &other_base,
        &format!("{PREP}{}", exec(2)),
    );
    let out = agent.run_job(&cfg2, "2");
    let text = format!(
        "{}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        agent.agent_log()
    );
    assert!(
        !out.status.success(),
        "job with a conflicting op-log path must fail:\n{text}"
    );
    let (active_file, other_file) = (agent_oplog_file(&base), agent_oplog_file(&other_base));
    assert!(
        text.contains(active_file.to_str().unwrap()) && text.contains(other_file.to_str().unwrap()),
        "error must name both resolved paths ({} and {}):\n{text}",
        active_file.display(),
        other_file.display()
    );
    assert_eq!(
        std::fs::read_dir(&data2).unwrap().count(),
        0,
        "rejected job must not write anything into its data dir"
    );
    assert!(
        !other_file.exists() && !other_base.exists(),
        "rejected job must not create its op-log"
    );

    // The agent is not wedged: a third job with the original path still logs.
    // (The pre-flight rejection leaves the agent Idle: no further state change to wait for.)
    agent.wait_idle_after(1);
    let cfg3 = write_cfg(
        &work.join("job3.yaml"),
        &agent,
        &data,
        &base,
        &format!("{}{}", exec(1), exec(2)),
    );
    assert_job_ok(&agent.run_job(&cfg3, "3"), &agent);
    let log_text = agent.agent_log();
    agent.stop(libc::SIGTERM);

    let gets = exec_get_ops(&log_text);
    assert_eq!(
        gets.len(),
        3,
        "job 1 (1 execute) and job 3 (2 executes): {gets:?}"
    );
    let rows = read_oplog(&agent_oplog_file(&base));
    assert_eq!(
        rows_of(&rows, "GET", "/prepared-"),
        gets.iter().sum::<u64>() as usize,
        "log holds job 1 and job 3 (job GETs: {gets:?})"
    );
    assert_eq!(rows_of(&rows, "PUT", "/data2/"), 0, "nothing from job 2");
}

/// The unary `RunWorkload` RPC and a staged `START` share the same process-wide session:
/// a staged START (sent straight over gRPC, i.e. without the controller's pre-flight
/// RPC) asking for a different path is rejected by the agent itself, and a later unary
/// run at the original path continues in the same session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unary_run_workload_and_staged_start_share_one_session() {
    use futures::StreamExt;
    use sai3_bench::pb::iobench::agent_client::AgentClient;
    use sai3_bench::pb::iobench::{ControlMessage, RunWorkloadRequest};

    let tmp = TempDir::new().unwrap();
    let work = tmp.path();
    let data = work.join("data");
    let data2 = work.join("data2");
    std::fs::create_dir(&data).unwrap();
    std::fs::create_dir(&data2).unwrap();
    let base = work.join("ops.tsv.zst");
    let uri = format!("file://{}/", data.display());

    let mut agent = Agent::start(work);
    let mut client = AgentClient::connect(format!("http://127.0.0.1:{}", agent.port))
        .await
        .unwrap();

    let unary_cfg = format!(
        r#"target: "{uri}"
duration: 1s
concurrency: 2
op_log_path: "{op}"
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
"#,
        op = base.display()
    );
    let unary = |cfg: String| RunWorkloadRequest {
        config_yaml: cfg,
        agent_id: AGENT_ID.to_string(),
        path_prefix: String::new(),
        start_timestamp_ns: 0,
        shared_storage: true,
        agent_index: 0,
        num_agents: 1,
    };

    // Job 1: unary run (prepare + workload), op-log session starts before prepare.
    let s1 = client
        .run_workload(unary(unary_cfg.clone()))
        .await
        .unwrap_or_else(|e| panic!("unary job 1: {e}\n{}", agent.agent_log()))
        .into_inner();
    let gets1 = s1.get.as_ref().map(|g| g.ops).unwrap_or(0);
    assert!(gets1 > 0, "job 1 ran GETs");

    // Job 2: staged START over gRPC with another op-log path -> ERROR, no storage I/O.
    let other_base = work.join("other.tsv.zst");
    let staged_cfg = write_cfg(
        &work.join("staged.yaml"),
        &agent,
        &data2,
        &other_base,
        &format!("{PREP}{}", exec(2)),
    );
    let start = ControlMessage {
        command: 1, // START
        config_yaml: std::fs::read_to_string(staged_cfg).unwrap(),
        agent_id: AGENT_ID.to_string(),
        path_prefix: "agent-1/".to_string(),
        shared_storage: true,
        num_agents: 1,
        ..Default::default()
    };
    let requests = futures::stream::iter(vec![start]).chain(futures::stream::pending());
    let mut stream = client
        .execute_workload(requests)
        .await
        .unwrap()
        .into_inner();
    let mut error_message = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
            Ok(Some(Ok(stats))) if stats.status == 3 => {
                error_message = Some(stats.error_message);
                break;
            }
            Ok(Some(Ok(_))) => continue,
            _ => break,
        }
    }
    let msg = error_message
        .unwrap_or_else(|| panic!("staged START must be rejected\n{}", agent.agent_log()));
    let unary_file = work.join("ops-agent-1.tsv.zst");
    let other_file = agent_oplog_file(&other_base);
    assert!(
        msg.contains(unary_file.to_str().unwrap()) && msg.contains(other_file.to_str().unwrap()),
        "error must name both paths ({} and {}): {msg}",
        unary_file.display(),
        other_file.display()
    );
    drop(stream);
    assert_eq!(
        std::fs::read_dir(&data2).unwrap().count(),
        0,
        "rejected START must not write into its data dir"
    );
    assert!(
        !other_file.exists(),
        "rejected START must not create its op-log"
    );

    // Job 3: unary run at the original path continues in the SAME session.
    let s3 = client
        .run_workload(unary(unary_cfg))
        .await
        .unwrap_or_else(|e| panic!("unary job 3: {e}\n{}", agent.agent_log()))
        .into_inner();
    let gets3 = s3.get.as_ref().map(|g| g.ops).unwrap_or(0);
    assert!(gets3 > 0, "job 3 ran GETs");
    agent.stop(libc::SIGTERM);

    let rows = read_oplog(&unary_file);
    assert_eq!(
        rows_of(&rows, "GET", "/prepared-"),
        (gets1 + gets3) as usize,
        "GET rows must cover job 1 and job 3 (GETs: {gets1}, {gets3})"
    );
    assert_eq!(
        rows_of(&rows, "PUT", "/prepared-"),
        2 * PREPARED,
        "prepare PUTs of job 1 and job 3 (the unary prepare rewrites the objects)"
    );
}
