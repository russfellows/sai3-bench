# Pre-Flight Validation Implementation Plan

**Date**: February 2, 2026  
**Status**: Updated with User Requirements  
**Priority**: High - Users experiencing cryptic failures without actionable error messages  
**Focus**: Filesystem validation (primary), Object storage validation (secondary)

---

## Problem Statement

Users running distributed workloads encounter failures like:

```
ERROR: h2 protocol error: error reading a body from connection
CRITICAL: Zero operations completed - workload did not execute!
```

**Root causes include:**
- File paths don't exist or lack permissions
- Invalid S3 credentials or bucket access
- Insufficient memory/resources
- Network connectivity issues

**Current behavior**: Agents crash 27+ seconds after starting, providing no diagnostic information.

**Desired behavior**: Validate configuration and environment BEFORE starting workload, with clear error messages and actionable fix suggestions.

---

## Architecture Overview

### Current Flow
```
Controller → Agent: Send Config
Agent: Validate Config (syntax only)
Agent: Report READY
Controller: Send START
Agent: Begin Workload → [CRASH with cryptic error]
```

### Proposed Flow (NEW PRE-FLIGHT PHASE)
```
Controller → Agent: Send Config
Agent: Validate Config (syntax only)
Agent: Report READY
Controller: Send PRE_FLIGHT (NEW PHASE before PREPARE)
Agent: RUN PRE-FLIGHT CHECKS
  ├─ User/Group ID Analysis
  ├─ Progressive Access Testing (stat → list → read → write → delete)
  ├─ Multi-Endpoint Validation (test ALL endpoints)
  ├─ Instance-Level Permission Detection (EC2 IAM, GCP SA, Azure MI)
  └─ Resource Validation (optional)
Agent: Report PRE_FLIGHT_COMPLETE (or FAILED with detailed errors)
Controller: Display validation results with suggestions
Controller: Aggregate errors across all agents
Controller: Send PREPARE (only if all agents passed pre-flight)
Agent: Begin PREPARE phase
Controller: Send START
Agent: Begin Workload
```

---

## Implementation Phases

### Phase 1: Core Pre-Flight Validation (PRIORITY - FILESYSTEM)

**Estimated effort**: 3-4 days

#### 1.1 File System Validation (TOP PRIORITY)

**File**: `crates/core/src/validation/filesystem.rs` (NEW)

```rust
pub async fn validate_filesystem(config: &Config) -> ValidationResult {
    let mut results = Vec::new();
    
    // 1. User/Group ID Analysis
    let current_uid = unsafe { libc::getuid() };
    let current_gid = unsafe { libc::getgid() };
    results.push(check_user_identity(current_uid, current_gid));
    
    // 2. Directory Existence & Ownership
    let metadata = std::fs::metadata(&config.data_root)?;
    let (owner_uid, owner_gid) = get_owner(&metadata);
    results.push(check_ownership_match(current_uid, current_gid, owner_uid, owner_gid));
    
    // 3. Mount Point Detection (informational)
    if let Some(mount_info) = detect_mount_type(&config.data_root) {
        results.push(ValidationResult::info(
            ErrorType::System,
            format!("Mount type: {} ({})", mount_info.fs_type, mount_info.device),
            "No action needed - informational only"
        ));
    }
    
    // 4. Progressive Access Testing (stat → list → read → write → delete)
    results.push(test_stat_access(&config.data_root).await?);
    results.push(test_list_access(&config.data_root).await?);
    results.push(test_read_access(&config.data_root).await?);
    results.push(test_write_access(&config.data_root).await?);
    results.push(test_mkdir_access(&config.data_root).await?);
    results.push(test_delete_access(&config.data_root).await?);
    
    // 5. Quota Limits (if easy to detect)
    if let Some(quota) = check_quota(&config.data_root) {
        results.push(quota);
    }
    
    // 6. Disk Space (for PUT workloads)
    if config.operations.contains(&Operation::Put) {
        results.push(check_disk_space(config).await?);
    }
    
    Ok(ValidationSummary::new(results))
}

// Progressive access testing functions
async fn test_stat_access(path: &Path) -> Result<ValidationResult> {
    // Test stat/metadata access (most basic)
}

async fn test_list_access(path: &Path) -> Result<ValidationResult> {
    // Test directory listing (readdir)
}

async fn test_read_access(path: &Path) -> Result<ValidationResult> {
    // Test reading existing file (if any) or create temporary test file
}

async fn test_write_access(path: &Path) -> Result<ValidationResult> {
    // Test creating .sai3bench_preflight_test file
}

async fn test_mkdir_access(path: &Path) -> Result<ValidationResult> {
    // Test creating subdirectory (important for prepare phase)
}

async fn test_delete_access(path: &Path) -> Result<ValidationResult> {
    // Test deleting test file/directory created above
}
```

