//! URI remapping engine for replay operations
//!
//! Supports flexible source→target mappings for warp-replay parity:
//! - **1→1**: Simple substitution (existing functionality)
//! - **1→N**: Fanout with strategies (round-robin, random, sticky-key)
//! - **N→1**: Consolidation to single target
//! - **N↔N**: General regex-based remapping with capture groups
//!
//! # Architecture
//!
//! The remap engine applies an ordered list of rules. First match wins.
//! Each rule can use different matching strategies:
//!
//! - **Simple**: Match host/bucket/prefix, map to single target
//! - **Fanout**: Match pattern, map to multiple targets with selection strategy
//! - **Consolidate**: Match any of multiple patterns, map to single target
//! - **Regex**: Full regex with capture groups for complex transformations
//!
//! # Example YAML
//!
//! ```yaml
//! rules:
//!   # 1→1: Simple bucket rename
//!   - match:
//!       bucket: "old-bucket"
//!     map_to:
//!       bucket: "new-bucket"
//!       prefix: "archived/"
//!   
//!   # 1→N: Fanout to multiple targets
//!   - match:
//!       bucket: "source"
//!       prefix: "data/"
//!     map_to_many:
//!       targets:
//!         - {bucket: "replica1", prefix: "data/"}
//!         - {bucket: "replica2", prefix: "data/"}
//!       strategy: "round_robin"
//!   
//!   # Regex: Advanced pattern matching
//!   - regex: "^s3://prod-([^/]+)/(.*)$"
//!     replace: "s3://staging-$1/$2"
//! ```

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use url::Url;

/// Top-level remapping configuration
#[derive(Debug, Deserialize, Clone)]
pub struct RemapConfig {
    /// Ordered list of rules; first match wins
    pub rules: Vec<RemapRule>,
}

/// A single remapping rule
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RemapRule {
    /// Simple 1→1 mapping
    Simple {
        #[serde(rename = "match")]
        match_spec: MatchSpec,
        map_to: TargetSpec,
    },
    
    /// 1→N fanout mapping
    Fanout {
        #[serde(rename = "match")]
        match_spec: MatchSpec,
        map_to_many: ManyTargets,
    },
    
    /// N→1 consolidation
    Consolidate {
        match_any: Vec<MatchSpec>,
        map_to: TargetSpec,
    },
    
    /// Regex-based mapping (most flexible)
    Regex {
        regex: String,
        replace: String,
    },
}

/// Pattern to match against URIs
#[derive(Debug, Deserialize, Clone)]
pub struct MatchSpec {
    /// Match host (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    
    /// Match bucket/container name (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    
    /// Match prefix (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Target specification for remapping
#[derive(Debug, Deserialize, Clone)]
pub struct TargetSpec {
    /// Target bucket/container name
    pub bucket: String,
    
    /// Target prefix (optional, defaults to empty)
    #[serde(default)]
    pub prefix: String,
}

/// Multiple targets for fanout
#[derive(Debug, Deserialize, Clone)]
pub struct ManyTargets {
    /// List of target specifications
    pub targets: Vec<TargetSpec>,
    
    /// Selection strategy: "round_robin", "random", "sticky_key"
    #[serde(default = "default_strategy")]
    pub strategy: String,
}

fn default_strategy() -> String {
    "round_robin".to_string()
}

/// Parsed URI components for matching/remapping
#[derive(Debug, Clone)]
pub struct ParsedUri {
    pub scheme: String,
    pub bucket: String,
    pub prefix: String,
    pub key: String,
}

impl ParsedUri {
    /// Parse URI into components
    /// 
    /// Examples:
    /// - `s3://bucket/prefix/key.dat` → bucket="bucket", prefix="prefix/", key="key.dat"
    /// - `gs://bucket/path/to/file` → bucket="bucket", prefix="path/to/", key="file"
    /// - `file:///tmp/data/file.txt` → bucket="tmp", prefix="data/", key="file.txt"
    pub fn parse(uri: &str) -> Result<Self> {
        let url = Url::parse(uri)
            .with_context(|| format!("Invalid URI: {}", uri))?;
        
        let scheme = url.scheme().to_string();
        
        // For S3-style URIs (s3://, gs://, az://), bucket is in host
        // For file://, path starts with /
        let path = if scheme == "file" {
            url.path().trim_start_matches('/').to_string()
        } else {
            // S3-style: s3://bucket/prefix/key
            // The bucket might be in the host OR first path segment
            if let Some(host) = url.host_str() {
                if !host.is_empty() {
                    // Bucket is in host, path is the rest
                    let path_part = url.path().trim_start_matches('/');
                    if path_part.is_empty() {
                        return Ok(Self {
                            scheme,
                            bucket: host.to_string(),
                            prefix: String::new(),
                            key: String::new(),
                        });
                    }
                    // Split path_part into prefix and key
                    let (prefix, key) = if let Some(idx) = path_part.rfind('/') {
                        (format!("{}/", &path_part[..idx]), path_part[idx+1..].to_string())
                    } else {
                        ("".to_string(), path_part.to_string())
                    };
                    return Ok(Self {
                        scheme,
                        bucket: host.to_string(),
                        prefix,
                        key,
                    });
                }
            }
            // Fallback: bucket is first path segment
            url.path().trim_start_matches('/').to_string()
        };
        
        // Split path into bucket and rest
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        let bucket = parts.first().unwrap_or(&"").to_string();
        let rest = parts.get(1).unwrap_or(&"");
        
        // Split rest into prefix and key
        let (prefix, key) = if let Some(idx) = rest.rfind('/') {
            (format!("{}/", &rest[..idx]), rest[idx+1..].to_string())
        } else if !rest.is_empty() {
            // No slash in rest, it's just a key
            ("".to_string(), rest.to_string())
        } else {
            ("".to_string(), "".to_string())
        };
        
        Ok(Self {
            scheme,
            bucket,
            prefix,
            key,
        })
    }
    
