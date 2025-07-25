// -----------------------------------------------------------------------------
// warp‑test ‑ lightweight S3 perf / utility CLI built on top of s3dlio
// -----------------------------------------------------------------------------
// Step‑2: add full list / stat / get / delete commands + improved put.
// -----------------------------------------------------------------------------
// Crates required (add to Cargo.toml if not present):
//   url = "2.8"
//   regex = "1.10"
// -----------------------------------------------------------------------------

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use regex::Regex;
use std::time::Instant;
use url::Url;

use s3dlio::config::ObjectType;
use s3dlio::s3_utils::{
    delete_objects, get_objects_parallel, list_objects, put_objects_with_random_data_and_type,
    stat_object_uri,
};

// -----------------------------------------------------------------------------
// CLI definition
// -----------------------------------------------------------------------------
#[derive(Parser)]
#[command(
    name = "warp-test",
    version,
    about = "Light‑weight S3 tester & utility built on s3dlio"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Health‑check: ensure we can list under bucket/prefix
    Health {
        #[arg(long, conflicts_with = "bucket")] uri: Option<String>,
        #[arg(long, requires = "prefix")] bucket: Option<String>,
        #[arg(long, requires = "bucket", default_value = "")] prefix: String,
    },

    /// List keys (supports glob on basename)
    List { #[arg(long)] uri: String },

    /// Stat (HEAD) one object
    Stat { #[arg(long)] uri: String },

    /// GET benchmark / downloader (prefix, glob, or single key)
    Get {
        #[arg(long)] uri: String,
        /// parallel jobs
        #[arg(long, default_value_t = 4)] jobs: usize,
    },

    /// DELETE objects (supports glob / prefix)
    Delete {
        #[arg(long)] uri: String,
        /// batch size (max concurrent delete jobs)
        #[arg(long, default_value_t = 4)] jobs: usize,
    },

    /// PUT benchmark: upload random data buffers (raw)
    Put {
        #[arg(long, conflicts_with = "bucket")] uri: Option<String>,
        #[arg(long, requires = "prefix")] bucket: Option<String>,
        #[arg(long, requires = "bucket", default_value = "bench/")] prefix: String,
        #[arg(long, default_value_t = 1024)] object_size: usize,
        #[arg(long, default_value_t = 1)] objects: usize,
        #[arg(long, default_value_t = 4)] concurrency: usize,
    },
}

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------
fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Health { uri, bucket, prefix } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            health(&b, &p)?;
        }
        Commands::List { uri } => list_cmd(&uri)?,
        Commands::Stat { uri } => stat_cmd(&uri)?,
        Commands::Get { uri, jobs } => get_cmd(&uri, jobs)?,
        Commands::Delete { uri, jobs } => delete_cmd(&uri, jobs)?,
        Commands::Put { uri, bucket, prefix, object_size, objects, concurrency } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            put_bench(object_size, objects, &b, &p, concurrency)?;
        }
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------
/// Accept either full s3:// URI or separate bucket/prefix.
fn parse_s3(uri: &Option<String>, bucket: Option<String>, prefix: String) -> Result<(String, String)> {
    if let Some(u) = uri {
        let parsed = Url::parse(u).context("Invalid S3 URI")?;
        if parsed.scheme() != "s3" {
            bail!("URI must begin with s3://");
        }
        let bucket = parsed
            .host_str()
            .context("Missing bucket in URI")?
            .to_string();
        let mut p = parsed.path().trim_start_matches('/').to_string();
        if !p.is_empty() && !p.ends_with('/') {
            p.push('/');
        }
        return Ok((bucket, p));
    }
    let bucket = bucket.expect("--bucket is required if --uri is not set");
    Ok((bucket, prefix))
}

// -----------------------------------------------------------------------------
// Command impls
// -----------------------------------------------------------------------------
fn health(bucket: &str, prefix: &str) -> Result<()> {
    let keys = list_objects(bucket, prefix).context("list_objects_v2 failed")?;
    println!("OK – found {} objects under s3://{}/{}", keys.len(), bucket, prefix);
    Ok(())
}

fn list_cmd(uri: &str) -> Result<()> {
    let (bucket, key_pattern) = s3dlio::s3_utils::parse_s3_uri(uri)?;
    let (effective_prefix, glob_pattern) = if let Some(pos) = key_pattern.rfind('/') {
        (&key_pattern[..=pos], &key_pattern[pos + 1..])
    } else {
        ("", key_pattern.as_str())
    };
    let mut keys = list_objects(&bucket, effective_prefix)?;
    let regex_pattern = format!("^{}$", regex::escape(glob_pattern).replace("\\*", ".*"));
    let re = Regex::new(&regex_pattern).context("Invalid glob pattern")?;
    keys.retain(|k| re.is_match(k.rsplit('/').next().unwrap_or(k)));
    for k in &keys {
        println!("{}", k);
    }
    println!("\nTotal objects: {}", keys.len());
    Ok(())
}