**New Dependencies**:
```toml
# For uid/gid and file metadata on Unix
libc = "0.2"
nix = "0.27"  # For mount point detection and quota checks
```

**Tests**: Create test with non-existent path, permission-denied path, read-only path, uid/gid mismatch

#### 1.2 Object Storage Validation (SECOND PRIORITY)

**File**: `crates/core/src/validation/object_storage.rs` (NEW)

```rust
pub async fn validate_object_storage(config: &Config) -> ValidationResult {
    let mut results = Vec::new();
    
    // 0. Detect permission source (env vars vs instance-level)
    results.push(detect_permission_source().await);
    
    // For EACH endpoint in multi_endpoint config:
    let endpoints = get_all_endpoints(config);
    for (idx, endpoint) in endpoints.iter().enumerate() {
        log::info!("Validating endpoint {}/{}: {}", idx + 1, endpoints.len(), endpoint);
        
        // Progressive Access Testing (head → list → get → put → delete)
        results.push(test_head_bucket(endpoint).await?);
        results.push(test_list_bucket(endpoint).await?);
        results.push(test_get_object(endpoint).await?);
        
        if config.operations.contains(&Operation::Put) {
            results.push(test_put_object(endpoint).await?);
            results.push(test_delete_object(endpoint).await?);
        }
    }
    
    // Warn if skip_verification=true with GET operations
    if config.skip_verification && config.operations.contains(&Operation::Get) {
        results.push(ValidationResult::warning(
            ErrorType::Configuration,
            "skip_verification=true with GET operations may fail if objects don't exist",
            "Set skip_verification=false or ensure objects exist before running workload"
        ));
    }
    
    Ok(ValidationSummary::new(results))
}

// Instance-level permission detection
async fn detect_permission_source() -> ValidationResult {
    // Check AWS EC2 instance IAM role
    if let Ok(role) = check_ec2_iam_role().await {
        return ValidationResult::info(
            ErrorType::Authentication,
            format!("Using EC2 IAM role: {}", role),
            "Instance-level permissions detected"
        );
    }
    
    // Check GCP service account
    if let Ok(sa) = check_gcp_service_account().await {
        return ValidationResult::info(
            ErrorType::Authentication,
            format!("Using GCP service account: {}", sa),
            "Instance-level permissions detected"
        );
    }
    
    // Check Azure managed identity
    if let Ok(mi) = check_azure_managed_identity().await {
        return ValidationResult::info(
            ErrorType::Authentication,
            format!("Using Azure managed identity: {}", mi),
            "Instance-level permissions detected"
        );
    }
    
    // Check environment variables
    if std::env::var("AWS_ACCESS_KEY_ID").is_ok() {
        return ValidationResult::info(
            ErrorType::Authentication,
            "Using AWS credentials from environment variables",
            "Credentials: AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY"
        );
    }
    
    ValidationResult::warning(
        ErrorType::Authentication,
        "No credentials detected (env vars or instance-level)",
        "Set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or use instance IAM role"
    )
}

async fn check_ec2_iam_role() -> Result<String> {
    // Query http://169.254.169.254/latest/meta-data/iam/security-credentials/
}

async fn check_gcp_service_account() -> Result<String> {
    // Query http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/
}

async fn check_azure_managed_identity() -> Result<String> {
    // Query Azure IMDS endpoint
}

// Progressive access testing (head → list → get → put → delete)
async fn test_head_bucket(endpoint: &str) -> Result<ValidationResult> {
    // Test HEAD bucket (most basic - does bucket exist?)
}

async fn test_list_bucket(endpoint: &str) -> Result<ValidationResult> {
    // Test LIST objects (requires s3:ListBucket)
}

async fn test_get_object(endpoint: &str) -> Result<ValidationResult> {
    // Test GET on existing object or create test object first
}

async fn test_put_object(endpoint: &str) -> Result<ValidationResult> {
    // Test PUT .sai3bench_preflight_test object
}

async fn test_delete_object(endpoint: &str) -> Result<ValidationResult> {
    // Test DELETE test object created above
}
```

