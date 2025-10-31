// src/bin/agent.rs

use anyhow::{Context, Result};
use clap::Parser;
use dotenvy::dotenv;
use futures::{stream::FuturesUnordered, StreamExt};
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::signal;
use tokio::sync::Semaphore;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{debug, error, info};

// Use AWS async SDK directly for listing to avoid nested runtimes
use aws_config::{self, BehaviorVersion};
use aws_sdk_s3 as s3;

// Modern ObjectStore pattern (v0.9.4+)
use s3dlio::object_store::store_for_uri;

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}
use pb::iobench::agent_server::{Agent, AgentServer};
use pb::iobench::{Empty, OpSummary, PingReply, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary, OpAggregateMetrics};

#[derive(Parser)]
#[command(name = "sai3bench-agent", version, about = "SAI3 Benchmark Agent (gRPC)")]
struct Cli {
    /// Increase verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Listen address, e.g. 0.0.0.0:7761
    #[arg(long, default_value = "0.0.0.0:7761")]
    listen: String,

    /// Enable TLS with an ephemeral self-signed certificate
    #[arg(long, default_value_t = false)]
    tls: bool,

    /// Subject DNS name for the self-signed cert (default "localhost")
    /// Controller must use --agent-domain to match this value for SNI.
    #[arg(long, default_value = "localhost")]
    tls_domain: String,

    /// If set, write the generated cert & key (PEM) here for the controller to trust with --agent-ca
    #[arg(long)]
    tls_write_ca: Option<std::path::PathBuf>,

    /// Optional comma-separated Subject Alternative Names (DNS names and/or IPs)
    /// Example: "localhost,myhost,10.0.0.5"
    /// NOTE: With the current minimal change we treat these as DNS names; see note below.
    #[arg(long)]
    tls_sans: Option<String>,
}

#[derive(Default)]
struct AgentSvc;

