//! Object storage validation with quick bucket accessibility checks
//!
//! Performs a minimal sanity check using a streaming list (max 1 result) to
//! confirm bucket reachability, authentication, and read permission — without
//! enumerating all objects even for buckets with billions of keys.
//!
//! Also checks:
//! - Permission source detection (env vars vs instance-level IAM)
//! - EC2 IAM role, GCP service account, Azure managed identity

use super::{ErrorType, ValidationResult, ValidationSummary};
use anyhow::Result;
use futures::StreamExt;
use s3dlio::object_store::store_for_uri;

/// Validate object storage access with a quick bucket accessibility check.
///
/// Performs a streaming list (reads at most 1 key) against each endpoint to
/// verify connectivity and credentials without loading all objects into memory.
///
/// # Arguments
/// * `endpoints` - List of object storage URIs to validate (e.g. `gs://bucket/prefix`)
/// * `_check_write` - Reserved for future write-permission testing; currently unused
pub async fn validate_object_storage(
    endpoints: &[String],
    _check_write: bool,
) -> Result<ValidationSummary> {
    let mut results = Vec::new();

    // Detect permission source (env vars vs instance-level)
    results.push(detect_permission_source().await?);

    for (idx, endpoint) in endpoints.iter().enumerate() {
        tracing::info!(
            "Validating object storage endpoint {}/{}: {}",
            idx + 1,
            endpoints.len(),
            endpoint
        );

        // Build an ObjectStore for this URI
        let store = match store_for_uri(endpoint) {
            Ok(s) => s,
            Err(e) => {
                results.push(ValidationResult::error(
                    ErrorType::Configuration,
                    "init",
                    format!("Cannot initialize storage for {}: {}", endpoint, e),
                    "Check URI format: s3://bucket/prefix  gs://bucket/prefix  az://account/container/prefix",
                ));
                continue;
            }
        };

        // Stream at most 1 key — fast even for buckets with billions of objects.
        // A network/auth error manifests as Some(Err(…)); an empty bucket as None.
        let mut stream = store.list_stream(endpoint, false);
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.next(),
        ).await {
            Err(_elapsed) => {
                results.push(ValidationResult::error(
                    ErrorType::Network,
                    "list",
                    format!("Timed out connecting to {}", endpoint),
                    "Check network connectivity, DNS resolution, and storage endpoint configuration",
                ));
            }
            Ok(Some(Err(e))) => {
                let err_str = e.to_string().to_lowercase();
                let (error_type, suggestion) = if err_str.contains("403")
                    || err_str.contains("forbidden")
                    || err_str.contains("accessdenied")
                    || err_str.contains("access denied")
                {
                    (ErrorType::Permission,
                     "Check that the service account/IAM role has storage.objects.list permission")
                } else if err_str.contains("401")
                    || err_str.contains("unauthorized")
                    || err_str.contains("unauthenticated")
                {
                    (ErrorType::Authentication,
                     "Credentials invalid or expired; check GOOGLE_APPLICATION_CREDENTIALS or IAM role")
                } else if err_str.contains("404")
                    || err_str.contains("nosuchbucket")
                    || err_str.contains("no such bucket")
                    || err_str.contains("not found")
                {
                    (ErrorType::Configuration,
                     "Bucket does not exist or the endpoint path is incorrect")
                } else {
                    (ErrorType::Network,
                     "Check network connectivity and storage backend availability")
                };
                results.push(ValidationResult::error(
                    error_type,
                    "list",
                    format!("Failed to list {}: {}", endpoint, e),
                    suggestion,
                ));
            }
            Ok(None) => {
                // Bucket is accessible but empty at this prefix
                results.push(ValidationResult::success(
                    "list",
                    format!("Bucket accessible (no objects at prefix): {}", endpoint),
                ));
            }
            Ok(Some(Ok(_first_key))) => {
                results.push(ValidationResult::success(
                    "list",
                    format!("Bucket accessible: {}", endpoint),
                ));
            }
        }
    }

    Ok(ValidationSummary::new(results))
}

/// Detect permission source (environment variables vs instance-level)
async fn detect_permission_source() -> Result<ValidationResult> {
    // Check AWS EC2 instance IAM role
    if let Ok(role) = check_ec2_iam_role().await {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            format!("Using EC2 IAM role: {}", role),
            "Instance-level permissions detected",
        ));
    }

    // Check GCP service account
    if let Ok(sa) = check_gcp_service_account().await {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            format!("Using GCP service account: {}", sa),
            "Instance-level permissions detected",
        ));
    }

    // Check Azure managed identity
    if let Ok(mi) = check_azure_managed_identity().await {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            format!("Using Azure managed identity: {}", mi),
            "Instance-level permissions detected",
        ));
    }

    // Check environment variables
    if std::env::var("AWS_ACCESS_KEY_ID").is_ok() {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            "Using AWS credentials from environment variables".to_string(),
            "Credentials: AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY",
        ));
    }

    if std::env::var("GOOGLE_APPLICATION_CREDENTIALS").is_ok() {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            "Using GCP credentials from GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            "Service account key file detected",
        ));
    }

    if std::env::var("AZURE_STORAGE_ACCOUNT").is_ok() {
        return Ok(ValidationResult::info(
            ErrorType::Authentication,
            "credentials",
            "Using Azure credentials from environment variables".to_string(),
            "AZURE_STORAGE_ACCOUNT detected",
        ));
    }

    // No credentials detected
    Ok(ValidationResult::warning(
        ErrorType::Authentication,
        "credentials",
        "No credentials detected (env vars or instance-level)".to_string(),
        "Set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY or use instance IAM role",
    ))
}

/// Check for EC2 IAM role via metadata service
async fn check_ec2_iam_role() -> Result<String> {
    // Query EC2 instance metadata service
    // http://169.254.169.254/latest/meta-data/iam/security-credentials/
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let response = client
        .get("http://169.254.169.254/latest/meta-data/iam/security-credentials/")
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("EC2 metadata returned HTTP {}", response.status());
    }

    let role_name = response.text().await?;
    if role_name.is_empty() {
        anyhow::bail!("No IAM role attached to instance");
    }

    Ok(role_name.trim().to_string())
}

/// Check for GCP service account via metadata service
async fn check_gcp_service_account() -> Result<String> {
    // Query GCP metadata server
    // http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let resp = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/email")
        .header("Metadata-Flavor", "Google")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("GCP metadata returned HTTP {}", resp.status());
    }

    let email = resp.text().await?;
    if email.is_empty() {
        anyhow::bail!("No service account attached to instance");
    }

    Ok(email.trim().to_string())
}

/// Check for Azure managed identity via IMDS endpoint
async fn check_azure_managed_identity() -> Result<String> {
    // Query Azure Instance Metadata Service
    // http://169.254.169.254/metadata/identity/oauth2/token
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let response = client
        .get("http://169.254.169.254/metadata/identity/info")
        .header("Metadata", "true")
        .query(&[("api-version", "2021-02-01")])
        .send()
        .await?;

    if response.status().is_success() {
        Ok("Managed Identity detected".to_string())
    } else {
        anyhow::bail!("No managed identity attached to instance");
    }
}


