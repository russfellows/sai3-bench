// tests/test_preflight_integration.rs - Integration tests for pre-flight validation
//
// These tests verify end-to-end gRPC communication between controller and agent,
// including actual filesystem permission detection and error reporting.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

// Import proto types
pub mod pb {
    pub mod iobench {
        include!("../src/pb/iobench.rs");
    }
}

use pb::iobench::{
    agent_server::{Agent, AgentServer},
    Empty, LiveStats, OpSummary, PingReply, PreFlightRequest, PreFlightResponse,
    RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary,
    ControlMessage, ValidationResult, ResultLevel, ErrorType,
};
use tokio_stream::wrappers::ReceiverStream;

/// Mock agent that implements the Agent trait for testing
struct MockAgent {
    /// Optional error to inject during pre-flight
    preflight_error: Arc<Mutex<Option<String>>>,
    /// Path to test directory (may not exist or have wrong permissions)
    test_path: Arc<Mutex<Option<PathBuf>>>,
}

impl MockAgent {
    fn with_test_path(path: PathBuf) -> Self {
        Self {
            preflight_error: Arc::new(Mutex::new(None)),
            test_path: Arc::new(Mutex::new(Some(path))),
        }
    }
}

#[tonic::async_trait]
impl Agent for MockAgent {
    type RunWorkloadWithLiveStatsStream = ReceiverStream<Result<LiveStats, tonic::Status>>;
    type ExecuteWorkloadStream = ReceiverStream<Result<LiveStats, tonic::Status>>;

    async fn ping(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<PingReply>, tonic::Status> {
        Ok(tonic::Response::new(PingReply {
            version: "test".to_string(),
        }))
    }

    async fn pre_flight_validation(
        &self,
        request: tonic::Request<PreFlightRequest>,
    ) -> Result<tonic::Response<PreFlightResponse>, tonic::Status> {
        let _req = request.into_inner();
        
        // Check for injected error
        if let Some(err_msg) = self.preflight_error.lock().await.as_ref() {
            let response = PreFlightResponse {
                passed: false,
                results: vec![ValidationResult {
                    level: ResultLevel::Error as i32,
                    error_type: ErrorType::System as i32,
                    message: err_msg.clone(),
                    suggestion: "Fix the injected error".to_string(),
                    details: "Test error injection".to_string(),
                    test_phase: "mock".to_string(),
                }],
                error_count: 1,
                warning_count: 0,
                info_count: 0,
            };
            return Ok(tonic::Response::new(response));
        }

        // Parse config and run real validation if test_path is set
        if let Some(test_path) = self.test_path.lock().await.as_ref() {
            // Run actual filesystem validation
            match sai3_bench::preflight::filesystem::validate_filesystem(
                test_path,
                true,  // check_write
                Some(1024 * 1024),  // 1 MB required
            ).await {
                Ok(summary) => {
                    let results: Vec<ValidationResult> = summary.results.iter().map(|r| {
                        ValidationResult {
                            level: match r.level {
                                sai3_bench::preflight::ResultLevel::Success => ResultLevel::Success as i32,
                                sai3_bench::preflight::ResultLevel::Info => ResultLevel::Info as i32,
                                sai3_bench::preflight::ResultLevel::Warning => ResultLevel::Warning as i32,
                                sai3_bench::preflight::ResultLevel::Error => ResultLevel::Error as i32,
                            },
                            error_type: match r.error_type {
                                Some(sai3_bench::preflight::ErrorType::Authentication) => ErrorType::Authentication as i32,
                                Some(sai3_bench::preflight::ErrorType::Permission) => ErrorType::Permission as i32,
                                Some(sai3_bench::preflight::ErrorType::Network) => ErrorType::Network as i32,
                                Some(sai3_bench::preflight::ErrorType::Configuration) => ErrorType::Configuration as i32,
                                Some(sai3_bench::preflight::ErrorType::Resource) => ErrorType::Resource as i32,
                                Some(sai3_bench::preflight::ErrorType::System) => ErrorType::System as i32,
                                None => ErrorType::Unknown as i32,
                            },
                            message: r.message.clone(),
                            suggestion: r.suggestion.clone(),
                            details: r.details.clone().unwrap_or_default(),
                            test_phase: r.test_phase.clone(),
                        }
                    }).collect();

                    let passed = !summary.has_errors();
                    Ok(tonic::Response::new(PreFlightResponse {
                        passed,
                        results,
                        error_count: summary.error_count() as i32,
                        warning_count: summary.warning_count() as i32,
                        info_count: summary.info_count() as i32,
                    }))
                }
                Err(e) => {
                    Err(tonic::Status::internal(format!("Validation error: {}", e)))
                }
            }
        } else {
            // No test path - return success
            Ok(tonic::Response::new(PreFlightResponse {
                passed: true,
                results: vec![ValidationResult {
                    level: ResultLevel::Success as i32,
                    error_type: ErrorType::Unknown as i32,
                    message: "Mock validation passed".to_string(),
                    suggestion: String::new(),
                    details: String::new(),
                    test_phase: "mock".to_string(),
                }],
                error_count: 0,
                warning_count: 0,
                info_count: 0,
            }))
        }
    }

    // Stub implementations for other required methods
    async fn run_get(
        &self,
        _request: tonic::Request<RunGetRequest>,
    ) -> Result<tonic::Response<OpSummary>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }

