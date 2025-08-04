// src/config.rs
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Total wall time to run (e.g. "60s", "5m"). Defaults to 60s if omitted.
    #[serde(default = "default_duration", with = "humantime_serde")]
    pub duration: std::time::Duration,

    /// Number of concurrent workers.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,

    /// Weighted list of operations to pick from.
    pub workload: Vec<WeightedOp>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WeightedOp {
    pub weight: u32,
    #[serde(flatten)]
    pub spec: OpSpec,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum OpSpec {
    /// GET with a single key, a prefix (ending in '/'), or a glob with '*'.
    Get { uri: String },

    /// PUT objects of a fixed size under prefix.
    Put {
        bucket: String,
        prefix: String,
        object_size: u64,
    },
}

fn default_duration() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

fn default_concurrency() -> usize {
    16
}

