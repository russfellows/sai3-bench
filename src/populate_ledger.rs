//! Populate ledger — always-on tracking of object creation counts and sizes.
//!
//! Writes a header + one TSV row per prepare phase to `populate_ledger.tsv`
//! inside any results directory.  Enabled unconditionally, regardless of the
//! `enable_metadata_cache` setting.
//!
//! # Purpose
//! At large-scale namespace sizes listing objects from storage is infeasible as
//! a validation method (10,000 LIST ops/s → ~1,157 days for 1T objects, and even
//! 100B objects would take ~115 days).  The ledger gives a lightweight ground-truth
//! count: after N batches, sum the `objects_created` column across all
//! `populate_ledger.tsv` files.
//!
//! ```sh
//! # Total objects created across all batches:
//! awk -F'\t' 'NR>1 && $3!="AGGREGATE" {sum += $5} END {print sum}' \
//!     sai3-*/populate_ledger.tsv
//! ```
//!
//! # File location
//! * **Standalone** (`sai3-bench run`): `<results_dir>/populate_ledger.tsv`
//! * **Distributed agent**: `/tmp/sai3-agent-<ts>-<id>/populate_ledger.tsv`
//! * **Controller aggregate**: `<results_dir>/populate_ledger.tsv`
//!   (one row per agent + one `AGGREGATE` row summing all agents)

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;

pub const LEDGER_FILENAME: &str = "populate_ledger.tsv";

const HEADER: &str =
    "timestamp\ttest_name\tagent_id\tstage\tobjects_created\tobjects_existed\t\
total_objects\ttotal_bytes\tavg_bytes\twall_seconds\tops_per_sec";

/// A single row for the populate ledger.
pub struct LedgerRow {
    /// RFC3339 UTC timestamp of when this prepare phase completed.
    pub timestamp: String,
    /// Test / run name (from config or `"standalone"`).
    pub test_name: String,
    /// Agent identifier: `"standalone"`, `"agent-0"`, `"AGGREGATE"`, …
    pub agent_id: String,
    /// Stage name: `"prepare"` or a custom stage name from the YAML.
    pub stage: String,
    /// New objects successfully created.
    pub objects_created: u64,
    /// Objects that already existed and were skipped.
    pub objects_existed: u64,
    /// Total bytes written (PUT bytes).
    pub total_bytes: u64,
    /// Wall-clock seconds for the prepare phase.
    pub wall_seconds: f64,
}

impl LedgerRow {
    /// Total objects visible in namespace (created + existed).
    pub fn total_objects(&self) -> u64 {
        self.objects_created + self.objects_existed
    }

    /// Average bytes per newly-created object (0 when no objects created).
    pub fn avg_bytes(&self) -> f64 {
        if self.objects_created == 0 {
            0.0
        } else {
            self.total_bytes as f64 / self.objects_created as f64
        }
    }

    /// Throughput in objects/second (0 when wall_seconds ≤ 0).
    pub fn ops_per_sec(&self) -> f64 {
        if self.wall_seconds <= 0.0 {
            0.0
        } else {
            self.objects_created as f64 / self.wall_seconds
        }
    }

    fn to_tsv_row(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.3}\t{:.1}",
            self.timestamp,
            self.test_name,
            self.agent_id,
            self.stage,
            self.objects_created,
            self.objects_existed,
            self.total_objects(),
            self.total_bytes,
            self.avg_bytes(),
            self.wall_seconds,
            self.ops_per_sec(),
        )
    }
}

/// Append a ledger row to `populate_ledger.tsv` inside `dir`.
///
/// Creates the file with a TSV header if it does not yet exist, then appends
/// the row.  Uses `O_APPEND` so concurrent agents writing to different files
/// in the same directory are safe without additional locking.
///
/// Errors are non-fatal in production callers — the caller logs and continues.
pub fn append_row(dir: &Path, row: &LedgerRow) -> Result<()> {
    let path = dir.join(LEDGER_FILENAME);
    let is_new = !path.exists();

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open populate ledger: {}", path.display()))?;

    let mut writer = BufWriter::new(file);

    if is_new {
        writeln!(writer, "{}", HEADER).context("Failed to write ledger header")?;
    }

    writeln!(writer, "{}", row.to_tsv_row()).context("Failed to write ledger row")?;
    Ok(())
}
