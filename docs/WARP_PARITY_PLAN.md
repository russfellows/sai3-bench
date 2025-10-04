# Warp/Warp-Replay Parity Implementation Plan

## Executive Summary

Align io-bench with warp/warp-replay capabilities to enable near-identical testing between the tools. The analysis is **ACCURATE** and comprehensive. This plan breaks down implementation into phases with specific file changes, function signatures, and acceptance criteria.

## Current State Assessment

### ✅ Already Implemented (No Changes Needed)
- **Time-bounded execution**: Duration-based runs with deadline scheduling
- **Weighted mixed workloads**: `WeightedIndex` chooser with configurable weights
- **GET/PUT/LIST/STAT/DELETE operations**: All 5 operations supported
- **Multi-backend support**: File, DirectIO, S3, Azure, GCS (5 backends)
- **Pre-resolution for GET**: Pre-lists and caches URIs before timed run
- **YAML configuration**: Duration, concurrency, weighted operations
- **Basic replay (v0.4.0/v0.4.1)**: Op-log capture, microsecond scheduling, 1→1 retargeting
- **Streaming replay (v0.4.1)**: Constant memory (~1.5 MB) via s3dlio-oplog
- **Concurrency control**: Semaphore-based worker pool

### ❌ Gaps to Close

1. **Prepare/Pre-population Step**: Ensure objects exist before testing (not just pre-list)
2. **Cleanup After Tests**: Optional deletion of prepared objects
3. **Advanced Replay Remapping**: 1→N, N→1, N↔N mappings beyond simple 1→1
4. **Remap Rule Engine**: YAML-driven flexible remapping with strategies
5. **UX Polish**: Documentation, defaults, CLI flags

---

## Implementation Phases

### Phase 1: Prepare/Pre-population (Priority: HIGH)
**Goal**: Match Warp's "prepare then measure" behavior

#### 1.1 Config Schema Extension

**File**: `src/config.rs`

