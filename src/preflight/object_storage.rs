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

    // Group endpoints by bucket so we test one representative per bucket and
    // report "N/N endpoints" rather than one error line per IP.  This keeps the
    // output readable even for large multi-IP endpoint sets.
    let mut bucket_groups: Vec<(String, Vec<String>)> = Vec::new();
    for ep in endpoints {
        let label = bucket_label(ep);
        if let Some(group) = bucket_groups.iter_mut().find(|(l, _)| l == &label) {
            group.1.push(ep.clone());
        } else {
            bucket_groups.push((label, vec![ep.clone()]));
        }
    }

    for (label, bucket_endpoints) in &bucket_groups {
        let representative = &bucket_endpoints[0];
        let count = bucket_endpoints.len();

        tracing::info!(
            "Validating bucket '{}' ({} endpoint{}) via {}",
            label,
            count,
            if count == 1 { "" } else { "s" },
            representative
        );

        // Build an ObjectStore for the representative endpoint
        let store = match store_for_uri(representative) {
            Ok(s) => s,
            Err(e) => {
                results.push(ValidationResult::error(
                    ErrorType::Configuration,
                    "init",
                    format!("Cannot initialize storage for {}: {}", representative, e),
                    "Check URI format: s3://bucket/prefix  gs://bucket/prefix  az://account/container/prefix",
                ));
                continue;
            }
        };

        // Stream at most 1 key — fast even for buckets with billions of objects.
        let mut stream = store.list_stream(representative, false);
        match tokio::time::timeout(std::time::Duration::from_secs(30), stream.next()).await {
            Err(_elapsed) => {
                results.push(ValidationResult::error(
                    ErrorType::Network,
                    "list",
                    format!(
                        "Timed out connecting to {} (bucket: {}{})",
                        representative,
                        label,
                        if count > 1 { format!(" — {} endpoints total", count) } else { String::new() }
                    ),
                    "Check network connectivity, DNS resolution, and storage endpoint configuration",
                ));
            }
            Ok(Some(Err(e))) => {
                let (error_type, suggestion) = classify_storage_error(&e);
                let suffix = if count > 1 {
                    format!(" (applies to all {} endpoints for this bucket)", count)
                } else {
                    String::new()
                };
                results.push(ValidationResult::error(
                    error_type,
                    "list",
                    format!("Failed to list {}: {}{}", representative, e, suffix),
                    suggestion,
                ));
            }
            Ok(None) => {
                let msg = if count > 1 {
                    format!(
                        "Bucket '{}' accessible (empty prefix) — {} endpoints",
                        label, count
                    )
                } else {
                    format!(
                        "Bucket accessible (no objects at prefix): {}",
                        representative
                    )
                };
                results.push(ValidationResult::success("list", msg));
            }
            Ok(Some(Ok(_first_key))) => {
                let msg = if count > 1 {
                    format!("Bucket '{}' accessible — {} endpoints", label, count)
                } else {
                    format!("Bucket accessible: {}", representative)
                };
                results.push(ValidationResult::success("list", msg));
            }
        }

        // Write probe: test only the representative endpoint; report count in message.
        if check_write {
            let probe_results = write_probe(representative, count).await;
            results.extend(probe_results);
        }
    }

    Ok(ValidationSummary::new(results))
}

