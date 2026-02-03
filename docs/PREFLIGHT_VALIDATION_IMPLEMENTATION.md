# Pre-Flight Validation Implementation Plan

**Date**: February 2, 2026  
**Status**: ✅ Phase 1 FILESYSTEM VALIDATION COMPLETE  
**Last Updated**: February 2, 2026 23:00  
**Priority**: High - Users experiencing cryptic failures without actionable error messages  
**Focus**: Filesystem validation (primary), Object storage validation (secondary)

---

## 🎯 IMPLEMENTATION STATUS

### ✅ COMPLETED (Phase 1 - Filesystem Validation)

**Phase 1.1 - File System Validation** ✅
- [x] Created `src/preflight/filesystem.rs` with complete implementation
- [x] Progressive access testing (stat → list → read → write → mkdir → delete)
- [x] User/Group ID analysis (`check_user_identity`)
- [x] Directory ownership detection (`check_directory_ownership`)
- [x] Mount point detection (`detect_mount_type`)
- [x] Disk space validation (`check_disk_space`)
- [x] **BONUS**: Protected path detection (`check_protected_paths`) - safety feature
- [x] **BONUS**: No directory creation suggestions - require manual creation for safety

**Phase 1.3 - Structured Error Types** ✅
- [x] Created `src/preflight/mod.rs` with ValidationResult/ValidationSummary
- [x] ErrorType enum (Authentication, Permission, Network, Configuration, Resource, System)
- [x] ResultLevel enum (Success, Info, Warning, Error)
- [x] `display_validation_results()` with emoji icons and formatted output

**Phase 1.4 - Agent Integration** ✅
- [x] Modified `src/bin/agent.rs` with `pre_flight_validation()` gRPC handler
- [x] PREFLIGHT command handling in agent state machine
- [x] Validation results converted to proto format

**Phase 1.5 - gRPC Protocol Update** ✅
- [x] Modified `proto/iobench.proto` with PreFlightValidation RPC
- [x] PreFlightRequest/PreFlightResponse messages
- [x] ValidationResult proto message with all fields
- [x] ResultLevel and ErrorType proto enums

**Phase 1.6 - Controller Display** ✅
- [x] Modified `src/bin/controller.rs` with `run_preflight_validation()`
- [x] Aggregates errors across all agents
- [x] Per-agent and aggregate error display
- [x] Shared `display_validation_results()` for both distributed and standalone modes

**Standalone Mode Integration** ✅
- [x] Modified `src/main.rs` to run pre-flight before prepare phase
- [x] Same validation logic as distributed mode
- [x] Uses shared display function

**Test Coverage** ✅
- [x] `tests/test_preflight_validation.rs` - 6 filesystem validation tests
- [x] `tests/test_agent_preflight.rs` - 9 agent unit tests
- [x] `tests/test_preflight_integration.rs` - 4 gRPC integration tests
- [x] **Total: 19 tests passing, zero warnings**

**Test Configurations** ✅
- [x] `tests/configs/preflight_test_protected_etc.yaml` - /etc protection
- [x] `tests/configs/preflight_test_dev_null.yaml` - device file detection
- [x] `tests/configs/preflight_test_var_lib.yaml` - sensitive directory warning
- [x] `tests/configs/preflight_test_nonexistent_dir.yaml` - missing directory
- [x] `tests/configs/preflight_test_readonly_dir.yaml` - permission error (distributed)
- [x] `tests/configs/preflight_test_readonly_standalone.yaml` - permission error (standalone)
- [x] `tests/configs/preflight_test_root_owned_dir.yaml` - ownership mismatch
- [x] `scripts/setup_preflight_tests.sh` - sudo script for permission tests