Add to `Config` struct:
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    // ... existing fields ...
    
    /// Optional prepare step to ensure objects exist before testing
    #[serde(default)]
    pub prepare: Option<PrepareConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PrepareConfig {
    /// Objects to ensure exist before test
    #[serde(default)]
    pub ensure_objects: Vec<EnsureSpec>,
    
    /// Whether to cleanup prepared objects after test
    #[serde(default)]
    pub cleanup: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EnsureSpec {
    /// Base URI for object creation (e.g., "s3://bucket/prefix/")
    pub base_uri: String,
    
    /// Target number of objects to ensure exist
    pub count: u64,
    
    /// Minimum object size in bytes
    #[serde(default = "default_min_size")]
    pub min_size: u64,
    
    /// Maximum object size in bytes  
    #[serde(default = "default_max_size")]
    pub max_size: u64,
    
    /// Fill pattern: "zero", "random", or future: "pattern"
    #[serde(default = "default_fill")]
    pub fill: FillPattern,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FillPattern {
    Zero,
    Random,
    // Future: Pattern { dedup_ratio: f64 }
}

fn default_min_size() -> u64 { 1024 * 1024 } // 1 MiB
fn default_max_size() -> u64 { 1024 * 1024 } // 1 MiB
fn default_fill() -> FillPattern { FillPattern::Zero }
```

**Validation**: 
- Deserialize YAML with `prepare` section
- Verify defaults work when `prepare` omitted

#### 1.2 Prepare Implementation

**File**: `src/workload.rs`

Add new function before `run_workload`:
```rust
/// Execute prepare step: ensure objects exist for testing
pub async fn prepare_objects(config: &PrepareConfig) -> anyhow::Result<Vec<PreparedObject>> {
    let mut created_objects = Vec::new();
    
    for spec in &config.ensure_objects {
        info!("Preparing objects: {} at {}", spec.count, spec.base_uri);
        
        let store = create_store_for_uri(&spec.base_uri)?;
        
        // 1. List existing objects
        let existing = store.list(&spec.base_uri, true).await
            .context("Failed to list existing objects")?;
        
        let existing_count = existing.len() as u64;
        info!("  Found {} existing objects", existing_count);
        
        // 2. Calculate how many to create
        let to_create = if existing_count >= spec.count {
            info!("  Sufficient objects already exist");
            0
        } else {
            spec.count - existing_count
        };
        
        // 3. Create missing objects
        if to_create > 0 {
            info!("  Creating {} additional objects", to_create);
            
            for i in 0..to_create {
                let key = format!("prepared-{:08}.dat", existing_count + i);
                let uri = format!("{}{}", spec.base_uri, key);
                
                // Generate data based on fill pattern
                let size = if spec.min_size == spec.max_size {
                    spec.min_size
                } else {
                    use rand::Rng;
                    rand::thread_rng().gen_range(spec.min_size..=spec.max_size)
                };
                
                let data = match spec.fill {
                    FillPattern::Zero => vec![0u8; size as usize],
                    FillPattern::Random => {
                        use rand::RngCore;
                        let mut data = vec![0u8; size as usize];
                        rand::thread_rng().fill_bytes(&mut data);
                        data
                    }
                };
                
                // PUT object
                store.put(&uri, &data).await
                    .with_context(|| format!("Failed to PUT {}", uri))?;
                
                created_objects.push(PreparedObject {
                    uri,
                    size,
                    created: true,
                });
                
                if (i + 1) % 1000 == 0 {
                    info!("  Progress: {}/{} objects created", i + 1, to_create);
                }
            }
        }
    }
    
    info!("Prepare complete: {} objects ready", created_objects.len());
    Ok(created_objects)
}

/// Cleanup prepared objects
pub async fn cleanup_prepared_objects(objects: &[PreparedObject]) -> anyhow::Result<()> {
    info!("Cleaning up {} prepared objects", objects.len());
    
    for (i, obj) in objects.iter().enumerate() {
        if obj.created {
            let store = create_store_for_uri(&obj.uri)?;
            if let Err(e) = store.delete(&obj.uri).await {
                warn!("Failed to delete {}: {}", obj.uri, e);
            }
        }
        
        if (i + 1) % 1000 == 0 {
            info!("  Progress: {}/{} objects deleted", i + 1, objects.len());
        }
    }
    
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PreparedObject {
    pub uri: String,
    pub size: u64,
    pub created: bool,
}
```

**Integration Point**: In `src/main.rs`, `Commands::Run` handler:
```rust
// Before run_workload, add:
let prepared_objects = if let Some(prepare) = &config.prepare {
    prepare_objects(prepare).await?
} else {
    Vec::new()
};

// ... existing run_workload call ...

// After run_workload, add:
if let Some(prepare) = &config.prepare {
    if prepare.cleanup && !prepared_objects.is_empty() {
        cleanup_prepared_objects(&prepared_objects).await?;
    }
}
```

**Acceptance Criteria**:
- [ ] Can run config with `prepare` section
- [ ] Creates objects if count insufficient
- [ ] Skips creation if objects already exist
- [ ] Cleanup deletes only created objects
- [ ] Progress logging every 1000 objects
- [ ] Works across all 5 backends

#### 1.3 CLI Flags

**File**: `src/main.rs`

Add optional flags to `Commands::Run`:
```rust
#[derive(Args)]
struct RunArgs {
    // ... existing args ...
    
    /// Only execute prepare step, then exit
    #[arg(long)]
    prepare_only: bool,
    
    /// Skip cleanup of prepared objects
    #[arg(long)]
    no_cleanup: bool,
}
```

**Implementation**:
```rust
if args.prepare_only {
    if let Some(prepare) = &config.prepare {
        prepare_objects(prepare).await?;
        info!("Prepare-only mode: objects created, exiting");
        return Ok(());
    } else {
        bail!("--prepare-only requires 'prepare' section in config");
    }
}

// Override cleanup from CLI
if args.no_cleanup {
    if let Some(prepare) = &mut config.prepare {
        prepare.cleanup = false;
    }
}
```

**Acceptance Criteria**:
- [ ] `--prepare-only` creates objects and exits
- [ ] `--no-cleanup` prevents deletion
- [ ] Error if `--prepare-only` without prepare config

#### 1.4 Example YAML

**File**: `tests/configs/warp_parity_mixed.yaml`

```yaml
duration: 300s  # 5 minutes like Warp default
concurrency: 20 # Warp's default

# Prepare step: ensure 50K objects exist
prepare:
  ensure_objects:
    - base_uri: "s3://benchmark-bucket/mixed/"
      count: 50000
      min_size: 1048576  # 1 MiB
      max_size: 1048576
      fill: zero
  cleanup: false  # Keep objects for repeated runs

# Warp-style target (optional, makes workload paths relative)
target: "s3://benchmark-bucket/"

# Mixed workload matching Warp patterns
workload:
  - weight: 50
    op: get
    path: "mixed/*"  # Relative to target
    
  - weight: 30
    op: put
    path: "mixed/put-*"
    object_size: 1048576
    
  - weight: 10
    op: list
    path: "mixed/"
    
  - weight: 5
    op: stat
    path: "mixed/*"
    
  - weight: 5
    op: delete
    path: "mixed/temp-*"
```

**Testing**:
```bash
# Prepare objects once
./target/release/io-bench run --config tests/configs/warp_parity_mixed.yaml --prepare-only

# Run test (objects already exist)
./target/release/io-bench run --config tests/configs/warp_parity_mixed.yaml

# Run with cleanup
./target/release/io-bench run --config tests/configs/warp_parity_mixed.yaml
```

---

### Phase 2: Advanced Replay Remapping (Priority: HIGH)

**Goal**: Support 1→N, N→1, N↔N remapping like warp-replay

#### 2.1 Remap Config Schema

**File**: `src/remap.rs` (NEW)

```rust
//! URI remapping engine for replay operations
//!
//! Supports flexible source→target mappings:
//! - 1→1: Simple substitution
//! - 1→N: Fanout with strategies (round-robin, random, sticky-key)
//! - N→1: Consolidation to single target
//! - N↔N: General regex-based remapping

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Deserialize, Clone)]
pub struct RemapConfig {
    /// Ordered list of rules; first match wins
    pub rules: Vec<RemapRule>,
}

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

#[derive(Debug, Deserialize, Clone)]
pub struct MatchSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TargetSpec {
    pub host: String,
    pub bucket: String,
    pub prefix: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ManyTargets {
    pub targets: Vec<TargetSpec>,
    
    /// Strategy: "round_robin", "random", "sticky_key"
    #[serde(default = "default_strategy")]
    pub strategy: String,
}

fn default_strategy() -> String {
    "round_robin".to_string()
}

/// URI components for matching/remapping
#[derive(Debug, Clone)]
pub struct ParsedUri {
    pub scheme: String,
    pub host: String,
    pub bucket: String,
    pub prefix: String,
    pub key: String,
}

impl ParsedUri {
    /// Parse URI into components
    /// Examples:
    /// - "s3://bucket/prefix/key.dat" → bucket="bucket", prefix="prefix/", key="key.dat"
    /// - "gs://bucket/path/to/file" → bucket="bucket", prefix="path/to/", key="file"
    pub fn parse(uri: &str) -> Result<Self> {
        use url::Url;
        
        let url = Url::parse(uri)
            .with_context(|| format!("Invalid URI: {}", uri))?;
        
        let scheme = url.scheme().to_string();
        let host = url.host_str().unwrap_or("").to_string();
        
        let path = url.path().trim_start_matches('/');
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        
        let bucket = parts.get(0).unwrap_or(&"").to_string();
        let rest = parts.get(1).unwrap_or(&"");
        
        // Split into prefix and key
        let (prefix, key) = if let Some(idx) = rest.rfind('/') {
            (rest[..=idx].to_string(), rest[idx+1..].to_string())
        } else {
            ("".to_string(), rest.to_string())
        };
        
        Ok(Self {
            scheme,
            host,
            bucket,
            prefix,
            key,
        })
    }
    
    /// Reconstruct URI from components
    pub fn to_uri(&self) -> String {
        format!("{}://{}/{}{}{}", 
            self.scheme, 
            self.bucket,
            self.prefix,
            if self.prefix.is_empty() { "" } else { "" },
            self.key
        )
    }
}

/// Remapping engine
pub struct RemapEngine {
    config: RemapConfig,
    
    /// State for round-robin
    round_robin_state: Arc<Mutex<HashMap<usize, usize>>>,
    
    /// State for sticky-key (key → target index)
    sticky_state: Arc<Mutex<HashMap<String, usize>>>,
}

impl RemapEngine {
    pub fn new(config: RemapConfig) -> Self {
        Self {
            config,
            round_robin_state: Arc::new(Mutex::new(HashMap::new())),
            sticky_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    
    /// Apply remapping rules to a URI
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
    
    fn matches(&self, parsed: &ParsedUri, spec: &MatchSpec) -> bool {
        if let Some(host) = &spec.host {
            if !parsed.host.contains(host) {
                return false;
            }
        }
        
        if let Some(bucket) = &spec.bucket {
            if parsed.bucket != *bucket {
                return false;
            }
        }
        
        if let Some(prefix) = &spec.prefix {
            if !parsed.prefix.starts_with(prefix) {
                return false;
            }
        }
        
        true
    }
    
    fn apply_target(&self, parsed: &ParsedUri, target: &TargetSpec) -> String {
        format!("{}://{}/{}{}",
            parsed.scheme,  // Keep original scheme
            target.bucket,
            target.prefix,
            parsed.key
        )
    }
    
    fn select_target_from_many<'a>(
        &self,
        rule_idx: usize,
        key: &str,
        many: &'a ManyTargets
    ) -> Result<&'a TargetSpec> {
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
                let idx = rand::thread_rng().gen_range(0..many.targets.len());
                Ok(&many.targets[idx])
            }
            
            "sticky_key" => {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                
                let mut state = self.sticky_state.lock().unwrap();
                let idx = *state.entry(key.to_string()).or_insert_with(|| {
                    // Hash key to get stable target
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
    fn test_parse_uri() {
        let uri = "s3://bucket/path/to/file.dat";
        let parsed = ParsedUri::parse(uri).unwrap();
        assert_eq!(parsed.scheme, "s3");
        assert_eq!(parsed.bucket, "bucket");
        assert_eq!(parsed.prefix, "path/to/");
        assert_eq!(parsed.key, "file.dat");
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
                    host: "s3://dest/".to_string(),
                    bucket: "dest".to_string(),
                    prefix: "new/".to_string(),
                },
            }],
        };
        
        let engine = RemapEngine::new(config);
        let result = engine.remap("s3://src/old/file.dat").unwrap();
        assert_eq!(result, "s3://dest/new/file.dat");
    }
}
```

#### 2.2 Integration with Replay

**File**: `src/replay_streaming.rs`

Update `ReplayConfig`:
```rust
pub struct ReplayConfig {
    // ... existing fields ...
    
    /// Optional remap configuration
    pub remap_config: Option<RemapConfig>,
}
```

Update `execute_operation`:
```rust
async fn execute_operation(
    entry: &OpLogEntry,
    config: &ReplayConfig,
    remap_engine: Option<&RemapEngine>,
) -> Result<()> {
    // Apply remapping if configured
    let uri = if let Some(engine) = remap_engine {
        engine.remap(&entry.file)
            .with_context(|| format!("Failed to remap URI: {}", entry.file))?
    } else if let Some(target) = &config.target_uri {
        // Simple 1→1 retargeting (existing behavior)
        retarget_uri(&entry.file, target)
    } else {
        entry.file.clone()
    };
    
    // ... rest of execution logic ...
}
```

#### 2.3 CLI Integration

**File**: `src/main.rs`

Update `Commands::Replay`:
```rust
#[derive(Args)]
struct ReplayArgs {
    // ... existing args ...
    
    /// Remap configuration file (YAML)
    #[arg(long)]
    remap: Option<PathBuf>,
}
```

**Implementation**:
```rust
let remap_config = if let Some(remap_path) = &args.remap {
    let file = std::fs::File::open(remap_path)
        .context("Failed to open remap config")?;
    let config: RemapConfig = serde_yaml::from_reader(file)
        .context("Failed to parse remap config")?;
    Some(config)
} else {
    None
};

replay_config.remap_config = remap_config;
```

#### 2.4 Example Remap YAML

**File**: `tests/configs/remap_examples.yaml`

```yaml
# Example remapping configurations

rules:
  # 1→1: Simple bucket rename
  - match:
      bucket: "old-bucket"
      prefix: "logs/"
    map_to:
      host: "s3://new-bucket/"
      bucket: "new-bucket"
      prefix: "archived-logs/"
  
  # 1→N: Fanout to multiple targets (round-robin)
  - match:
      bucket: "source"
      prefix: "media/"
    map_to_many:
      targets:
        - host: "s3://replica1/"
          bucket: "replica1"
          prefix: "media/"
        - host: "s3://replica2/"
          bucket: "replica2"
          prefix: "media/"
        - host: "gs://gcs-replica/"
          bucket: "gcs-replica"
          prefix: "media/"
      strategy: "round_robin"
  
  # N→1: Consolidate multiple sources
  - match_any:
      - bucket: "temp-1"
      - bucket: "temp-2"
      - bucket: "temp-3"
    map_to:
      host: "s3://consolidated/"
      bucket: "consolidated"
      prefix: "merged/"
  
  # Regex: Advanced pattern matching
  - regex: "^s3://prod-([^/]+)/(.*)$"
    replace: "s3://staging-$1/$2"
```

**Testing**:
```bash
# Replay with remapping
./target/release/io-bench replay \
  --op-log production.tsv.zst \
  --remap tests/configs/remap_examples.yaml \
  --speed 2.0
```

**Acceptance Criteria**:
- [ ] 1→1 remapping works
- [ ] 1→N with round-robin strategy
- [ ] 1→N with random strategy
- [ ] 1→N with sticky-key (consistent per key)
- [ ] N→1 consolidation
- [ ] Regex-based remapping
- [ ] No match returns original URI

---

### Phase 3: UX Polish & Documentation (Priority: MEDIUM)

#### 3.1 Documentation Updates

**File**: `README.md`

Add section:
```markdown
## Warp Compatibility Mode

io-bench can run tests nearly identical to MinIO Warp and warp-replay:

### Mixed Workloads (like Warp)
```bash
# Create YAML config with prepare step
cat > mixed.yaml <<EOF
duration: 300s
concurrency: 20
prepare:
  ensure_objects:
    - base_uri: "s3://bench/test/"
      count: 50000
      min_size: 1048576
  cleanup: false
workload:
  - {weight: 50, op: get, path: "test/*"}
  - {weight: 30, op: put, path: "test/*", object_size: 1048576}
  - {weight: 10, op: list, path: "test/"}
  - {weight: 5, op: stat, path: "test/*"}
  - {weight: 5, op: delete, path: "test/*"}
EOF

# Run like Warp
./target/release/io-bench run --config mixed.yaml
```

### Replay with Remapping (like warp-replay)
```bash
# 1→N fanout replay
./target/release/io-bench replay \
  --op-log production.tsv.zst \
  --remap fanout-config.yaml \
  --speed 2.0
```
```

**File**: `docs/WARP_PARITY.md` (NEW)

Complete guide comparing Warp and io-bench commands, config files, and workflows.

#### 3.2 Default Concurrency

**File**: `src/config.rs`

Update default:
```rust
fn default_concurrency() -> usize {
    20  // Match Warp's default
}
```

Document in README:
```markdown
Default concurrency: 20 (matching MinIO Warp)
```

#### 3.3 Variable Substitution (Optional)

**File**: `src/config.rs`

Add CLI flag:
```rust
#[derive(Args)]
struct RunArgs {
    // ... existing ...
    
    /// Variable substitutions (key=value)
    #[arg(long = "var", value_parser = parse_key_val::<String, String>)]
    vars: Vec<(String, String)>,
}

fn parse_key_val<T, U>(s: &str) -> Result<(T, U)>
where
    T: std::str::FromStr,
    T::Err: Display,
    U: std::str::FromStr,
    U::Err: Display,
{
    let pos = s.find('=')
        .ok_or_else(|| anyhow!("invalid KEY=value: no `=` found in `{}`", s))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}
```

YAML template example:
```yaml
# Use {{VAR}} syntax
target: "s3://{{BUCKET}}/{{PREFIX}}/"
duration: {{DURATION}}
concurrency: {{CONCURRENCY}}
```

Usage:
```bash
./target/release/io-bench run \
  --config template.yaml \
  --var BUCKET=my-bucket \
  --var PREFIX=test \
  --var DURATION=300s \
  --var CONCURRENCY=64
```

---

### Phase 4: Dependencies & Testing (Priority: HIGH)

#### 4.1 Cargo.toml Updates

```toml
[dependencies]
# ... existing ...

# For remapping
regex = "1"
globset = "0.4"  # Optional: glob pattern support
hashbrown = "0.14"  # For sticky-key hash maps

# For random fill
rand = "0.8"

[dev-dependencies]
# ... existing ...
```

#### 4.2 Integration Tests

**File**: `tests/warp_parity_tests.rs` (NEW)

```rust
//! Integration tests for Warp parity features

use anyhow::Result;
use io_bench::config::*;
use io_bench::workload::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn test_prepare_creates_objects() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}/", temp_dir.path().display());
    
    let prepare = PrepareConfig {
        ensure_objects: vec![EnsureSpec {
            base_uri: base_uri.clone(),
            count: 100,
            min_size: 1024,
            max_size: 1024,
            fill: FillPattern::Zero,
        }],
        cleanup: false,
    };
    
    let prepared = prepare_objects(&prepare).await?;
    assert_eq!(prepared.len(), 100);
    
    // Verify objects exist
    let store = create_store_for_uri(&base_uri)?;
    let objects = store.list(&base_uri, true).await?;
    assert_eq!(objects.len(), 100);
    
    Ok(())
}

