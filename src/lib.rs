// src/lib.rs

use regex::escape;

pub mod config;
pub mod replay; // Legacy in-memory replay (v0.4.0)
pub mod replay_streaming; // New streaming replay (v0.5.0+)
pub mod workload;

/// Returns the bucket index for the size (0..8+) as per your CLI logic.
pub fn bucket_index(nbytes: usize) -> usize {
    if nbytes == 0 {
        0
    } else if nbytes <= 8 * 1024 {
        1
    } else if nbytes <= 64 * 1024 {
        2
    } else if nbytes <= 512 * 1024 {
        3
    } else if nbytes <= 4 * 1024 * 1024 {
        4
    } else if nbytes <= 32 * 1024 * 1024 {
        5
    } else if nbytes <= 256 * 1024 * 1024 {
        6
    } else if nbytes <= 2 * 1024 * 1024 * 1024 {
        7
    } else {
        8
    }
}

/// Converts a simple glob (with `*`) into a fully-anchored regex string.
pub fn glob_to_regex(glob: &str) -> String {
    format!("^{}$", escape(glob).replace(r"\*", ".*"))
}