fn stat_cmd(uri: &str) -> Result<()> {
    let os = stat_object_uri(uri)?;
    println!("Size            : {} bytes", os.size);
    println!("LastModified    : {:?}", os.last_modified);
    println!("ETag            : {:?}", os.e_tag);
    println!("Content-Type    : {:?}", os.content_type);
    println!("StorageClass    : {:?}", os.storage_class);
    println!("VersionId       : {:?}", os.version_id);
    Ok(())
}

fn get_cmd(uri: &str, jobs: usize) -> Result<()> {
    let (bucket, key_or_prefix) = s3dlio::s3_utils::parse_s3_uri(uri)?;

    // Determine list of keys
    let keys: Vec<String> = if key_or_prefix.contains('*') {
        // glob pattern (match only basename)
        let (effective_prefix, glob_pattern) = if let Some(pos) = key_or_prefix.rfind('/') {
            (&key_or_prefix[..=pos], &key_or_prefix[pos + 1..])
        } else {
            ("", key_or_prefix.as_str())
        };
        let mut k = list_objects(&bucket, effective_prefix)?;
        let re = Regex::new(&format!("^{}$", regex::escape(glob_pattern).replace("\\*", ".*")))?;
        k.retain(|kk| re.is_match(kk.rsplit('/').next().unwrap_or(kk)));
        k
    } else if key_or_prefix.ends_with('/') || key_or_prefix.is_empty() {
        // prefix listing
        list_objects(&bucket, &key_or_prefix)?
    } else {
        vec![key_or_prefix]
    };

    if keys.is_empty() {
        bail!("No objects match given URI");
    }

    let uris: Vec<String> = keys.iter().map(|k| format!("s3://{}/{}", bucket, k)).collect();
    eprintln!("Fetching {} objects with {} jobs…", uris.len(), jobs);
    let t0 = Instant::now();
    let total_bytes: usize = get_objects_parallel(&uris, jobs)?
        .into_iter()
        .map(|(_, bytes)| bytes.len())
        .sum();
    let dt = t0.elapsed();
    println!(
        "downloaded {:.2} MB in {:?} ({:.2} MB/s)",
        total_bytes as f64 / 1_048_576.0,
        dt,
        total_bytes as f64 / 1_048_576.0 / dt.as_secs_f64()
    );
    Ok(())
}

fn delete_cmd(uri: &str, _jobs: usize) -> Result<()> {
    let (bucket, key_or_pattern) = s3dlio::s3_utils::parse_s3_uri(uri)?;
    let keys_to_delete: Vec<String> = if key_or_pattern.contains('*') {
        let (effective_prefix, glob_pattern) = if let Some(pos) = key_or_pattern.rfind('/') {
            (&key_or_pattern[..=pos], &key_or_pattern[pos + 1..])
        } else {
            ("", key_or_pattern.as_str())
        };
        let mut keys = list_objects(&bucket, effective_prefix)?;
        let re = Regex::new(&format!("^{}$", regex::escape(glob_pattern).replace("\\*", ".*")))?;
        keys.retain(|k| re.is_match(k.rsplit('/').next().unwrap_or(k)));
        keys
    } else if key_or_pattern.ends_with('/') || key_or_pattern.is_empty() {
        list_objects(&bucket, &key_or_pattern)?
    } else {
        vec![key_or_pattern]
    };

    if keys_to_delete.is_empty() {
        bail!("No objects to delete under the specified URI");
    }
    eprintln!("Deleting {} objects…", keys_to_delete.len());
    delete_objects(&bucket, &keys_to_delete)?;
    eprintln!("Done.");
    Ok(())
}

fn put_bench(size: usize, count: usize, bucket: &str, prefix: &str, concurrency: usize) -> Result<()> {
    let uris: Vec<String> = (0..count)
        .map(|i| format!("s3://{}/{}obj_{}", bucket, prefix, i))
        .collect();
    let t0 = Instant::now();
    put_objects_with_random_data_and_type(&uris, size, concurrency, ObjectType::Raw, 1, 1)?;
    let dt = t0.elapsed();
    let mb_total = (count * size) as f64 / (1024.0 * 1024.0);
    println!(
        "Uploaded {} objects ({:.2} MB) in {:?} ({:.2} MB/s)",
        count,
        mb_total,
        dt,
        mb_total / dt.as_secs_f64()
    );
    Ok(())
}