    /// Reconstruct URI from components
    pub fn to_uri(&self) -> String {
        if self.prefix.is_empty() && self.key.is_empty() {
            format!("{}://{}/", self.scheme, self.bucket)
        } else if self.prefix.is_empty() {
            format!("{}://{}/{}", self.scheme, self.bucket, self.key)
        } else {
            format!("{}://{}/{}{}", self.scheme, self.bucket, self.prefix, self.key)
        }
    }
}

/// Remapping engine with stateful strategies
pub struct RemapEngine {
    config: RemapConfig,
    
    /// State for round-robin (rule_index → counter)
    round_robin_state: Arc<Mutex<HashMap<usize, usize>>>,
    
    /// State for sticky-key (key → target_index per rule)
    sticky_state: Arc<Mutex<HashMap<String, HashMap<String, usize>>>>,
}

impl RemapEngine {
    /// Create a new remapping engine from configuration
    pub fn new(config: RemapConfig) -> Self {
        Self {
            config,
            round_robin_state: Arc::new(Mutex::new(HashMap::new())),
            sticky_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// Apply remapping rules to a URI
    /// 
    /// Returns the remapped URI if any rule matches, or the original URI if no match.
    pub fn remap(&self, uri: &str) -> Result<String> {
        let parsed = ParsedUri::parse(uri)?;
        
        // Try each rule in order
        for (rule_idx, rule) in self.config.rules.iter().enumerate() {
            match rule {
                RemapRule::Simple { match_spec, map_to } => {
                    if self.matches(&parsed, match_spec) {
                        return Ok(self.apply_target(&parsed, map_to));
                    }
                }
                
                RemapRule::Fanout { match_spec, map_to_many } => {
                    if self.matches(&parsed, match_spec) {
                        let target = self.select_target_from_many(
                            rule_idx,
                            &parsed.key,
                            map_to_many
                        )?;
                        return Ok(self.apply_target(&parsed, target));
                    }
                }
                
                RemapRule::Consolidate { match_any, map_to } => {
                    if match_any.iter().any(|spec| self.matches(&parsed, spec)) {
                        return Ok(self.apply_target(&parsed, map_to));
                    }
                }
                
                RemapRule::Regex { regex, replace } => {
                    use regex::Regex;
                    let re = Regex::new(regex)
                        .with_context(|| format!("Invalid regex: {}", regex))?;
                    
                    if re.is_match(uri) {
                        return Ok(re.replace(uri, replace.as_str()).to_string());
                    }
                }
            }
        }
        
        // No match: return original
        Ok(uri.to_string())
    }
    
    /// Check if a parsed URI matches a specification
    fn matches(&self, parsed: &ParsedUri, spec: &MatchSpec) -> bool {
        if let Some(ref bucket) = spec.bucket {
            if parsed.bucket != *bucket {
                return false;
            }
        }
        
        if let Some(ref prefix) = spec.prefix {
            if !parsed.prefix.starts_with(prefix) {
                return false;
            }
        }
        
        true
    }
    
    /// Apply a target specification to a parsed URI
    /// Logic:
    /// - If target.prefix is empty: preserve original path (prefix + key)
    /// - If target.prefix is non-empty: replace original prefix with target prefix, keep key
    fn apply_target(&self, parsed: &ParsedUri, target: &TargetSpec) -> String {
        let (new_prefix, new_key) = if target.prefix.is_empty() {
            // Empty target prefix: preserve original full path
            if parsed.prefix.is_empty() {
                ("".to_string(), parsed.key.clone())
            } else {
                (parsed.prefix.clone(), parsed.key.clone())
            }
        } else {
            // Non-empty target prefix: replace original prefix with target prefix
            (target.prefix.clone(), parsed.key.clone())
        };
        
        ParsedUri {
            scheme: parsed.scheme.clone(),
            bucket: target.bucket.clone(),
            prefix: new_prefix,
            key: new_key,
        }.to_uri()
    }
    
    /// Select a target from multiple options using a strategy
    fn select_target_from_many<'a>(
        &self,
        rule_idx: usize,
        key: &str,
        many: &'a ManyTargets
    ) -> Result<&'a TargetSpec> {
        if many.targets.is_empty() {
            bail!("No targets defined for fanout rule");
        }
        
        match many.strategy.as_str() {
            "round_robin" => {
                let mut state = self.round_robin_state.lock().unwrap();
                let counter = state.entry(rule_idx).or_insert(0);
                let idx = *counter % many.targets.len();
                *counter = (*counter + 1) % many.targets.len();
                Ok(&many.targets[idx])
            }
            
            "random" => {
                use rand::Rng;
                let idx = rand::rng().random_range(0..many.targets.len());
                Ok(&many.targets[idx])
            }
            
            "sticky_key" => {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                
                let mut state = self.sticky_state.lock().unwrap();
                let rule_state = state.entry(rule_idx.to_string()).or_default();
                
                let idx = *rule_state.entry(key.to_string()).or_insert_with(|| {
                    // Hash key to get stable target index
                    let mut hasher = DefaultHasher::new();
                    key.hash(&mut hasher);
                    (hasher.finish() as usize) % many.targets.len()
                });
                
                Ok(&many.targets[idx])
            }
            
            _ => bail!("Unknown strategy: {}", many.strategy),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_uri_s3() {
        let uri = "s3://bucket/path/to/file.dat";
        let parsed = ParsedUri::parse(uri).unwrap();
        assert_eq!(parsed.scheme, "s3");
        assert_eq!(parsed.bucket, "bucket");
        assert_eq!(parsed.prefix, "path/to/");
        assert_eq!(parsed.key, "file.dat");
        assert_eq!(parsed.to_uri(), uri);
    }
    
    #[test]
    fn test_parse_uri_file() {
        let uri = "file:///tmp/data/test.txt";
        let parsed = ParsedUri::parse(uri).unwrap();
        assert_eq!(parsed.scheme, "file");
        assert_eq!(parsed.bucket, "tmp");
        assert_eq!(parsed.prefix, "data/");
        assert_eq!(parsed.key, "test.txt");
    }
    
    #[test]
    fn test_simple_remap() {
        let config = RemapConfig {
            rules: vec![RemapRule::Simple {
                match_spec: MatchSpec {
                    host: None,
                    bucket: Some("src".to_string()),
                    prefix: None,
                },
                map_to: TargetSpec {
                    bucket: "dest".to_string(),
                    prefix: "new/".to_string(),
                },
            }],
        };
        
        let engine = RemapEngine::new(config);
        let result = engine.remap("s3://src/old/file.dat").unwrap();
        assert_eq!(result, "s3://dest/new/file.dat");
    }
    
    #[test]
    fn test_fanout_round_robin() {
        let config = RemapConfig {
            rules: vec![RemapRule::Fanout {
                match_spec: MatchSpec {
                    host: None,
                    bucket: Some("src".to_string()),
                    prefix: None,
                },
                map_to_many: ManyTargets {
                    targets: vec![
                        TargetSpec {
                            bucket: "dest1".to_string(),
                            prefix: "".to_string(),
                        },
                        TargetSpec {
                            bucket: "dest2".to_string(),
                            prefix: "".to_string(),
                        },
                    ],
                    strategy: "round_robin".to_string(),
                },
            }],
        };
        
        let engine = RemapEngine::new(config);
        
        let r1 = engine.remap("s3://src/data/file1.dat").unwrap();
        let r2 = engine.remap("s3://src/data/file2.dat").unwrap();
        let r3 = engine.remap("s3://src/data/file3.dat").unwrap();
        
        // Should alternate between targets
        assert_eq!(r1, "s3://dest1/data/file1.dat");
        assert_eq!(r2, "s3://dest2/data/file2.dat");
        assert_eq!(r3, "s3://dest1/data/file3.dat");
    }
    
    #[test]
    fn test_regex_remap() {
        let config = RemapConfig {
            rules: vec![RemapRule::Regex {
                regex: "^s3://prod-([^/]+)/(.*)$".to_string(),
                replace: "s3://staging-$1/$2".to_string(),
            }],
        };
        
        let engine = RemapEngine::new(config);
        let result = engine.remap("s3://prod-db/data/file.dat").unwrap();
        assert_eq!(result, "s3://staging-db/data/file.dat");
    }
    
    #[test]
    fn test_no_match_returns_original() {
        let config = RemapConfig {
            rules: vec![RemapRule::Simple {
                match_spec: MatchSpec {
                    host: None,
                    bucket: Some("different".to_string()),
                    prefix: None,
                },
                map_to: TargetSpec {
                    bucket: "target".to_string(),
                    prefix: "".to_string(),
                },
            }],
        };
        
        let engine = RemapEngine::new(config);
        let original = "s3://unchanged/data/file.dat";
        let result = engine.remap(original).unwrap();
        assert_eq!(result, original);
    }
}