#[tokio::test]
async fn test_prepare_skips_existing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let base_uri = format!("file://{}/", temp_dir.path().display());
    
    // First prepare: create 50
    let prepare1 = PrepareConfig {
        ensure_objects: vec![EnsureSpec {
            base_uri: base_uri.clone(),
            count: 50,
            min_size: 1024,
            max_size: 1024,
            fill: FillPattern::Zero,
        }],
        cleanup: false,
    };
    
    let prepared1 = prepare_objects(&prepare1).await?;
    assert_eq!(prepared1.len(), 50);
    
    // Second prepare: should create 50 more
    let prepare2 = PrepareConfig {
        ensure_objects: vec![EnsureSpec {
            base_uri: base_uri.clone(),
            count: 100,
            min_size: 1024,
            max_size: 1024,
            fill: FillPattern::Zero,
        }],
        cleanup: false,
    };
    
    let prepared2 = prepare_objects(&prepare2).await?;
    assert_eq!(prepared2.len(), 50); // Only 50 new
    
    Ok(())
}

#[test]
fn test_remap_simple() -> Result<()> {
    use io_bench::remap::*;
    
    let config = RemapConfig {
        rules: vec![RemapRule::Simple {
            match_spec: MatchSpec {
                host: None,
                bucket: Some("src".to_string()),
                prefix: None,
            },
            map_to: TargetSpec {
                host: "s3://dest/".to_string(),
                bucket: "dest".to_string(),
                prefix: "new/".to_string(),
            },
        }],
    };
    
    let engine = RemapEngine::new(config);
    let result = engine.remap("s3://src/old/file.dat")?;
    assert_eq!(result, "s3://dest/new/file.dat");
    
    Ok(())
}

