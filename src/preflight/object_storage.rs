//! Object storage validation with quick bucket accessibility checks
//!
//! Performs a minimal sanity check using a streaming list (max 1 result) to
//! confirm bucket reachability, authentication, and read permission — without
//! enumerating all objects even for buckets with billions of keys.
//!
//! Also checks:
//! - Permission source detection (env vars vs instance-level IAM)
//! - EC2 IAM role, GCP service account, Azure managed identity
//! - Optional write probe: PUT → LIST → DELETE a small test object (warnings only)

use super::{ErrorType, ValidationResult, ValidationSummary};
use anyhow::Result;
use bytes;
use futures::StreamExt;
use rand::Rng;
use s3dlio::object_store::store_for_uri;

/// Known content written during the write probe. After PUTting this, we GET it
/// back and compare byte-for-byte to confirm read-after-write consistency.
const PROBE_CONTENT: &[u8] = b"sai3bench-write-probe-v1";

/// Validate object storage access with a quick bucket accessibility check.
///
/// Performs a streaming list (reads at most 1 key) against each endpoint to
/// verify connectivity and credentials without loading all objects into memory.
///
/// When `check_write` is `true`, also runs a write probe per endpoint:
/// PUT a small known-content object → GET it back and verify content → DELETE.
/// Failures at any probe step are reported as **warnings only** — they never
/// cause the preflight to hard-fail. This is so that read-only creds on a
/// GET-only workload don't block execution.
///
/// # Arguments
/// * `endpoints` - List of object storage URIs to validate (e.g. `gs://bucket/prefix`)
/// * `check_write` - When `true`, run PUT/GET/DELETE write probe on each endpoint
pub async fn validate_object_storage(
    endpoints: &[String],
    check_write: bool,
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

        // Write probe: PUT a known-content object, GET it back, DELETE it.
        // All steps are warning-only — never a hard failure.
        if check_write {
            let probe_results = write_probe(endpoint).await;
            results.extend(probe_results);
        }
    }

    Ok(ValidationSummary::new(results))
}

/// PUT a tiny probe object, GET it back and verify its content, then DELETE it.
///
/// All three steps issue only warnings on failure — the preflight does not
/// hard-fail because a read-only credential set on a GET-only workload should
/// never prevent the run.
async fn write_probe(endpoint: &str) -> Vec<ValidationResult> {
    let mut results = Vec::new();

    // Build a unique probe URI from the endpoint so concurrent agents don't collide.
    // Uniqueness comes from a random 16-bit suffix; the content itself is a fixed
    // known string (PROBE_CONTENT) that we verify on read-back.
    let suffix: u32 = rand::rng().random();
    let sep = if endpoint.ends_with('/') { "" } else { "/" };
    let probe_uri = format!("{}{}sai3bench-preflight-probe-{:08x}.bin", endpoint, sep, suffix);

    // ── Step 1: PUT ────────────────────────────────────────────────────────────
    let store = match store_for_uri(&probe_uri) {
        Ok(s) => s,
        Err(e) => {
            results.push(ValidationResult::warning(
                ErrorType::Configuration,
                "write-probe",
                format!("Cannot initialize store for write probe at {}: {}", endpoint, e),
                "Write probe skipped — check URI format",
            ));
            return results;
        }
    };

    let put_data = bytes::Bytes::from_static(PROBE_CONTENT);
    let put_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.put(&probe_uri, put_data),
    ).await;

    match put_result {
        Err(_elapsed) => {
            results.push(ValidationResult::warning(
                ErrorType::Network,
                "write-probe-put",
                format!("Write probe PUT timed out at {}", endpoint),
                "PUT took > 15 s — check network and write permissions",
            ));
            return results;
        }
        Ok(Err(e)) => {
            results.push(ValidationResult::warning(
                ErrorType::Permission,
                "write-probe-put",
                format!("Write probe PUT failed at {}: {}", endpoint, e),
                "Workload may fail if it requires write access — check write permissions",
            ));
            return results;
        }
        Ok(Ok(())) => {
            results.push(ValidationResult::success(
                "write-probe-put",
                format!("Write probe PUT succeeded at {}", endpoint),
            ));
        }
    }

    // ── Step 2: GET and verify content ─────────────────────────────────────────
    let get_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.get(&probe_uri),
    ).await;

    match get_result {
        Err(_elapsed) => {
            results.push(ValidationResult::warning(
                ErrorType::Network,
                "write-probe-get",
                format!("Write probe GET timed out at {}", endpoint),
                "Object was written but could not be read back within 15 s",
            ));
        }
        Ok(Err(e)) => {
            results.push(ValidationResult::warning(
                ErrorType::Network,
                "write-probe-get",
                format!("Write probe GET failed at {}: {}", endpoint, e),
                "Object was written but was not readable — possible consistency issue",
            ));
        }
        Ok(Ok(data)) => {
            if data.as_ref() == PROBE_CONTENT {
                results.push(ValidationResult::success(
                    "write-probe-get",
                    format!("Write probe GET verified correct content at {}", endpoint),
                ));
            } else {
                results.push(ValidationResult::warning(
                    ErrorType::System,
                    "write-probe-get",
                    format!(
                        "Write probe GET at {} returned unexpected content (got {} bytes, expected {:?})",
                        endpoint,
                        data.len(),
                        std::str::from_utf8(PROBE_CONTENT).unwrap_or("<binary>"),
                    ),
                    "Storage may be returning stale or corrupted data",
                ));
            }
        }
    }

    // ── Step 3: DELETE ─────────────────────────────────────────────────────────
    let del_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.delete(&probe_uri),
    ).await;

    match del_result {
        Err(_elapsed) => {
            results.push(ValidationResult::warning(
                ErrorType::Network,
                "write-probe-delete",
                format!("Write probe DELETE timed out at {} — probe object may remain", endpoint),
                "Manually delete: sai3bench-preflight-probe-*.bin",
            ));
        }
        Ok(Err(e)) => {
            results.push(ValidationResult::warning(
                ErrorType::Permission,
                "write-probe-delete",
                format!("Write probe DELETE failed at {}: {} — probe object may remain", endpoint, e),
                "Manually delete: sai3bench-preflight-probe-*.bin",
            ));
        }
        Ok(Ok(())) => {
            results.push(ValidationResult::success(
                "write-probe-delete",
                format!("Write probe DELETE succeeded at {}", endpoint),
            ));
        }
    }

    results
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


