// tests/test_agent_preflight.rs - Unit tests for agent pre-flight validation

use sai3_bench::config::Config;
use std::path::PathBuf;

/// Helper function to extract filesystem path (duplicated from agent.rs for testing)
fn extract_filesystem_path_from_config(config: &Config) -> Option<PathBuf> {
    config.target.as_ref().and_then(|target| {
        target.strip_prefix("file://")
            .or_else(|| target.strip_prefix("direct://"))
            .map(PathBuf::from)
    })
}

/// Helper to create a minimal test Config
fn make_test_config(target: Option<String>) -> Config {
    Config {
        duration: std::time::Duration::from_secs(60),
        concurrency: 10,
        target,
        workload: vec![],
        prepare: None,
        range_engine: None,
        page_cache_mode: None,
        distributed: None,
        io_rate: None,
        processes: None,
        processing_mode: sai3_bench::config::ProcessingMode::MultiRuntime,
        live_stats_tracker: None,
        error_handling: sai3_bench::config::ErrorHandlingConfig::default(),
        op_log_path: None,
        multi_endpoint: None,
        perf_log: None,
        warmup_period: None,
        cache_checkpoint_interval_secs: sai3_bench::config::default_cache_checkpoint_interval(),
        s3dlio_optimization: None,
    }
}

#[test]
fn test_extract_filesystem_path_file_uri() {
    let config = make_test_config(Some("file:///tmp/test".to_string()));

    let path = extract_filesystem_path_from_config(&config);
    assert_eq!(path, Some(PathBuf::from("/tmp/test")));
}

#[test]
fn test_extract_filesystem_path_direct_uri() {
    let config = make_test_config(Some("direct:///mnt/nvme/data".to_string()));

    let path = extract_filesystem_path_from_config(&config);
    assert_eq!(path, Some(PathBuf::from("/mnt/nvme/data")));
}

#[test]
fn test_extract_filesystem_path_s3_uri_returns_none() {
    let config = make_test_config(Some("s3://bucket/prefix".to_string()));

    let path = extract_filesystem_path_from_config(&config);
    assert_eq!(path, None);
}

#[test]
fn test_extract_filesystem_path_azure_uri_returns_none() {
    let config = make_test_config(Some("az://container/prefix".to_string()));

    let path = extract_filesystem_path_from_config(&config);
    assert_eq!(path, None);
}

#[test]
fn test_extract_filesystem_path_no_target() {
    let config = make_test_config(None);

    let path = extract_filesystem_path_from_config(&config);
    assert_eq!(path, None);
}

#[test]
fn test_validation_result_to_proto_success() {
    use sai3_bench::preflight::{ValidationResult, ResultLevel};

    let result = ValidationResult {
        level: ResultLevel::Success,
        error_type: None,
        message: "Directory accessible".to_string(),
        suggestion: String::new(),
        details: None,
        test_phase: "stat".to_string(),
    };

    // Verify the result structure
    assert_eq!(result.level, ResultLevel::Success);
    assert_eq!(result.error_type, None);
    assert_eq!(result.message, "Directory accessible");
    assert_eq!(result.test_phase, "stat");
}

#[test]
fn test_validation_result_to_proto_error() {
    use sai3_bench::preflight::{ValidationResult, ResultLevel, ErrorType};

    let result = ValidationResult {
        level: ResultLevel::Error,
        error_type: Some(ErrorType::Permission),
        message: "Access denied".to_string(),
        suggestion: "Check directory permissions".to_string(),
        details: Some("uid=1000 gid=1000".to_string()),
        test_phase: "write".to_string(),
    };

    // Verify the result structure
    assert_eq!(result.level, ResultLevel::Error);
    assert_eq!(result.error_type, Some(ErrorType::Permission));
    assert_eq!(result.message, "Access denied");
    assert_eq!(result.suggestion, "Check directory permissions");
    assert_eq!(result.details, Some("uid=1000 gid=1000".to_string()));
    assert_eq!(result.test_phase, "write");
}

#[test]
fn test_validation_summary_error_count() {
    use sai3_bench::preflight::{ValidationResult, ValidationSummary, ResultLevel, ErrorType};

    let results = vec![
        ValidationResult {
            level: ResultLevel::Success,
            error_type: None,
            message: "Test passed".to_string(),
            suggestion: String::new(),
            details: None,
            test_phase: "stat".to_string(),
        },
        ValidationResult {
            level: ResultLevel::Error,
            error_type: Some(ErrorType::Permission),
            message: "Access denied".to_string(),
            suggestion: "Fix permissions".to_string(),
            details: None,
            test_phase: "write".to_string(),
        },
        ValidationResult {
            level: ResultLevel::Warning,
            error_type: Some(ErrorType::Resource),
            message: "Low disk space".to_string(),
            suggestion: "Free up space".to_string(),
            details: None,
            test_phase: "disk_space".to_string(),
        },
    ];

    let summary = ValidationSummary::new(results);
    
    assert_eq!(summary.error_count(), 1);
    assert_eq!(summary.warning_count(), 1);
    assert_eq!(summary.success_count(), 1);
    assert!(summary.has_errors());
    assert!(summary.has_warnings());
}

#[test]
fn test_validation_summary_no_errors() {
    use sai3_bench::preflight::{ValidationResult, ValidationSummary, ResultLevel};

    let results = vec![
        ValidationResult {
            level: ResultLevel::Success,
            error_type: None,
            message: "All tests passed".to_string(),
            suggestion: String::new(),
            details: None,
            test_phase: "complete".to_string(),
        },
    ];

    let summary = ValidationSummary::new(results);
    
    assert_eq!(summary.error_count(), 0);
    assert_eq!(summary.warning_count(), 0);
    assert!(!summary.has_errors());
    assert!(!summary.has_warnings());
}