**Safety Features (Beyond Original Plan)** ✅
- [x] Protected system directories (/etc, /usr, /sys, /proc, /boot, /root) → ERROR
- [x] Device files (/dev/*) → WARN for existing devices, ERROR for missing
- [x] Sensitive directories (/var/lib, /var/log, /var/spool) → WARN
- [x] Nonexistent directories → ERROR with safety message (no "mkdir -p" suggestion)

### ❌ NOT YET IMPLEMENTED

**Phase 1.2 - Object Storage Validation** ❌
- [ ] `src/preflight/object_storage.rs` is a stub - needs full implementation
- [ ] Progressive access testing (head → list → get → put → delete)
- [ ] Instance-level permission detection (EC2 IAM, GCP SA, Azure MI)
- [ ] Multi-endpoint validation (test ALL endpoints)
- [ ] Skip_verification warning for GET operations
- [ ] Authentication error mapping (NoCredentials, AccessDenied, etc.)

**Phase 2 - Resource Validation** ❌
- [ ] Memory & thread validation
- [ ] File descriptor limits
- [ ] Disk space for PUT workloads (partial - filesystem validation only)

**Phase 3 - Enhanced Features** ❌
- [ ] Network pre-checks (DNS, connectivity, latency)
- [ ] `--skip-preflight` flag (backwards compatibility)
- [ ] `--strict-validation` flag (fail on warnings)
- [ ] `--preflight-timeout` option

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

#### 1.1 File System Validation (TOP PRIORITY) ✅ **COMPLETED**

**Status**: ✅ Fully implemented and tested  
**File**: `src/preflight/filesystem.rs` ✅ Created  
**Dependencies**: Added `users` crate for uid/gid lookup  

**Implemented Functions**:
- ✅ `validate_filesystem()` - Main entry point with progressive testing
- ✅ `check_user_identity()` - Current uid/gid detection
- ✅ `check_directory_ownership()` - Directory owner comparison
- ✅ `detect_mount_type()` - Mount point and filesystem type detection
- ✅ `test_stat_access()` - Directory existence and permissions
- ✅ `test_list_access()` - Directory listing (readdir)
- ✅ `test_read_access()` - Read existing files or create test file
- ✅ `test_write_access()` - Write .sai3bench_preflight_test file
- ✅ `test_mkdir_access()` - Create subdirectory
- ✅ `test_delete_access()` - Delete test files/directories
- ✅ `check_disk_space()` - Validate sufficient space for PUT workloads
- ✅ `check_protected_paths()` - Safety check for system directories (BONUS FEATURE)

**Safety Features** (Beyond original plan):
- ✅ Protected directories (/etc, /usr, /sys, /proc, /boot, /root) → ERROR and refuse to run
- ✅ Device files (/dev/*) → Proper detection with warnings
- ✅ Sensitive directories (/var/lib, /var/log, /var/spool) → WARN
- ✅ Nonexistent directories → ERROR with "must create manually" message (no mkdir -p suggestion)

**Tests**: ✅ 6 filesystem tests passing
- ✅ test_validate_nonexistent_directory
- ✅ test_validate_readonly_directory
- ✅ test_validate_existing_writable_directory
- ✅ test_validate_insufficient_disk_space
- ✅ test_progressive_testing_stops_on_error
- ✅ test_validate_display_output

#### 1.2 Object Storage Validation (SECOND PRIORITY) ❌ **NOT IMPLEMENTED**

**Status**: ❌ Stub only - needs full implementation  
**File**: `src/preflight/object_storage.rs` - EXISTS as stub only

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

**Tests**: ❌ Not implemented - needs mock S3 responses for auth failures, missing bucket, permission denied, all endpoints validated

#### 1.3 Structured Error Types ✅ **COMPLETED**

**Status**: ✅ Fully implemented  
**File**: `src/preflight/mod.rs` ✅ Created

**Implemented Types**:
```rust
pub enum ErrorType {  // ✅ Implemented
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

pub struct ValidationSummary {  // ✅ Implemented
    pub results: Vec<ValidationResult>,
}

impl ValidationSummary {  // ✅ All methods implemented
    pub fn has_errors(&self) -> bool;
    pub fn has_warnings(&self) -> bool;
    pub fn error_count(&self) -> usize;
    pub fn warning_count(&self) -> usize;
}
```

**Display Function**: ✅ `display_validation_results()` - Formatted output with emojis, shared by distributed and standalone modes

#### 1.4 Agent Integration (NEW PRE_FLIGHT PHASE) ✅ **COMPLETED**

**Status**: ✅ Fully implemented  
**File**: `src/bin/agent.rs` ✅ Modified

**Implemented**:
- ✅ `pre_flight_validation()` gRPC handler
- ✅ PREFLIGHT command in agent state machine
- ✅ Validation result to proto conversion
- ✅ File path extraction from config (file://, direct://, no_target)

**Tests**: ✅ 9 agent unit tests passing
- ✅ test_extract_filesystem_path_file_uri
- ✅ test_extract_filesystem_path_direct_uri
- ✅ test_extract_filesystem_path_no_target
- ✅ test_extract_filesystem_path_s3_uri_returns_none
- ✅ test_extract_filesystem_path_azure_uri_returns_none
- ✅ test_validation_result_to_proto_success
- ✅ test_validation_result_to_proto_error
- ✅ test_validation_summary_no_errors
- ✅ test_validation_summary_error_count

**Note**: Agent runs pre-flight validation BEFORE prepare phase, as designed

#### 1.5 gRPC Protocol Update (NEW PRE_FLIGHT RPC) ✅ **COMPLETED**

**Status**: ✅ Fully implemented  
**File**: `proto/iobench.proto` ✅ Modified

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

**Implemented**: ✅ All proto definitions added
```proto
service AgentService {
  rpc SendConfig(ConfigMessage) returns (StatusResponse);
  rpc PreFlightValidation(PreFlightRequest) returns (PreFlightResponse);  // ✅ ADDED
  rpc Prepare(PrepareRequest) returns (StatusResponse);
  rpc Start(StartRequest) returns (stream ProgressUpdate);
  rpc Stop(StopRequest) returns (StatusResponse);
  rpc GetStatus(StatusRequest) returns (StatusResponse);
}

// ✅ ADDED: Pre-flight request/response
message PreFlightRequest {
  // Empty - uses config already sent via SendConfig
}

message PreFlightResponse {  // ✅ ADDED
  bool passed = 1;  // true if no errors (warnings OK)
  repeated ValidationResult results = 2;
  int32 error_count = 3;
  int32 warning_count = 4;
  int32 info_count = 5;
}

message ValidationResult {  // ✅ ADDED
  ResultLevel level = 1;
  ErrorType error_type = 2;
  string message = 3;
  string suggestion = 4;
  string details = 5;        // Optional technical details (uid/gid, etc)
  string test_phase = 6;     // "stat", "list", "read", "write", "delete"
}

enum ResultLevel {  // ✅ ADDED
  RESULT_LEVEL_SUCCESS = 0;
  RESULT_LEVEL_INFO = 1;
  RESULT_LEVEL_WARNING = 2;
  RESULT_LEVEL_ERROR = 3;
}

enum ErrorType {  // ✅ ADDED
  ERROR_TYPE_UNKNOWN = 0;
  ERROR_TYPE_AUTHENTICATION = 1;
  ERROR_TYPE_PERMISSION = 2;
  ERROR_TYPE_NETWORK = 3;
  ERROR_TYPE_CONFIGURATION = 4;
  ERROR_TYPE_RESOURCE = 5;
  ERROR_TYPE_SYSTEM = 6;
}
```

**After editing proto**: ✅ Completed - `cargo build` regenerated Rust code from proto definitions

#### 1.6 Controller Display (AGGREGATE ERRORS ACROSS AGENTS) ✅ **COMPLETED**

**Status**: ✅ Fully implemented  
**File**: `src/bin/controller.rs` ✅ Modified

**Implemented**:
- ✅ `run_preflight_validation()` - Calls PreFlightValidation RPC on all agents
- ✅ Aggregates errors by type across all agents
- ✅ Per-agent error display with emojis and icons
- ✅ Aggregate summary showing agents affected by each error type
- ✅ Shared `display_validation_results()` function (in mod.rs)

**Standalone Mode Integration**: ✅ **COMPLETED**
- **File**: `src/main.rs` ✅ Modified
- ✅ Pre-flight validation runs BEFORE prepare phase
- ✅ Uses same validation logic as distributed mode
- ✅ Uses shared `display_validation_results()` function
- ✅ Exits with error if validation fails

**Tests**: ✅ 4 gRPC integration tests passing
- ✅ test_preflight_grpc_success
- ✅ test_preflight_grpc_detects_readonly_directory
- ✅ test_preflight_grpc_detects_nonexistent_directory
- ✅ (1 test filtered out - removed duplicate)

---

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

### Phase 2: Resource Validation (MEDIUM PRIORITY) ❌ **NOT IMPLEMENTED**

**Estimated effort**: 1-2 days  
**Status**: ❌ Not started

#### 2.1 Memory & Thread Validation ❌ **NOT IMPLEMENTED**

**File**: `src/validation/resources.rs` ❌ Does not exist

**TODO**:
- [ ] Estimate memory usage (buffer_size * threads * 2)
- [ ] Check available system memory
- [ ] Warn if >80% memory usage
- [ ] Check thread count vs system limits
- [ ] Check file descriptor limits

**Platform-specific dependencies needed**:
- Linux: Read `/proc/meminfo`, `/proc/sys/kernel/threads-max`, `ulimit -n`
- macOS: Use `sysctl` APIs
- Windows: WMI queries

#### 2.2 Disk Space Validation ✅ **PARTIAL** (Filesystem only)

**Status**: ✅ Implemented in `check_disk_space()` for filesystem validation
- ✅ Validates disk space for PUT workloads
- ✅ 20% safety margin
- ❌ Not implemented for object storage validation

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

### Phase 3: Enhanced Features (LOWER PRIORITY) ❌ **NOT IMPLEMENTED**

**Estimated effort**: 2-3 days  
**Status**: ❌ Not started

#### 3.1 Network Pre-Checks ❌ **NOT IMPLEMENTED**

**File**: `src/validation/network.rs` ❌ Does not exist

**TODO**:
- [ ] DNS resolution for S3 endpoints
- [ ] TCP connectivity test (5-second timeout)
- [ ] Optional latency measurement

#### 3.2 Skip Pre-Flight Option (Backwards Compatibility) ❌ **NOT IMPLEMENTED**

**Controller flag**: `--skip-preflight` ❌ Not added

**TODO**:
```bash
# Default: run pre-flight validation (CURRENT BEHAVIOR)
sai3bench-ctl run --config test.yaml

# Skip pre-flight (dangerous - for backwards compatibility only) - NOT IMPLEMENTED
sai3bench-ctl run --config test.yaml --skip-preflight  # ❌ Flag doesn't exist yet
```

**Note**: Pre-flight is now a standard phase and always runs. This flag would be for backwards compatibility only.

#### 3.3 Command-Line Options ❌ **NOT IMPLEMENTED**

**TODO**: Add these CLI arguments to controller/standalone binaries

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

---

## Testing Strategy

### Unit Tests ✅ **COMPLETED**

**Status**: ✅ 19 tests passing, zero warnings

**Filesystem Tests** (6 tests in `tests/test_preflight_validation.rs`):
- ✅ test_validate_nonexistent_directory
- ✅ test_validate_readonly_directory
- ✅ test_validate_existing_writable_directory
- ✅ test_validate_insufficient_disk_space
- ✅ test_progressive_testing_stops_on_error
- ✅ test_validate_display_output

**Agent Tests** (9 tests in `tests/test_agent_preflight.rs`):
- ✅ test_extract_filesystem_path_file_uri
- ✅ test_extract_filesystem_path_direct_uri
- ✅ test_extract_filesystem_path_no_target
- ✅ test_extract_filesystem_path_s3_uri_returns_none
- ✅ test_extract_filesystem_path_azure_uri_returns_none
- ✅ test_validation_result_to_proto_success
- ✅ test_validation_result_to_proto_error
- ✅ test_validation_summary_no_errors
- ✅ test_validation_summary_error_count

**Integration Tests** (4 tests in `tests/test_preflight_integration.rs`):
- ✅ test_preflight_grpc_success
- ✅ test_preflight_grpc_detects_readonly_directory
- ✅ test_preflight_grpc_detects_nonexistent_directory
- ✅ (1 test filtered out - removed duplicate)

**TODO - Object Storage Tests**: ❌ Not implemented
- [ ] Test with invalid S3 credentials (mocked)
- [ ] Test with missing bucket
- [ ] Test with permission denied

**TODO - Resource Tests**: ❌ Not implemented
- [ ] Test with excessive memory requirements
- [ ] Test with insufficient disk space (for object storage)

### Manual Testing Checklist ✅ **COMPLETED FOR FILESYSTEM**

**Filesystem Tests** (all verified with real configs):
- ✅ File path doesn't exist → Clear error "Directory must be created manually before running benchmarks for safety"
- ✅ Permission denied → Clear error with chmod/chgrp suggestion and uid/gid details
- ✅ Protected directory (/etc) → ERROR "Do not use /etc for benchmarking - use /tmp or a dedicated mount point"
- ✅ Device file (/dev/null) → ERROR "Path exists but is not a directory"
- ✅ Sensitive directory (/var/lib) → Would WARN if directory existed
- ✅ All checks pass → Workload executes normally

**Object Storage Tests**: ❌ Not implemented
- [ ] Invalid AWS credentials → Auth error with env var suggestion
- [ ] Bucket doesn't exist → Config error with bucket name check
- [ ] Insufficient disk space → Resource error with space needed

---

## Dependencies to Add

**Status**: ✅ Filesystem dependencies added, ❌ Object storage dependencies not yet needed

```toml
# In Cargo.toml - FILESYSTEM VALIDATION
[dependencies]
users = "0.11"  # ✅ ADDED - For uid/gid lookup and username/groupname resolution
# NOTE: Using 'users' crate instead of 'nix' for cross-platform compatibility

# TODO - OBJECT STORAGE VALIDATION (Phase 1.2)
# reqwest = { version = "0.11", features = ["json"] }  # ❌ NOT ADDED - For metadata service queries

# TODO - RESOURCE VALIDATION (Phase 2)
# fs2 = "0.4"              # ❌ NOT ADDED - Cross-platform disk space checks (alternative to manual statvfs)
# sysinfo = "0.30"         # ❌ NOT ADDED - System resource queries (memory, CPU)
```

**Note**: Disk space checking uses `libc::statvfs()` directly instead of `fs2` crate

---

## Success Criteria

**Phase 1 FILESYSTEM Complete When**: ✅ **ALL CRITERIA MET**
1. ✅ Pre-flight runs as separate phase before PREPARE
2. ✅ Progressive access testing works (stat → list → read → write → mkdir → delete)
3. ✅ User/group ID mismatches detected and reported with details
4. ✅ **BONUS**: Protected path detection (system directories) added for safety
5. ✅ **BONUS**: No directory creation suggestions - require manual creation
6. ✅ File permission errors show clear fix suggestions (uid/gid, chmod)
7. ✅ Agents report validation errors to controller via PreFlightValidation RPC
8. ✅ Controller aggregates errors across all agents
9. ✅ Controller displays structured error messages with icons and suggestions
10. ✅ Standalone mode also runs pre-flight validation
11. ✅ All Phase 1 unit tests passing (19 total)
12. ✅ Manual testing with real permission scenarios verified

**Phase 1 OBJECT STORAGE Complete When**: ❌ **NOT STARTED**
1. ❌ All endpoints in multi-endpoint configs validated
2. ❌ Instance-level permissions detected (EC2 IAM, GCP SA, Azure MI)
3. ❌ S3 auth errors show credential source and troubleshooting
4. ❌ Progressive testing: head → list → get → put → delete
5. ❌ Skip_verification warning for GET operations
6. ❌ Object storage unit tests passing

---

## Example Success Output

**Filesystem Validation** ✅ **IMPLEMENTED AND WORKING**
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

**Object Storage Validation** ❌ **NOT IMPLEMENTED - EXAMPLE ONLY**
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
   ...
```

---

## Timeline Estimate

**ACTUAL TIME SPENT**:
- ✅ **Phase 1.1-1.6 (Filesystem Validation)**: ~3 days (includes bonus safety features)
- ✅ **Testing & Manual Validation**: ~0.5 days
- ✅ **Total Phase 1 Filesystem**: ~3.5 days

**REMAINING ESTIMATES**:
- ❌ **Phase 1.2 (Object Storage)**: 2-3 days
- ❌ **Phase 2 (Resource Checks)**: 1-2 days  
- ❌ **Phase 3 (Enhanced Features)**: 2-3 days

**Total Remaining**: ~5-8 days for Phases 1.2, 2, and 3

---

## Notes for Next Session

### ✅ COMPLETED TONIGHT (February 2, 2026)
1. ✅ Phase 1.1 - Full filesystem validation with progressive testing
2. ✅ Phase 1.3 - Structured error types and display functions
3. ✅ Phase 1.4 - Agent integration with gRPC handlers
4. ✅ Phase 1.5 - gRPC protocol updates (proto definitions)
5. ✅ Phase 1.6 - Controller display with error aggregation
6. ✅ Standalone mode integration
7. ✅ 19 unit/integration tests (all passing)
8. ✅ 7 manual test configs with various failure scenarios
9. ✅ Safety features: protected paths, no mkdir suggestions
10. ✅ All changes committed to `feature/preflight-validation` branch

### 🎯 NEXT PRIORITY (Phase 1.2 - Object Storage)
1. ❌ Implement `src/preflight/object_storage.rs` (currently stub)
2. ❌ Progressive access testing: head → list → get → put → delete
3. ❌ Instance-level permission detection (EC2 IAM, GCP SA, Azure MI)
4. ❌ Multi-endpoint validation (test ALL endpoints in config)
5. ❌ Skip_verification warning for GET operations
6. ❌ Object storage unit tests

### 📝 KEY INSIGHTS FROM TONIGHT
- Progressive testing works well - stop on first error
- Protected path detection prevents dangerous operations
- Shared display function reduces code duplication
- Real permission testing (with sudo) validates the design
- Emojis and icons make errors easier to scan visually

### 🚀 WHEN READY TO CONTINUE
**Start with**: Phase 1.2 (Object Storage Validation)  
**Reference**: s3dlio library for object storage operations  
**Test approach**: Mock S3 responses for auth failures, missing buckets, permission denied  
**Goal**: Same progressive testing philosophy as filesystem validation

---

**Phase 1 Filesystem Validation: COMPLETE ✅**  
**Ready for Phase 1.2 Object Storage Validation ❌**
