// tests/grpc_integration.rs
//
// Integration test for the gRPC Agent and Controller binaries (PLAINTEXT mode).
// We only exercise `ping` here to avoid requiring AWS credentials or S3 access.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command as StdCommand};
use std::thread;
use std::time::{Duration, Instant};

/// Bind to 127.0.0.1:0 to discover a free ephemeral port, then drop the listener.
fn pick_free_port() -> io::Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let port = l.local_addr()?.port();
    drop(l);
    Ok(port)
}

/// Wait until the TCP port is connectable (agent is accepting) or timeout.
fn wait_for_port(addr: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect(addr).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Spawns the agent with `--listen` (PLAINTEXT), waits for readiness,
/// then runs the controller `ping` with `--insecure`.
#[test]
fn agent_and_controller_ping() {
    // 1) Allocate a free port and compose the listen address
    let port = pick_free_port().expect("failed to pick a free port");
    let addr = format!("127.0.0.1:{port}");

    // 2) Start the agent on that address (PLAINTEXT)
    let agent: Child = StdCommand::new(assert_cmd::cargo::cargo_bin!("sai3bench-agent"))
        .arg("--listen")
        .arg(&addr)
        .spawn()
        .expect("failed to spawn sai3bench-agent");

    // Ensure we kill the agent at the end of the test, even on panic
    struct Guard(Option<Child>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let _guard = Guard(Some(agent));

    // 3) Wait for the agent to be listening (up to 3 seconds)
    assert!(
        wait_for_port(&addr, Duration::from_secs(3)),
        "agent didn't start listening at {addr} in time"
    );

    // 4) Run the controller `ping` against the agent (plaintext/insecure is now default)
    let output = StdCommand::new(assert_cmd::cargo::cargo_bin!("sai3bench-ctl"))
        .arg("--agents")
        .arg(&addr)
        .arg("ping")
        .output()
        .expect("failed to run sai3bench-ctl");

    // 5) Assert success and that either stdout or stderr contains the "connected" line
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "controller ping failed\nstatus={:?}\nstdout=\n{}\nstderr=\n{}",
        output.status,
        out,
        err
    );
    assert!(
        out.contains(&format!("connected to {}", addr)) || err.contains(&format!("connected to {}", addr)),
        "expected controller to print 'connected to {}'\nstdout=\n{}\nstderr=\n{}",
        addr,
        out,
        err
    );
}