**Authentication error mapping**:
- `NoCredentialsError` → "Set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY or use EC2 IAM role"
- `InvalidCredentials` → "Check credentials are valid for this account"
- `AccessDenied` → "Grant s3:ListBucket, s3:GetObject, s3:PutObject permissions"
- `BucketNotFound` → "Check bucket name and region in configuration"

**New Dependencies**:
```toml
# For metadata service queries
reqwest = { version = "0.11", features = ["json"] }
```

**Tests**: Mock S3 responses for auth failures, missing bucket, permission denied, all endpoints validated

#### 1.3 Structured Error Types

**File**: `crates/core/src/validation/mod.rs` (NEW)

```rust
pub enum ErrorType {
    Authentication,   // Invalid credentials, missing auth
    Permission,       // Access denied, insufficient permissions
    Network,          // DNS, connectivity, timeouts  
    Configuration,    // Invalid config, missing fields, bad paths
    Resource,         // Memory, disk space, file descriptors
    System,           // OS errors, mount failures
}

pub enum ResultLevel {
    Success,  // Check passed
    Info,     // Informational (e.g., mount type detected)
    Warning,  // Potential issue but not fatal
    Error,    // Fatal issue - must fix before proceeding
}

pub struct ValidationResult {
    pub level: ResultLevel,
    pub error_type: Option<ErrorType>,
    pub message: String,                 // What went wrong (or what was detected)
    pub suggestion: String,              // How to fix it (or additional context)
    pub details: Option<String>,         // Technical details (stack trace, uid/gid, etc)
    pub test_phase: String,              // "stat", "list", "read", "write", "delete", etc.
}

impl ValidationResult {
    pub fn error(type: ErrorType, message: impl Into<String>, suggestion: impl Into<String>) -> Self;
    pub fn warning(type: ErrorType, message: impl Into<String>, suggestion: impl Into<String>) -> Self;
    pub fn info(type: ErrorType, message: impl Into<String>, suggestion: impl Into<String>) -> Self;
    pub fn success(test_phase: impl Into<String>) -> Self;
}

pub struct ValidationSummary {
    pub results: Vec<ValidationResult>,
}

impl ValidationSummary {
    pub fn has_errors(&self) -> bool {
        self.results.iter().any(|r| matches!(r.level, ResultLevel::Error))
    }
    
    pub fn has_warnings(&self) -> bool {
        self.results.iter().any(|r| matches!(r.level, ResultLevel::Warning))
    }
    
    pub fn error_count(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.level, ResultLevel::Error)).count()
    }
    
    pub fn warning_count(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.level, ResultLevel::Warning)).count()
    }
}
```

#### 1.4 Agent Integration (NEW PRE_FLIGHT PHASE)

**File**: `crates/agent/src/workload.rs`

**NEW**: Add separate `run_preflight()` function called from gRPC handler