    async fn run_put(
        &self,
        _request: tonic::Request<RunPutRequest>,
    ) -> Result<tonic::Response<OpSummary>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }

    async fn run_workload(
        &self,
        _request: tonic::Request<RunWorkloadRequest>,
    ) -> Result<tonic::Response<WorkloadSummary>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }

    async fn run_workload_with_live_stats(
        &self,
        _request: tonic::Request<RunWorkloadRequest>,
    ) -> Result<tonic::Response<Self::RunWorkloadWithLiveStatsStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }

    async fn abort_workload(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<Empty>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }

    async fn execute_workload(
        &self,
        _request: tonic::Request<tonic::Streaming<ControlMessage>>,
    ) -> Result<tonic::Response<Self::ExecuteWorkloadStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented in test"))
    }
}

#[tokio::test]
async fn test_preflight_grpc_success() -> Result<()> {
    // Create temp directory with proper permissions
    let temp_dir = TempDir::new()?;
    let test_path = temp_dir.path().to_path_buf();

    // Start mock agent server
    let agent = MockAgent::with_test_path(test_path.clone());
    let addr = "127.0.0.1:50051".parse().unwrap();
    
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServer::new(agent))
            .serve(addr)
            .await
            .unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Connect client
    let mut client = pb::iobench::agent_client::AgentClient::connect("http://127.0.0.1:50051")
        .await
        .context("Failed to connect to test agent")?;

    // Send pre-flight request
    let request = PreFlightRequest {
        config_yaml: "".to_string(),
        agent_id: "test-agent".to_string(),
    };

    let response = client.pre_flight_validation(request).await?;
    let result = response.into_inner();

    // Verify response
    assert!(result.passed, "Pre-flight should pass for writable directory");
    assert_eq!(result.error_count, 0);
    assert!(result.results.len() > 0, "Should have validation results");

    // Cleanup
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_preflight_grpc_detects_nonexistent_directory() -> Result<()> {
    // Create path to non-existent directory
    let nonexistent = PathBuf::from("/tmp/sai3bench-test-nonexistent-dir-12345");
    
    // Ensure it doesn't exist
    if nonexistent.exists() {
        std::fs::remove_dir_all(&nonexistent)?;
    }

    // Start mock agent server
    let agent = MockAgent::with_test_path(nonexistent.clone());
    let addr = "127.0.0.1:50052".parse().unwrap();
    
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServer::new(agent))
            .serve(addr)
            .await
            .unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Connect client
    let mut client = pb::iobench::agent_client::AgentClient::connect("http://127.0.0.1:50052")
        .await
        .context("Failed to connect to test agent")?;

    // Send pre-flight request
    let request = PreFlightRequest {
        config_yaml: "".to_string(),
        agent_id: "test-agent".to_string(),
    };

    let response = client.pre_flight_validation(request).await?;
    let result = response.into_inner();

    // Verify response
    assert!(!result.passed, "Pre-flight should fail for non-existent directory");
    assert!(result.error_count > 0, "Should have at least one error");
    
    // Debug: print the actual errors we got
    eprintln!("Results from validation:");
    for (i, r) in result.results.iter().enumerate() {
        eprintln!("  Result {}: level={}, error_type={}, message={}", 
            i, r.level, r.error_type, r.message);
    }
    
    // Check that we got configuration, permission, or system error
    let has_relevant_error = result.results.iter().any(|r| {
        r.level == ResultLevel::Error as i32 &&
        (r.error_type == ErrorType::Permission as i32 || 
         r.error_type == ErrorType::System as i32 ||
         r.error_type == ErrorType::Configuration as i32)
    });
    assert!(has_relevant_error, "Should have permission, system, or configuration error");

    // Cleanup
    server_handle.abort();
    Ok(())
}