#[test]
fn test_remap_fanout_round_robin() -> Result<()> {
    use io_bench::remap::*;
    
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
                        host: "s3://dest1/".to_string(),
                        bucket: "dest1".to_string(),
                        prefix: "".to_string(),
                    },
                    TargetSpec {
                        host: "s3://dest2/".to_string(),
                        bucket: "dest2".to_string(),
                        prefix: "".to_string(),
                    },
                ],
                strategy: "round_robin".to_string(),
            },
        }],
    };
    
    let engine = RemapEngine::new(config);
    
    let r1 = engine.remap("s3://src/file1.dat")?;
    let r2 = engine.remap("s3://src/file2.dat")?;
    let r3 = engine.remap("s3://src/file3.dat")?;
    
    // Should alternate
    assert_eq!(r1, "s3://dest1/file1.dat");
    assert_eq!(r2, "s3://dest2/file2.dat");
    assert_eq!(r3, "s3://dest1/file3.dat");
    
    Ok(())
}
```

**Run tests**:
```bash
cargo test --test warp_parity_tests -- --nocapture
```

---

## Implementation Timeline

### Sprint 1 (Week 1): Core Prepare Functionality
- [ ] Config schema extension (1.1)
- [ ] Prepare implementation (1.2)
- [ ] CLI flags (1.3)
- [ ] Example YAML (1.4)
- [ ] Basic testing

### Sprint 2 (Week 2): Remap Engine
- [ ] Remap config schema (2.1)
- [ ] RemapEngine implementation (2.1)
- [ ] Integration with replay (2.2)
- [ ] CLI integration (2.3)
- [ ] Example remap YAML (2.4)

### Sprint 3 (Week 3): Testing & Polish
- [ ] Integration tests (4.2)
- [ ] Documentation (3.1)
- [ ] Default updates (3.2)
- [ ] Variable substitution (3.3 - optional)
- [ ] Performance validation

### Sprint 4 (Week 4): Validation & Release
- [ ] End-to-end Warp comparison testing
- [ ] Performance benchmarking
- [ ] Documentation review
- [ ] Release v0.5.0

---

## Success Criteria

### Functional Requirements
- [ ] Can run identical mixed workload tests as Warp
- [ ] Prepare step creates exact object counts
- [ ] Cleanup removes only prepared objects
- [ ] 1→1 remapping matches simple retargeting
- [ ] 1→N fanout with all 3 strategies
- [ ] N→1 consolidation works
- [ ] Regex remapping handles complex patterns

### Performance Requirements
- [ ] Prepare step: 10K objects/minute minimum
- [ ] Remap overhead: <1ms per operation
- [ ] Memory: Constant during streaming replay with remap

### Documentation Requirements
- [ ] README includes Warp comparison
- [ ] WARP_PARITY.md complete guide
- [ ] All YAML examples tested
- [ ] CLI help text updated

---

## Risk Mitigation

### Risk: Breaking existing configs
**Mitigation**: All new features are optional (prepare, remap)

### Risk: Performance regression in prepare
**Mitigation**: Batch operations, progress logging, optional parallelism

### Risk: Remap complexity
**Mitigation**: Start with simple rules, add complexity incrementally

### Risk: Testing coverage
**Mitigation**: Integration tests for each feature, real-world validation

---

## Future Enhancements (Post-v0.5.0)

1. **Advanced Fill Patterns**: Dedup-aware, compressible data
2. **Prepare Parallelism**: Concurrent PUT during prepare
3. **Remap Caching**: LRU cache for regex matches
4. **Remap Analytics**: Report which rules matched how many ops
5. **Warp CLI Compatibility**: Accept Warp-style CLI flags
6. **Benchmark Comparison Tool**: Side-by-side Warp vs io-bench results

---

## Conclusion

This plan provides a clear path to Warp/warp-replay parity while maintaining io-bench's multi-backend advantage. The phased approach allows incremental delivery with testing at each step.

**The analysis is ACCURATE** - it correctly identifies gaps and provides concrete, implementable solutions that align with io-bench's existing architecture.

Next step: Choose Phase 1 (Prepare) or Phase 2 (Remap) to start implementation.