```rust
// NEW: Pre-flight phase (called before PREPARE)
pub async fn run_preflight(config: &Config) -> Result<ValidationSummary> {
    log::info!("🔍 Starting pre-flight validation...");
    
    let validation = run_preflight_checks(config).await?;
    
    if validation.has_errors() {
        log::error!("❌ Pre-flight validation failed: {} errors", validation.error_count());
        return Err(ValidationError::Failed(validation));
    }
    
    if validation.has_warnings() {
        log::warn!("⚠️ Pre-flight warnings: {} warnings", validation.warning_count());
    } else {
        log::info!("✅ Pre-flight validation passed");
    }
    
    Ok(validation)
}

// Existing prepare/run/cleanup functions unchanged
pub async fn run_workload(config: Config) -> Result<WorkloadResult> {
    // Pre-flight already completed - proceed with workload
    // ...
}

async fn run_preflight_checks(config: &Config) -> Result<ValidationSummary> {
    let mut results = Vec::new();
    
    // 1. Validate storage backend (file:// or s3://)
    match &config.uri_prefix {
        Some(uri) if uri.starts_with("file://") => {
            results.push(validate_filesystem(config).await?);
        }
        Some(uri) if uri.starts_with("s3://") || uri.starts_with("az://") => {
            results.push(validate_object_storage(config).await?);
        }
        _ => {}
    }
    
    // 2. Validate resources (memory, threads, FDs)
    results.push(validate_resources(config).await?);
    
    Ok(ValidationSummary::new(results))
}
```

#### 1.5 gRPC Protocol Update (NEW PRE_FLIGHT RPC)

**File**: `proto/agent.proto`

```proto
service AgentService {
  rpc SendConfig(ConfigMessage) returns (StatusResponse);
  rpc PreFlight(PreFlightRequest) returns (PreFlightResponse);  // NEW
  rpc Prepare(PrepareRequest) returns (StatusResponse);
  rpc Start(StartRequest) returns (stream ProgressUpdate);
  rpc Stop(StopRequest) returns (StatusResponse);
  rpc GetStatus(StatusRequest) returns (StatusResponse);
}

// NEW: Pre-flight request/response
message PreFlightRequest {
  // Empty - uses config already sent via SendConfig
}

message PreFlightResponse {
  bool passed = 1;  // true if no errors (warnings OK)
  repeated ValidationResult results = 2;
  int32 error_count = 3;
  int32 warning_count = 4;
  int32 info_count = 5;
}

message ValidationResult {
  ResultLevel level = 1;
  ErrorType error_type = 2;
  string message = 3;
  string suggestion = 4;
  string details = 5;        // Optional technical details (uid/gid, etc)
  string test_phase = 6;     // "stat", "list", "read", "write", "delete"
}

enum ResultLevel {
  RESULT_LEVEL_SUCCESS = 0;
  RESULT_LEVEL_INFO = 1;
  RESULT_LEVEL_WARNING = 2;
  RESULT_LEVEL_ERROR = 3;
}

enum ErrorType {
  ERROR_TYPE_UNKNOWN = 0;
  ERROR_TYPE_AUTHENTICATION = 1;
  ERROR_TYPE_PERMISSION = 2;
  ERROR_TYPE_NETWORK = 3;
  ERROR_TYPE_CONFIGURATION = 4;
  ERROR_TYPE_RESOURCE = 5;
  ERROR_TYPE_SYSTEM = 6;
}
```

**After editing proto**: Run `cargo build` to regenerate Rust code from proto definitions.

#### 1.6 Controller Display (AGGREGATE ERRORS ACROSS AGENTS)

**File**: `crates/controller/src/distributed.rs`

**NEW**: Add pre-flight phase to distributed workflow

