// src/bin/controller.rs

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}

use pb::iobench::agent_client::AgentClient;
use pb::iobench::{Empty, RunGetRequest, RunPutRequest};

#[derive(Parser)]
#[command(name = "iobench-ctl", version, about = "IO Benchmark Controller (gRPC)")]
struct Cli {
    /// Comma-separated agent addresses (host:port)
    #[arg(long)]
    agents: String,

    /// If set, connect without TLS (http)
    #[arg(long, default_value_t = false)]
    insecure: bool,

    /// Path to PEM file containing the agent's self-signed certificate (to trust)
    /// Only used when not --insecure
    #[arg(long)]
    agent_ca: Option<PathBuf>,

    /// Override SNI / domain name for TLS (default "localhost").
    /// For IP addresses, you probably generated the agent cert with "DNS:localhost".
    #[arg(long, default_value = "localhost")]
    agent_domain: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Simple reachability check (ping) against all agents
    Ping,
    /// Distributed GET
    Get {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Distributed PUT
    Put {
        #[arg(long)]
        bucket: String,
        #[arg(long, default_value = "bench/")]
        prefix: String,
        #[arg(long, default_value_t = 1024)]
        object_size: u64,
        #[arg(long, default_value_t = 1)]
        objects: u64,
        #[arg(long, default_value_t = 4)]
        concurrency: u64,
    },
}

async fn mk_client(
    target: &str,
    insecure: bool,
    ca_path: Option<&PathBuf>,
    sni_domain: &str,
) -> Result<AgentClient<Channel>> {
    if insecure {
        // Plain HTTP
        let ep = format!("http://{}", target);
        let channel = Endpoint::try_from(ep)?
            .connect_timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .connect()
            .await?;
        return Ok(AgentClient::new(channel));
    }

    // HTTPS + custom root (self-signed cert from the agent)
    let ca_pem = match ca_path {
        Some(p) => fs::read(p).context("reading --agent-ca")?,
        None => anyhow::bail!("TLS is enabled (not --insecure), but --agent-ca was not provided"),
    };
    let ca = Certificate::from_pem(ca_pem);

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name(sni_domain);

    let ep = format!("https://{}", target);
    let channel = Endpoint::try_from(ep)?
        .tls_config(tls)?
        .connect_timeout(Duration::from_secs(5))
        .tcp_nodelay(true)
        .connect()
        .await?;
    Ok(AgentClient::new(channel))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();

    let agents: Vec<String> = cli
        .agents
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    match &cli.command {
        Commands::Ping => {
            for a in &agents {
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c.ping(Empty {}).await?.into_inner();
                eprintln!("connected to {} (agent version {})", a, r.version);
            }
        }
        Commands::Get { uri, jobs } => {
            for a in &agents {
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c
                    .run_get(RunGetRequest {
                        uri: uri.clone(),
                        jobs: *jobs as u32,
                    })
                    .await?
                    .into_inner();
                eprintln!(
                    "[{}] GET: {:.2} MB in {:.3}s -> {:.2} MB/s",
                    a,
                    r.total_bytes as f64 / 1_048_576.0,
                    r.seconds,
                    (r.total_bytes as f64 / 1_048_576.0) / r.seconds.max(1e-6)
                );
            }
        }
        Commands::Put {
            bucket,
            prefix,
            object_size,
            objects,
            concurrency,
        } => {
            for a in &agents {
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c
                    .run_put(RunPutRequest {
                        bucket: bucket.clone(),
                        prefix: prefix.clone(),
                        object_size: *object_size,
                        objects: *objects as u32,         // <-- proto expects u32
                        concurrency: *concurrency as u32, // <-- proto expects u32
                    })
                    .await?
                    .into_inner();
                eprintln!(
                    "[{}] PUT: {:.2} MB in {:.3}s -> {:.2} MB/s",
                    a,
                    r.total_bytes as f64 / 1_048_576.0,
                    r.seconds,
                    (r.total_bytes as f64 / 1_048_576.0) / r.seconds.max(1e-6)
                );
            }
        }
    }

    Ok(())
}