#[tonic::async_trait]
impl Agent for AgentSvc {
    async fn ping(&self, _req: Request<Empty>) -> Result<Response<PingReply>, Status> {
        Ok(Response::new(PingReply {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    async fn run_get(&self, req: Request<RunGetRequest>) -> Result<Response<OpSummary>, Status> {
        let RunGetRequest { uri, jobs } = req.into_inner();
        
        // Parse URI to extract base and pattern
        // For S3: s3://bucket/prefix/pattern
        // For other backends: backend://path/pattern
        let (base_uri, pattern) = if let Some(last_slash) = uri.rfind('/') {
            let base = &uri[..=last_slash];
            let pat = &uri[last_slash + 1..];
            (base, pat)
        } else {
            return Err(Status::invalid_argument("Invalid URI format"));
        };

        // Expand keys: supports exact key, prefix, or glob '*'
        let full_uris = if pattern.contains('*') {
            // Glob pattern - list directory and filter
            let keys = list_keys_for_uri(&uri)
                .await
                .map_err(to_status)?;
            keys.into_iter()
                .filter(|k| glob_match(pattern, k))
                .map(|k| format!("{}{}", base_uri, k))
                .collect::<Vec<_>>()
        } else if pattern.is_empty() {
            // List all in directory
            let keys = list_keys_for_uri(base_uri)
                .await
                .map_err(to_status)?;
            keys.into_iter()
                .map(|k| format!("{}{}", base_uri, k))
                .collect::<Vec<_>>()
        } else {
            // Exact key
            vec![uri.clone()]
        };

        if full_uris.is_empty() {
            return Err(Status::invalid_argument("No objects match given URI"));
        }

        let started = Instant::now();
        let sem = Arc::new(Semaphore::new(jobs as usize));
        let mut futs = FuturesUnordered::new();
        
        for full_uri in full_uris {
            let sem2 = sem.clone();
            futs.push(tokio::spawn(async move {
                let _p = sem2.acquire_owned().await.unwrap();
                // Use ObjectStore pattern with full URI
                let store = store_for_uri(&full_uri).map_err(|e| anyhow::anyhow!(e))?;
                let bytes = store.get(&full_uri).await?;
                Ok::<usize, anyhow::Error>(bytes.len())
            }));
        }
        
        let mut total = 0usize;
        while let Some(join_res) = futs.next().await {
            let inner = join_res.map_err(to_status)?.map_err(to_status)?;
            total += inner;
        }
        let secs = started.elapsed().as_secs_f64();
        Ok(Response::new(OpSummary {
            total_bytes: total as u64,
            seconds: secs,
            notes: String::new(),
        }))
    }

    async fn run_put(&self, req: Request<RunPutRequest>) -> Result<Response<OpSummary>, Status> {
        let RunPutRequest {
            bucket,
            prefix,
            object_size,
            objects,
            concurrency,
        } = req.into_inner();
        
        // Build base URI from bucket (assume s3:// for backward compatibility)
        let base_uri = if bucket.starts_with("s3://") || bucket.starts_with("file://") || 
                         bucket.starts_with("az://") || bucket.starts_with("gs://") {
            bucket.clone()
        } else {
            format!("s3://{}", bucket)
        };
        
        let keys: Vec<String> = (0..objects as usize)
            .map(|i| format!("{}{}obj_{}", base_uri, prefix, i))
            .collect();
        let data = vec![0u8; object_size as usize];

        let started = Instant::now();
        let sem = Arc::new(Semaphore::new(concurrency as usize));
        let mut futs = FuturesUnordered::new();
        
        for full_uri in keys {
            let d = data.clone();
            let sem2 = sem.clone();
            futs.push(tokio::spawn(async move {
                let _p = sem2.acquire_owned().await.unwrap();
                // Use ObjectStore pattern with full URI
                let store = store_for_uri(&full_uri).map_err(|e| anyhow::anyhow!(e))?;
                store.put(&full_uri, &d).await?;
                Ok::<(), anyhow::Error>(())
            }));
        }
        
        while let Some(join_res) = futs.next().await {
            join_res.map_err(to_status)?.map_err(to_status)?;
        }
        let secs = started.elapsed().as_secs_f64();
        Ok(Response::new(OpSummary {
            total_bytes: object_size * objects as u64,
            seconds: secs,
            notes: String::new(),
        }))
    }

    async fn run_workload(
        &self,
        req: Request<RunWorkloadRequest>,
    ) -> Result<Response<WorkloadSummary>, Status> {
        info!("Received run_workload request");
        
        let RunWorkloadRequest {
            config_yaml,
            agent_id,
            path_prefix,
            start_timestamp_ns,
            shared_storage,
        } = req.into_inner();

        debug!("Agent ID: {}, Path prefix: {}, Shared storage: {}", agent_id, path_prefix, shared_storage);
        debug!("Config YAML: {} bytes", config_yaml.len());

        // Parse the YAML configuration
        let mut config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .map_err(|e| {
                error!("Failed to parse YAML config: {}", e);
                Status::invalid_argument(format!("Invalid YAML config: {}", e))
            })?;

        debug!("YAML config parsed successfully");

        // Apply agent-specific path prefix for isolation
        config
            .apply_agent_prefix(&agent_id, &path_prefix, shared_storage)
            .map_err(|e| {
                error!("Failed to apply path prefix: {}", e);
                Status::internal(format!("Failed to apply path prefix: {}", e))
            })?;

        debug!("Agent-specific path prefix applied (shared_storage={})", shared_storage);

        debug!("Path prefix applied successfully");

        // Wait until coordinated start time
        let start_time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(start_timestamp_ns as u64);
        if let Ok(wait_duration) = start_time.duration_since(std::time::SystemTime::now()) {
            if wait_duration > std::time::Duration::from_secs(60) {
                error!("Start time is too far in the future: {:?}", wait_duration);
                return Err(Status::invalid_argument(
                    "Start time is too far in the future (>60s)",
                ));
            }
            debug!("Waiting {:?} until coordinated start", wait_duration);
            tokio::time::sleep(wait_duration).await;
        }
        // If start_time is in the past, start immediately

        info!("Starting workload execution for agent {}", agent_id);

        // Execute prepare phase if configured
        let (_prepared_objects, tree_manifest) = if let Some(ref prepare_config) = config.prepare {
            debug!("Executing prepare phase");
            let (prepared, manifest) = sai3_bench::workload::prepare_objects(prepare_config, Some(&config.workload))
                .await
                .map_err(|e| {
                    error!("Prepare phase failed: {}", e);
                    Status::internal(format!("Prepare phase failed: {}", e))
                })?;
            info!("Prepared {} objects", prepared.len());
            
            // Use configurable delay from YAML (only if objects were created)
            if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                let delay_secs = prepare_config.post_prepare_delay;
                info!("Waiting {}s for object propagation (configured delay)...", delay_secs);
                tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)).await;
            }
            
            (prepared, manifest)
        } else {
            debug!("No prepare phase configured");
            (Vec::new(), None)
        };

        // Execute the workload using existing workload::run function
        let summary = sai3_bench::workload::run(&config, tree_manifest)
            .await
            .map_err(|e| {
                error!("Workload execution failed: {}", e);
                Status::internal(format!("Workload execution failed: {}", e))
            })?;

        info!("Workload completed successfully for agent {}", agent_id);
        debug!("Summary: {} ops, {} bytes, {:.2}s", 
               summary.total_ops, summary.total_bytes, summary.wall_seconds);

        // v0.6.4: Create results directory and capture output
        let (metadata_json, tsv_content, results_path, op_log_path, 
             histogram_get, histogram_put, histogram_meta) = 
            create_agent_results(&agent_id, &config_yaml, &summary)
                .await
                .map_err(|e| {
                    error!("Failed to create agent results: {}", e);
                    Status::internal(format!("Failed to create agent results: {}", e))
                })?;

        info!("Agent results saved to: {}", results_path);

        // Convert Summary to WorkloadSummary protobuf message
        Ok(Response::new(WorkloadSummary {
            agent_id: agent_id.clone(),
            wall_seconds: summary.wall_seconds,
            total_ops: summary.total_ops,
            total_bytes: summary.total_bytes,
            console_log: String::new(),  // Agent doesn't generate console output
            metadata_json,
            tsv_content,
            results_path,
            op_log_path: op_log_path.unwrap_or_default(),
            histogram_get,
            histogram_put,
            histogram_meta,
            get: Some(OpAggregateMetrics {
                bytes: summary.get.bytes,
                ops: summary.get.ops,
                mean_us: summary.get.mean_us,
                p50_us: summary.get.p50_us,
                p95_us: summary.get.p95_us,
                p99_us: summary.get.p99_us,
            }),
            put: Some(OpAggregateMetrics {
                bytes: summary.put.bytes,
                ops: summary.put.ops,
                mean_us: summary.put.mean_us,
                p50_us: summary.put.p50_us,
                p95_us: summary.put.p95_us,
                p99_us: summary.put.p99_us,
            }),
            meta: Some(OpAggregateMetrics {
                bytes: summary.meta.bytes,
                ops: summary.meta.ops,
                mean_us: summary.meta.mean_us,
                p50_us: summary.meta.p50_us,
                p95_us: summary.meta.p95_us,
                p99_us: summary.meta.p99_us,
            }),
            p50_us: summary.p50_us,
            p95_us: summary.p95_us,
            p99_us: summary.p99_us,
        }))
    }
}

/// Create agent results directory and capture output files (v0.6.4)
/// Returns: (metadata_json, tsv_content, results_path, op_log_path, histogram_bytes)
async fn create_agent_results(
    agent_id: &str,
    config_yaml: &str,
    summary: &sai3_bench::workload::Summary,
) -> anyhow::Result<(String, String, String, Option<String>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    use sai3_bench::results_dir::ResultsDir;
    use sai3_bench::tsv_export::TsvExporter;
    use std::fs;
    use std::io::Write;
    use hdrhistogram::serialization::{Serializer, V2Serializer};
    
    // Get PID for unique naming (prevents collisions when multiple agents run on same host)
    let pid = std::process::id();
    
    // Write config to temp file so ResultsDir can copy it
    let temp_config_path = std::env::temp_dir().join(format!("agent-{}-{}-config.yaml", agent_id, pid));
    {
        let mut temp_file = fs::File::create(&temp_config_path)?;
        temp_file.write_all(config_yaml.as_bytes())?;
    }
    
    // Create results directory in /tmp for agent (includes PID for uniqueness)
    let agent_name = format!("{}-pid{}", agent_id, pid);
    let results_base = std::env::temp_dir();
    let mut results_dir = ResultsDir::create(
        &temp_config_path,
        Some(&agent_name),
        Some(&results_base),
    )?;
    
    // Clean up temp config file
    let _ = fs::remove_file(&temp_config_path);
    
    // Write TSV export
    let tsv_path = results_dir.tsv_path();
    let exporter = TsvExporter::with_path(&tsv_path)?;
    exporter.export_results(
        &summary.get_hists,
        &summary.put_hists,
        &summary.meta_hists,
        &summary.get_bins,
        &summary.put_bins,
        &summary.meta_bins,
        summary.wall_seconds,
    )?;
    
    // Note: Operation logs (--op-log) are not transferred via gRPC
    // They remain on the agent's local filesystem if created
    let op_log_path: Option<String> = None;  // Future: check for op-log file
    
    // Finalize results directory
    results_dir.finalize(summary.wall_seconds)?;
    
    // Read back the files we need to transfer
    let metadata_json = fs::read_to_string(results_dir.path().join("metadata.json"))?;
    let tsv_content = fs::read_to_string(&tsv_path)?;
    let results_path = results_dir.path().display().to_string();
    
    // Serialize histograms for accurate aggregation (v0.6.4)
    let mut serializer = V2Serializer::new();
    let mut get_hist_bytes = Vec::new();
    let mut put_hist_bytes = Vec::new();
    let mut meta_hist_bytes = Vec::new();
    
    // Serialize each histogram type
    for bucket_hist in summary.get_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut get_hist_bytes)
            .context("Failed to serialize GET histogram")?;
    }
    for bucket_hist in summary.put_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut put_hist_bytes)
            .context("Failed to serialize PUT histogram")?;
    }
    for bucket_hist in summary.meta_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut meta_hist_bytes)
            .context("Failed to serialize META histogram")?;
    }
    
    Ok((metadata_json, tsv_content, results_path, op_log_path, 
         get_hist_bytes, put_hist_bytes, meta_hist_bytes))
}