```rust
pub async fn run_distributed_workload(config: Config, agents: Vec<String>) -> Result<()> {
    // 1. Send config to all agents
    send_config_to_agents(&config, &agents).await?;
    
    // 2. NEW: Run pre-flight checks on all agents
    println!("\n🔍 Running pre-flight validation on {} agents...", agents.len());
    let preflight_results = run_preflight_on_agents(&agents).await?;
    
    // 3. Display aggregated results
    display_preflight_results(&preflight_results);
    
    // 4. Check if any agent failed
    let failed_agents = preflight_results.iter()
        .filter(|(_, r)| !r.passed)
        .count();
    
    if failed_agents > 0 {
        return Err(anyhow!("Pre-flight validation failed on {} agents", failed_agents));
    }
    
    // 5. Continue with existing workflow (prepare → start → cleanup)
    run_prepare_phase(&agents).await?;
    run_workload_phase(&agents).await?;
    run_cleanup_phase(&agents).await?;
    
    Ok(())
}

async fn run_preflight_on_agents(agents: &[String]) -> Result<HashMap<String, PreFlightResponse>> {
    // Call PreFlight RPC on each agent concurrently
}

fn display_preflight_results(results: &HashMap<String, PreFlightResponse>) {
    // Aggregate errors by type across all agents
    let mut error_summary = HashMap::new();
    let mut warning_summary = HashMap::new();
    
    for (agent_id, response) in results {
        println!("\n📊 Agent {}: {} errors, {} warnings, {} info",
                 agent_id, response.error_count, response.warning_count, response.info_count);
        
        // Display per-agent results
        for result in &response.results {
            display_validation_result(agent_id, result);
            
            // Track for aggregation
            if matches!(result.level, ResultLevel::Error) {
                *error_summary.entry(&result.error_type).or_insert(0) += 1;
            }
            if matches!(result.level, ResultLevel::Warning) {
                *warning_summary.entry(&result.error_type).or_insert(0) += 1;
            }
        }
    }
    
    // Display aggregated summary
    println!("\n📈 Pre-flight Summary Across All Agents:");
    if !error_summary.is_empty() {
        println!("   ❌ Errors by type:");
        for (error_type, count) in error_summary {
            println!("      - {}: {} agents affected", error_type_name(error_type), count);
        }
    }
    if !warning_summary.is_empty() {
        println!("   ⚠️ Warnings by type:");
        for (error_type, count) in warning_summary {
            println!("      - {}: {} agents affected", error_type_name(error_type), count);
        }
    }
}

fn display_validation_result(agent_id: &str, result: &ValidationResult) {
    let (icon, prefix) = match result.level {
        ResultLevel::Error => ("❌", "ERROR"),
        ResultLevel::Warning => ("⚠️", "WARN"),
        ResultLevel::Info => ("ℹ️", "INFO"),
        ResultLevel::Success => ("✅", "OK"),
    };
    
    let type_icon = match result.error_type {
        ErrorType::Authentication => "🔐",
        ErrorType::Permission => "🚫",
        ErrorType::Network => "🌐",
        ErrorType::Configuration => "⚙️",
        ErrorType::Resource => "💾",
        ErrorType::System => "⚠️",
        _ => "",
    };
    
    println!("   {} {} [{}] {}", icon, prefix, result.test_phase, result.message);
    if !result.suggestion.is_empty() {
        println!("      {} 💡 {}", type_icon, result.suggestion);
    }
    if let Some(details) = &result.details {
        println!("      📋 Details: {}", details);
    }
}
```

---

### Phase 2: Resource Validation (MEDIUM PRIORITY)

**Estimated effort**: 1-2 days

#### 2.1 Memory & Thread Validation

**File**: `crates/core/src/validation/resources.rs` (NEW)

```rust
pub async fn validate_resources(config: &Config) -> ValidationResult {
    // 1. Estimate memory usage (buffer_size * threads * 2)
    // 2. Check available system memory
    // 3. Warn if >80% memory usage
    // 4. Check thread count vs system limits
    // 5. Check file descriptor limits
}
```

**Platform-specific**:
- Linux: Read `/proc/meminfo`, `/proc/sys/kernel/threads-max`, `ulimit -n`
- macOS: Use `sysctl` APIs
- Windows: WMI queries

#### 2.2 Disk Space Validation

```rust
// For PUT workloads only
let total_size = config.num_objects * config.object_size;
let available = fs2::available_space(&config.data_root)?;

if available < total_size * 1.2 {  // 20% safety margin
    return ValidationResult::error(
        ErrorType::Resource,
        format!("Insufficient disk space: need {} GB, have {} GB",
                total_size / 1_000_000_000, available / 1_000_000_000),
        "Free up disk space or reduce num_objects/object_size"
    );
}
```

**Dependency**: Add `fs2` crate for cross-platform disk space checks

---

### Phase 3: Enhanced Features (LOWER PRIORITY)

**Estimated effort**: 2-3 days

#### 3.1 Network Pre-Checks

**File**: `crates/core/src/validation/network.rs` (NEW)

- DNS resolution for S3 endpoints
- TCP connectivity test (5-second timeout)
- Optional latency measurement

