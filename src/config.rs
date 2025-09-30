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

    /// Base URI for the target backend (e.g., "s3://bucket/path", "file:///tmp/test", "direct:///mnt/data")
    /// If specified, all operations will be relative to this base URI.
    pub target: Option<String>,

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
    /// Can be absolute URI (s3://bucket/key) or relative path (data/file.txt) when target is set.
    Get { 
        path: String 
    },

    /// PUT objects of a fixed size.
    /// Uses 'path' relative to target, or absolute URI.
    Put {
        path: String,
        object_size: u64,
    },

    /// LIST objects under a path/prefix.
    /// Uses 'path' relative to target, or absolute URI.
    List {
        path: String,
    },

    /// STAT/HEAD a single object to get metadata.
    /// Uses 'path' relative to target, or absolute URI.
    Stat {
        path: String,
    },

    /// DELETE objects (single or glob pattern).
    /// Uses 'path' relative to target, or absolute URI.
    Delete {
        path: String,
    },
}

fn default_duration() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

fn default_concurrency() -> usize {
    16
}

impl Config {
    /// Resolve a path to a full URI using the target base URI
    pub fn resolve_uri(&self, path: &str) -> String {
        match &self.target {
            Some(base) => {
                if path.contains("://") {
                    // Path is already an absolute URI
                    path.to_string()
                } else {
                    // Combine base URI with relative path
                    if base.ends_with('/') {
                        format!("{}{}", base, path)
                    } else {
                        format!("{}/{}", base, path)
                    }
                }
            }
            None => {
                // No target specified, path must be absolute URI
                path.to_string()
            }
        }
    }

    /// Get the resolved URI for a GET operation
    pub fn get_uri(&self, get_op: &OpSpec) -> String {
        match get_op {
            OpSpec::Get { path } => self.resolve_uri(path),
            _ => panic!("Expected GET operation"),
        }
    }

    /// Get the resolved PUT target information
    pub fn get_put_info(&self, put_op: &OpSpec) -> (String, u64) {
        match put_op {
            OpSpec::Put { path, object_size } => {
                let base_uri = self.resolve_uri(path);
                (base_uri, *object_size)
            }
            _ => panic!("Expected PUT operation"),
        }
    }

    /// Get the resolved URI for a metadata operation (List, Stat, Delete)
    pub fn get_meta_uri(&self, meta_op: &OpSpec) -> String {
        match meta_op {
            OpSpec::List { path } | OpSpec::Stat { path } | OpSpec::Delete { path } => {
                self.resolve_uri(path)
            }
            _ => panic!("Expected metadata operation"),
        }
    }
}

