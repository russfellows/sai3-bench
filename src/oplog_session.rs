//! Process-wide op-log session for long-lived processes (`sai3bench-agent`).
//!
//! s3dlio allows exactly ONE op-log session per process: `init_op_logger` while a
//! session is active is a silent no-op that keeps writing to the ORIGINAL file, and
//! after `finalize_op_logger` it fails for good. A long-lived agent that runs many
//! stages and jobs therefore cannot initialise and finalize per run: every run after
//! the first would be unlogged or fail.
//!
//! This module owns the session lifecycle for such a process:
//!
//! * [`begin`] starts the session on the first request and keeps it active. A later
//!   request for the SAME resolved path continues in the active session. A request
//!   for a DIFFERENT path (while active, or after [`end`]) is an error naming both
//!   paths, returned before the caller does any storage work.
//! * [`check`] is the read-only form of the same decision, for early rejection.
//! * [`end`] finalizes the session once, at process shutdown, so the zstd stream is
//!   complete.
//!
//! The standalone `sai3-bench run` command does not use this module: it has exactly
//! one session per process and keeps calling init/finalize directly.

use anyhow::{bail, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

enum State {
    /// No session was requested yet.
    Idle,
    /// A session is writing to this (normalised) path.
    Active { requested: PathBuf, key: PathBuf },
    /// The session was finalized; it cannot be restarted in this process.
    Finalized { requested: PathBuf },
}

static SESSION: Mutex<State> = Mutex::new(State::Idle);

fn lock() -> MutexGuard<'static, State> {
    SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lexically normalise `path` into an absolute comparison key (no filesystem access,
/// so a path that does not exist yet still compares correctly). `.` and `..`
/// components and repeated separators are folded.
fn key_of(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Decide whether a request for `path` is acceptable, without changing any state.
/// `Ok(true)` = continue in the active session, `Ok(false)` = a new session may start.
fn decide(state: &State, path: &Path) -> Result<bool> {
    match state {
        State::Idle => Ok(false),
        State::Active { requested, key } => {
            if *key == key_of(path) {
                Ok(true)
            } else {
                bail!(
                    "op-log path conflict: this process already has an op-log session writing to '{}' \
                     (s3dlio allows one op-log session per process), but this job requests '{}'. \
                     Use the same op_log_path for every job on this agent, or restart the agent",
                    requested.display(),
                    path.display()
                )
            }
        }
        State::Finalized { requested } => bail!(
            "op-log session for '{}' was already finalized and s3dlio cannot start a new one in \
             this process; cannot open the requested op-log '{}'. Restart the agent",
            requested.display(),
            path.display()
        ),
    }
}

/// Read-only check of a request for `path`: `Err` if [`begin`] would reject it.
pub fn check(path: &Path) -> Result<()> {
    decide(&lock(), path).map(|_| ())
}

/// Start the process's op-log session at `path`, or continue the active one when
/// `path` resolves to the same file. Rejects (without creating any file) a different
/// path while a session is active, and any request after [`end`].
pub fn begin(path: &Path) -> Result<()> {
    let mut state = lock();
    if decide(&state, path)? {
        return Ok(());
    }
    // A failed init leaves s3dlio (and this state) Idle, so a corrected request can retry.
    crate::workload::init_operation_logger(path)?;
    *state = State::Active {
        requested: path.to_path_buf(),
        key: key_of(path),
    };
    Ok(())
}

/// The path of the active session, if any.
pub fn active_path() -> Option<PathBuf> {
    match &*lock() {
        State::Active { requested, .. } => Some(requested.clone()),
        _ => None,
    }
}

/// Finalize the session (process shutdown). A no-op when no session is active. After
/// this, [`begin`] rejects every request.
pub fn end() -> Result<()> {
    let mut state = lock();
    let requested = match &*state {
        State::Active { requested, .. } => requested.clone(),
        _ => return Ok(()),
    };
    *state = State::Finalized { requested };
    crate::workload::finalize_operation_logger()
}