#### 3.2 Skip Pre-Flight Option (Backwards Compatibility)

**Controller flag**: `--skip-preflight`

```bash
# Default: run pre-flight validation
sai3bench-ctl run --config test.yaml

# Skip pre-flight (dangerous - for backwards compatibility only)
sai3bench-ctl run --config test.yaml --skip-preflight
```

**Note**: Pre-flight is now a standard phase, not optional. Use `--skip-preflight` only if you're certain configuration is valid.

#### 3.3 Command-Line Options

```rust
#[derive(Parser)]
struct CliArgs {
    /// Skip pre-flight validation checks (backwards compatibility - dangerous!)
    #[arg(long)]
    skip_preflight: bool,
    
    /// Fail on validation warnings (strict mode)
    #[arg(long)]
    strict_validation: bool,
    
    /// Pre-flight timeout per agent (default: 30s)
    #[arg(long, default_value = "30")]
    preflight_timeout: u64,
}
```

---

## Key Files to Review

### Existing Architecture
1. **`crates/agent/src/workload.rs`** - Agent workload execution entry point
2. **`crates/controller/src/distributed.rs`** - Controller agent coordination
3. **`proto/agent.proto`** - gRPC protocol definition
4. **`crates/core/src/config.rs`** - Configuration structures
5. **`crates/core/src/storage/mod.rs`** - Storage backend abstractions

### Files to Create
1. **`crates/core/src/validation/mod.rs`** - Validation framework
2. **`crates/core/src/validation/filesystem.rs`** - File system checks
3. **`crates/core/src/validation/object_storage.rs`** - S3/Azure/GCS checks
4. **`crates/core/src/validation/resources.rs`** - System resource checks
5. **`crates/core/src/validation/network.rs`** - Network connectivity checks

### Files to Modify
1. **`crates/agent/src/workload.rs`** - Add validation calls
2. **`crates/controller/src/distributed.rs`** - Display validation results
3. **`proto/agent.proto`** - Add validation message types
4. **`Cargo.toml`** - Add dependencies (fs2 for disk space)

---

## Testing Strategy

### Unit Tests
```rust
#[cfg(test)]
mod tests {
    // 1. Test with non-existent file path
    // 2. Test with read-only directory
    // 3. Test with invalid S3 credentials (mocked)
    // 4. Test with insufficient disk space
    // 5. Test with excessive memory requirements
}
```

### Integration Tests
```bash
# Create test scenarios
tests/
  fixtures/
    no_permission/     # chmod 000
    read_only/         # chmod 444
    valid_path/        # chmod 755
    invalid_s3.yaml    # Bad credentials
    valid_config.yaml  # Working config
```

### Manual Testing Checklist
- [ ] File path doesn't exist → Clear error with mkdir suggestion
- [ ] Permission denied → Clear error with chmod suggestion
- [ ] Invalid AWS credentials → Auth error with env var suggestion
- [ ] Bucket doesn't exist → Config error with bucket name check
- [ ] Insufficient disk space → Resource error with space needed
- [ ] All checks pass → Workload executes normally

---

## Dependencies to Add

```toml
# In crates/core/Cargo.toml
[dependencies]
fs2 = "0.4"              # Cross-platform disk space checks
sysinfo = "0.30"         # System resource queries (memory, CPU)
libc = "0.2"             # For uid/gid on Unix
nix = { version = "0.27", features = ["user", "mount"] }  # Mount detection, quota
reqwest = { version = "0.11", features = ["json"] }  # Metadata service queries
```

---

## Success Criteria

**Phase 1 Complete When**:
1. ✅ Pre-flight runs as separate phase before PREPARE
2. ✅ Progressive access testing works (stat → list → read → write → delete)
3. ✅ User/group ID mismatches detected and reported with details
4. ✅ All endpoints in multi-endpoint configs validated
5. ✅ Instance-level permissions detected (EC2 IAM, GCP SA, Azure MI)
6. ✅ File permission errors show clear fix suggestions (uid/gid, chmod)
7. ✅ S3 auth errors show credential source and troubleshooting
8. ✅ Agents report validation errors to controller via PreFlight RPC
9. ✅ Controller aggregates errors across all agents
10. ✅ Controller displays structured error messages with icons and suggestions
11. ✅ Zero operations completed → Pre-flight catches issue before workload starts
12. ✅ All Phase 1 unit tests passing