#[tokio::test]
async fn test_preflight_grpc_detects_readonly_directory() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    
    // Create temp directory and make it read-only
    let temp_dir = TempDir::new()?;
    let test_path = temp_dir.path().to_path_buf();
    
    // Set read-only permissions (0555)
    let mut perms = std::fs::metadata(&test_path)?.permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&test_path, perms)?;

    // Start mock agent server
    let agent = MockAgent::with_test_path(test_path.clone());
    let addr = "127.0.0.1:50053".parse().unwrap();
    
    let server_handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(AgentServer::new(agent))
            .serve(addr)
            .await
            .unwrap();
    });

    // Give server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Connect client
    let mut client = pb::iobench::agent_client::AgentClient::connect("http://127.0.0.1:50053")
        .await
        .context("Failed to connect to test agent")?;

    // Send pre-flight request
    let request = PreFlightRequest {
        config_yaml: "".to_string(),
        agent_id: "test-agent".to_string(),
    };

    let response = client.pre_flight_validation(request).await?;
    let result = response.into_inner();

    // Verify response - should fail due to write check
    assert!(!result.passed, "Pre-flight should fail for read-only directory");
    assert!(result.error_count > 0, "Should have at least one error");
    
    // Check for permission error
    let has_permission_error = result.results.iter().any(|r| {
        r.level == ResultLevel::Error as i32 &&
        r.error_type == ErrorType::Permission as i32
    });
    assert!(has_permission_error, "Should have permission error for read-only directory");

    // Cleanup - restore permissions before cleanup
    let mut perms = std::fs::metadata(&test_path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&test_path, perms)?;
    
    server_handle.abort();
    Ok(())
}

#[test]
fn test_validation_result_aggregation() {
    // Test that we correctly aggregate errors/warnings across multiple agents
    use pb::iobench::PreFlightResponse;
    
    let responses = vec![
        PreFlightResponse {
            passed: true,
            results: vec![],
            error_count: 0,
            warning_count: 1,
            info_count: 2,
        },
        PreFlightResponse {
            passed: false,
            results: vec![],
            error_count: 2,
            warning_count: 0,
            info_count: 1,
        },
        PreFlightResponse {
            passed: true,
            results: vec![],
            error_count: 0,
            warning_count: 3,
            info_count: 0,
        },
    ];

    let total_errors: i32 = responses.iter().map(|r| r.error_count).sum();
    let total_warnings: i32 = responses.iter().map(|r| r.warning_count).sum();
    let total_info: i32 = responses.iter().map(|r| r.info_count).sum();
    let all_passed = responses.iter().all(|r| r.passed);

    assert_eq!(total_errors, 2);
    assert_eq!(total_warnings, 4);
    assert_eq!(total_info, 3);
    assert!(!all_passed);
}