/// PUT a tiny probe object, GET it back and verify its content, then DELETE it.
///
/// `endpoint_count` is the total number of IPs for this bucket; it's included in
/// messages so the user knows "1 of N sampled" without seeing N individual results.
///
/// All three steps issue only warnings on failure — the preflight does not
/// hard-fail because a read-only credential set on a GET-only workload should
/// never prevent the run.
async fn write_probe(endpoint: &str, endpoint_count: usize) -> Vec<ValidationResult> {
    let mut results = Vec::new();

    // Build a unique probe URI from the endpoint so concurrent agents don't collide.
    // Uniqueness comes from a random 16-bit suffix; the content itself is a fixed
    // known string (PROBE_CONTENT) that we verify on read-back.
    let suffix: u32 = rand::rng().random();
    let sep = if endpoint.ends_with('/') { "" } else { "/" };
    let probe_uri = format!(
        "{}{}sai3bench-preflight-probe-{:08x}.bin",
        endpoint, sep, suffix
    );

    // ── Step 1: PUT ────────────────────────────────────────────────────────────
    let store = match store_for_uri(&probe_uri) {
        Ok(s) => s,
        Err(e) => {
            results.push(ValidationResult::warning(
                ErrorType::Configuration,
                "write-probe",
                format!(
                    "Cannot initialize store for write probe at {}: {}",
                    endpoint, e
                ),
                "Write probe skipped — check URI format",
            ));
            return results;
        }
    };

    let put_data = bytes::Bytes::from_static(PROBE_CONTENT);
    let put_result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        store.put(&probe_uri, put_data),
    )
    .await;

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
            let suffix = if endpoint_count > 1 {
                format!(" (sampled 1 of {} endpoints)", endpoint_count)
            } else {
                String::new()
            };
            results.push(ValidationResult::warning(
                ErrorType::Permission,
                "write-probe-put",
                format!("Write probe PUT failed at {}{}: {}", endpoint, suffix, e),
                "Workload may fail if it requires write access — check write permissions",
            ));
            return results;
        }
        Ok(Ok(())) => {
            let suffix = if endpoint_count > 1 {
                format!(" ({} endpoints)", endpoint_count)
            } else {
                String::new()
            };
            results.push(ValidationResult::success(
                "write-probe-put",
                format!("Write probe PUT succeeded at {}{}", endpoint, suffix),
            ));
        }
    }

    // ── Step 2: GET and verify content ─────────────────────────────────────────
    let get_result =
        tokio::time::timeout(std::time::Duration::from_secs(15), store.get(&probe_uri)).await;

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
    let del_result =
        tokio::time::timeout(std::time::Duration::from_secs(15), store.delete(&probe_uri)).await;

    match del_result {
        Err(_elapsed) => {
            results.push(ValidationResult::warning(
                ErrorType::Network,
                "write-probe-delete",
                format!(
                    "Write probe DELETE timed out at {} — probe object may remain",
                    endpoint
                ),
                "Manually delete: sai3bench-preflight-probe-*.bin",
            ));
        }
        Ok(Err(e)) => {
            results.push(ValidationResult::warning(
                ErrorType::Permission,
                "write-probe-delete",
                format!(
                    "Write probe DELETE failed at {}: {} — probe object may remain",
                    endpoint, e
                ),
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

/// Extract a human-readable bucket label from a storage URI.
///
/// `s3://10.9.0.17:80/my-bucket/prefix/` → `"my-bucket"`  
/// `gs://my-bucket/prefix/` → `"my-bucket"`
/// Returns the full URI on parse failure (always produces a useful string).
fn bucket_label(uri: &str) -> String {
    // Strip scheme (s3://, gs://, az://)
    let rest = if let Some(pos) = uri.find("://") {
        &uri[pos + 3..]
    } else {
        return uri.to_string();
    };
    // Drop host[:port]
    let after_host = if let Some(pos) = rest.find('/') {
        &rest[pos + 1..]
    } else {
        return uri.to_string();
    };
    // First path segment = bucket
    let bucket = after_host.split('/').next().unwrap_or(after_host);
    if bucket.is_empty() {
        uri.to_string()
    } else {
        bucket.to_string()
    }
}

/// Classify an error string (both display and debug formats) into an [ErrorType]
/// with an actionable suggestion.
///
/// AWS SDK Rust uses the term "service error" for any HTTP 4xx/5xx response body
/// that was parsed as an S3 error document, which means the status code is NOT
/// present in the `Display` string — only in the `Debug` representation.
/// We check both forms so we can still give a useful classification.
fn classify_storage_error(e: &anyhow::Error) -> (ErrorType, &'static str) {
    let display = e.to_string().to_lowercase();
    let debug = format!("{:?}", e).to_lowercase();

    // Check display string first (human-readable chain)
    if display.contains("403")
        || display.contains("forbidden")
        || display.contains("accessdenied")
        || display.contains("access denied")
        || debug.contains("403")
        || debug.contains("forbidden")
        || debug.contains("accessdenied")
    {
        return (
            ErrorType::Permission,
            "Check bucket name, credentials, and IAM/ACL permissions",
        );
    }
    if display.contains("401")
        || display.contains("unauthorized")
        || display.contains("unauthenticated")
        || debug.contains("401")
        || debug.contains("unauthorized")
    {
        return (
            ErrorType::Authentication,
            "Credentials invalid or expired — check AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY",
        );
    }
    if display.contains("404")
        || display.contains("nosuchbucket")
        || display.contains("no such bucket")
        || display.contains("not found")
        || debug.contains("nosuchbucket")
        || debug.contains("404")
    {
        return (
            ErrorType::Configuration,
            "Bucket does not exist or endpoint path is incorrect",
        );
    }
    // "service error" = S3 XML error body parsed by SDK; status code is in debug only
    if display.contains("service error") || debug.contains("service error") {
        return (
            ErrorType::Permission,
            "S3 service returned an error — likely 403 Forbidden (wrong bucket, missing credentials, \
             or insufficient permissions). Check AWS credentials and bucket name.",
        );
    }
    // Generic network/connectivity failures
    (
        ErrorType::Network,
        "Check network connectivity and storage backend availability",
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── bucket_label ──────────────────────────────────────────────────────────

    #[test]
    fn test_bucket_label_s3_with_ip_and_port() {
        // The original bug trigger: IP:port endpoint with bucket in path
        assert_eq!(
            bucket_label("s3://10.9.0.17:80/my-bucket/prefix/"),
            "my-bucket"
        );
    }

    #[test]
    fn test_bucket_label_s3_standard() {
        assert_eq!(
            bucket_label("s3://s3.amazonaws.com/my-bucket/subdir/"),
            "my-bucket"
        );
    }

    #[test]
    fn test_bucket_label_s3_no_trailing_path() {
        // Bucket only, no sub-path
        assert_eq!(bucket_label("s3://host/just-bucket"), "just-bucket");
    }

    #[test]
    fn test_bucket_label_gs() {
        assert_eq!(
            bucket_label("gs://my-gcs-project/datasets/training/"),
            "datasets"
        );
    }

    #[test]
    fn test_bucket_label_az() {
        assert_eq!(
            bucket_label("az://mystorageaccount/mycontainer/blobs/"),
            "mycontainer"
        );
    }

    #[test]
    fn test_bucket_label_no_scheme_passthrough() {
        // No "://" → returned as-is
        assert_eq!(bucket_label("not-a-uri"), "not-a-uri");
    }

    #[test]
    fn test_bucket_label_host_no_path() {
        // Nothing after host → fall back to full URI
        assert_eq!(bucket_label("s3://just-a-host"), "s3://just-a-host");
    }

    // ── classify_storage_error ────────────────────────────────────────────────

    #[test]
    fn test_classify_403_in_display() {
        let err = anyhow::anyhow!("request failed with status 403");
        let (kind, _) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Permission);
    }

    #[test]
    fn test_classify_forbidden_in_display() {
        let err = anyhow::anyhow!("HTTP Forbidden: access denied to resource");
        let (kind, hint) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Permission);
        assert!(hint.contains("credentials") || hint.contains("IAM"));
    }

    #[test]
    fn test_classify_accessdenied_in_display() {
        let err = anyhow::anyhow!("S3 Error: AccessDenied - you lack permission");
        let (kind, _) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Permission);
    }

    #[test]
    fn test_classify_service_error_permission() {
        // "service error" without a status code in display — matches the AWS SDK pattern.
        // The fix: we also check the Debug representation which includes the HTTP status.
        let err = anyhow::anyhow!("service error encountered while making a request");
        let (kind, hint) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Permission);
        assert!(hint.contains("403") || hint.contains("Forbidden") || hint.contains("permission"));
    }

    #[test]
    fn test_classify_401_unauthorized() {
        let err = anyhow::anyhow!("401 Unauthorized - invalid credentials");
        let (kind, hint) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Authentication);
        assert!(
            hint.contains("AWS_ACCESS_KEY_ID")
                || hint.contains("expired")
                || hint.contains("Credentials")
        );
    }

    #[test]
    fn test_classify_unauthenticated() {
        let err = anyhow::anyhow!("unauthenticated: token missing");
        let (kind, _) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Authentication);
    }

    #[test]
    fn test_classify_nosuchbucket_in_display() {
        let err = anyhow::anyhow!("NoSuchBucket: the specified bucket does not exist");
        let (kind, hint) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Configuration);
        assert!(hint.contains("does not exist") || hint.contains("path"));
    }

    #[test]
    fn test_classify_404_in_display() {
        let err = anyhow::anyhow!("endpoint returned 404 not found");
        let (kind, _) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Configuration);
    }

    #[test]
    fn test_classify_network_fallback() {
        // Unknown error → generic network classification
        let err = anyhow::anyhow!("connection reset by peer");
        let (kind, hint) = classify_storage_error(&err);
        assert_eq!(kind, ErrorType::Network);
        assert!(hint.contains("network") || hint.contains("connectivity"));
    }
}