**Example Success Output (Filesystem)**:
```
🔍 Running pre-flight validation on 4 agents...

📊 Agent agent-1: 0 errors, 0 warnings, 2 info
   ℹ️ INFO [identity] Current user: uid=1000 (testuser), gid=1000 (testgroup)
   ℹ️ INFO [mount] Mount type: nfs4 (/mnt/nfs-server:/export)
   ✅ OK [stat] Directory exists and is accessible
   ✅ OK [list] Directory listing successful
   ✅ OK [read] Read access confirmed
   ✅ OK [write] Write access confirmed (test file created)
   ✅ OK [mkdir] Subdirectory creation successful
   ✅ OK [delete] Delete access confirmed (test file removed)

📊 Agent agent-2: 1 errors, 0 warnings, 2 info
   ℹ️ INFO [identity] Current user: uid=1001 (user2), gid=1001 (group2)
   ℹ️ INFO [ownership] Directory owner: uid=0 (root), gid=0 (root), perms=drwxr-xr-x (755)
   ✅ OK [stat] Directory exists and is accessible
   ✅ OK [list] Directory listing successful
   ✅ OK [read] Read access confirmed
   ❌ ERROR [write] Permission denied creating test file
      🚫 💡 Grant write permission: sudo chmod g+w /mnt/storage && sudo chgrp group2 /mnt/storage
      📋 Details: EACCES: Permission denied - user not in owner group

📈 Pre-flight Summary Across All Agents:
   ❌ Errors by type:
      - Permission: 1 agents affected

❌ Pre-flight validation failed on 1 agents - fix errors before proceeding
```

**Example Success Output (Object Storage)**:
```
🔍 Running pre-flight validation on 2 agents...

📊 Agent agent-1: 0 errors, 0 warnings, 3 info
   ℹ️ INFO [credentials] Using EC2 IAM role: s3-benchmark-role
      💡 Instance-level permissions detected
   ℹ️ INFO [endpoint-1] Validating endpoint 1/2: s3://my-bucket/path/
   ✅ OK [head] Bucket exists and is accessible
   ✅ OK [list] List objects successful (s3:ListBucket granted)
   ✅ OK [get] Get object successful (s3:GetObject granted)
   ✅ OK [put] Put test object successful (s3:PutObject granted)
   ✅ OK [delete] Delete test object successful (s3:DeleteObject granted)
   ℹ️ INFO [endpoint-2] Validating endpoint 2/2: s3://my-bucket-2/path/
   ✅ OK [head] Bucket exists and is accessible
   ✅ OK [list] List objects successful
   ✅ OK [get] Get object successful
   ✅ OK [put] Put test object successful
   ✅ OK [delete] Delete test object successful

📊 Agent agent-2: 0 errors, 0 warnings, 2 info
   ℹ️ INFO [credentials] Using AWS credentials from environment variables
      💡 Credentials: AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY
   [similar output...]

✅ Pre-flight validation passed on all agents
```

---

## Timeline Estimate

- **Phase 1** (Core Validation): 2-3 days
- **Phase 2** (Resource Checks): 1-2 days  
- **Phase 3** (Enhanced Features): 2-3 days
- **Testing & Documentation**: 1 day

**Total**: ~6-9 days for complete implementation

---

## Notes for Next Agent

1. **Start with Phase 1.1** (file system validation) - easiest to test locally
2. **Use existing error types** in codebase as reference (see `crates/core/src/error.rs`)
3. **gRPC changes require proto regeneration** - run `cargo build` after editing `.proto`
4. **Test incrementally** - add validation for one error type at a time
5. **User reported issue**: Agents crash with "h2 protocol error" ~27s after workload start with ZERO operations - this is the pain point to solve

**Current behavior**: Silent failures, cryptic errors  
**Target behavior**: Clear diagnostics, actionable suggestions, fail-fast validation

---

**Ready to implement!** Start with Phase 1.1 and work through sequentially.