fn to_status<E: std::fmt::Display>(e: E) -> Status {
    Status::internal(e.to_string())
}

fn glob_match(pattern: &str, key: &str) -> bool {
    let escaped = regex::escape(pattern).replace(r"\*", ".*");
    let re = regex::Regex::new(&format!("^{}$", escaped)).unwrap();
    re.is_match(key)
}

/// Universal helper for listing keys from any URI backend
/// Works with s3://, file://, az://, gs://, etc.
async fn list_keys_for_uri(uri: &str) -> Result<Vec<String>> {
    // Try ObjectStore first (works for most backends)
    match store_for_uri(uri) {
        Ok(store) => {
            let keys = store.list(uri, false).await
                .context("Failed to list objects")?;
            // Extract just the key portion (filename) from full URIs
            let base_uri = if let Some(pos) = uri.rfind('/') {
                &uri[..=pos]
            } else {
                uri
            };
            Ok(keys.into_iter()
                .filter_map(|k| {
                    if let Some(stripped) = k.strip_prefix(base_uri) {
                        Some(stripped.to_string())
                    } else {
                        Some(k)
                    }
                })
                .collect())
        }
        Err(_) => {
            // Fallback to AWS SDK for S3-specific URIs (for compatibility)
            if uri.starts_with("s3://") {
                let parts: Vec<&str> = uri.strip_prefix("s3://").unwrap().split('/').collect();
                if parts.is_empty() {
                    return Ok(Vec::new());
                }
                let bucket = parts[0];
                let prefix = if parts.len() > 1 {
                    parts[1..].join("/")
                } else {
                    String::new()
                };
                list_keys_async(bucket, &prefix).await
            } else {
                Err(anyhow::anyhow!("Unsupported URI scheme: {}", uri))
            }
        }
    }
}

