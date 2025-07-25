use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use url::Url;

use s3dlio::s3_utils::{list_objects, put_objects_with_random_data_and_type};
use s3dlio::config::ObjectType;

#[derive(Parser)]
#[command(name = "warp-test", version, about = "Light‑weight S3 performance tester built on s3dlio")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Verify that the bucket+prefix are reachable (accepts s3:// URI or separate args)
    Health {
        /// Full S3 URI (e.g. s3://bucket/prefix) or just bucket name
        #[arg(long, conflicts_with = "bucket")]
        uri: Option<String>,
        /// Bucket name (e.g. my-bucket)
        #[arg(long, requires = "prefix")]
        bucket: Option<String>,
        /// Prefix under the bucket (e.g. bench/)
        #[arg(long, requires = "bucket", default_value = "")]
        prefix: String,
    },
    /// PUT‑only benchmark (similar URI handling)
    Put {
        /// Full S3 URI for prefix (e.g. s3://bucket/bench/)
        #[arg(long, conflicts_with = "bucket")]
        uri: Option<String>,
        /// Bucket name
        #[arg(long, requires = "prefix")]
        bucket: Option<String>,
        /// Prefix under bucket
        #[arg(long, requires = "bucket", default_value = "bench/")]
        prefix: String,
        /// object size in bytes
        #[arg(long, default_value_t = 1024)]
        object_size: usize,
        /// total number of objects
        #[arg(long, default_value_t = 1)]
        objects: usize,
        /// degree of parallelism
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Health { uri, bucket, prefix } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            health(&b, &p)?;
        }
        Commands::Put { uri, bucket, prefix, object_size, objects, concurrency } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            put_bench(object_size, objects, &b, &p, concurrency)?;
        }
    }

    Ok(())
}

/// Parse either an s3:// URI or separate bucket/prefix fields
fn parse_s3(
    uri: &Option<String>,
    bucket: Option<String>,
    prefix: String,
) -> Result<(String, String)> {
    if let Some(u) = uri {
        let url = Url::parse(u).context("Invalid S3 URI")?;
        if url.scheme() != "s3" {
            anyhow::bail!("URI must start with s3://");
        }
        let bucket = url.host_str().context("Missing bucket in URI")?.to_string();
        let mut p = url.path().trim_start_matches('/').to_string();
        if !p.is_empty() && !p.ends_with('/') {
            p.push('/');
        }
        return Ok((bucket, p));
    }
    // must have bucket presence if no uri
    let bucket = bucket.expect("--bucket is required if --uri is not set");
    Ok((bucket, prefix))
}

/// Synchronously list objects under `bucket/prefix`
fn health(bucket: &str, prefix: &str) -> Result<()> {
    let keys = list_objects(bucket, prefix)
        .context("list_objects_v2 failed")?;
    println!("OK – found {} objects under s3://{}/{}", keys.len(), bucket, prefix);
    Ok(())
}

/// Build a list of `s3://bucket/prefixobj_i` URIs and call the high‑level PUT helper
fn put_bench(
    size: usize,
    count: usize,
    bucket: &str,
    prefix: &str,
    concurrency: usize,
) -> Result<()> {
    // build full object URIs
    let uris: Vec<String> = (0..count)
        .map(|i| format!("s3://{}/{}obj_{}", bucket, prefix, i))
        .collect();

    put_objects_with_random_data_and_type(
        &uris,
        size,
        concurrency,
        ObjectType::Raw,
        /* dedup_factor */ 1,
        /* compress_factor */ 1,
    )?;

    println!(
        "Uploaded {} objects ({} bytes each) under s3://{}/{} with concurrency {}",
        count, size, bucket, prefix, concurrency
    );
    Ok(())
}