/// Async helper that lists object keys under `prefix` for `bucket` using the AWS Rust SDK.
///
/// We do this here (instead of using `s3dlio::s3_utils::list_objects`) to avoid
/// calling a blocking `block_on` inside a Tokio runtime, which can panic.
async fn list_keys_async(bucket: &str, prefix: &str) -> Result<Vec<String>> {
    // Use the modern, non-deprecated loader
    let cfg = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let client = s3::Client::new(&cfg);

    let mut out = Vec::new();
    let mut cont: Option<String> = None;
    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(c) = cont.as_deref() {
            req = req.continuation_token(c);
        }
        let resp = req.send().await.map_err(|e| anyhow::anyhow!(e))?;

        // `resp.contents()` is a slice &[Object]
        for obj in resp.contents() {
            if let Some(k) = obj.key() {
                let key = k.strip_prefix(prefix).unwrap_or(k);
                out.push(key.to_string());
            }
        }
        match resp.next_continuation_token() {
            Some(tok) if !tok.is_empty() => cont = Some(tok.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    dotenv().ok();
    let args = Cli::parse();
    
    // Initialize logging based on verbosity level
    let level = match args.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3bench_agent={},sai3_bench={}", level, level));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    debug!("Logging initialized at level: {}", level);
    
    let addr: SocketAddr = args.listen.parse().context("invalid listen addr")?;

    // Decide between plaintext and TLS
    if !args.tls {
        info!("Agent starting in PLAINTEXT mode on {}", addr);
        println!("sai3bench-agent listening (PLAINTEXT) on {}", addr);
        Server::builder()
            .add_service(AgentServer::new(AgentSvc::default()))
            .serve_with_shutdown(addr, async {
                let _ = signal::ctrl_c().await;
            })
            .await
            .context("tonic server failed")?;
        return Ok(());
    }

    // --- TLS path with self-signed cert generated at startup ---
    use rcgen::generate_simple_self_signed;
    use tonic::transport::{Identity, ServerTlsConfig};

    // Build SANs: use --tls-sans if provided (comma-separated), otherwise fallback to --tls-domain
    let sans: Vec<String> = if let Some(list) = &args.tls_sans {
        list.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![args.tls_domain.clone()]
    };

    let cert = generate_simple_self_signed(sans).context("generate self-signed cert")?;

    // rcgen 0.14: pull the PEM from the inner cert and the signing key
    let cert_pem = cert.cert.pem();                  // certificate as PEM string
    let key_pem  = cert.signing_key.serialize_pem(); // private key as PEM string

    // Optionally write them so the controller can trust with --agent-ca
    if let Some(dir) = args.tls_write_ca.as_ref() {
        fs::create_dir_all(dir).ok();
        fs::write(dir.join("agent_cert.pem"), &cert_pem).ok();
        fs::write(dir.join("agent_key.pem"), &key_pem).ok();
        println!(
            "wrote self-signed cert & key to {}",
            dir.to_string_lossy()
        );
    }

    let identity = Identity::from_pem(cert_pem.as_bytes(), key_pem.as_bytes());
    let tls = ServerTlsConfig::new().identity(identity);

    println!(
        "sai3bench-agent listening (TLS) on {} — SANs: {}",
        addr,
        if let Some(list) = &args.tls_sans {
            list
        } else {
            &args.tls_domain
        }
    );

    Server::builder()
        .tls_config(tls)?
        .add_service(AgentServer::new(AgentSvc::default()))
        .serve_with_shutdown(addr, async {
            let _ = signal::ctrl_c().await;
        })
        .await
        .context("tonic server (TLS) failed")?;

    Ok(())
}

