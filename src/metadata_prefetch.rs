// src/metadata_prefetch.rs
//
// Asynchronous metadata pre-fetching pipeline to eliminate stat() overhead
// from the critical I/O path. Separate worker pool fetches file sizes ahead
// of I/O operations.

use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// Metadata information for an object
#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    /// Full URI of the object
    pub uri: String,
    /// Size in bytes (if available)
    pub size: Option<u64>,
    /// Whether this is a local file (file:// or direct://)
    pub is_local: bool,
}

/// Configuration for metadata pre-fetching
#[derive(Debug, Clone)]
pub struct MetadataPrefetchConfig {
    /// Number of worker tasks for fetching metadata
    pub num_workers: usize,
    /// Channel buffer size (how many metadata items to buffer)
    pub buffer_size: usize,
}

impl Default for MetadataPrefetchConfig {
    fn default() -> Self {
        Self {
            num_workers: 8,  // Default to 8 metadata workers
            buffer_size: 64, // Buffer up to 64 metadata entries
        }
    }
}

/// Metadata pre-fetcher that runs asynchronously ahead of I/O operations
pub struct MetadataPrefetcher {
    config: MetadataPrefetchConfig,
}

impl MetadataPrefetcher {
    /// Create a new metadata pre-fetcher with configuration
    pub fn new(config: MetadataPrefetchConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration
    pub fn with_default_config() -> Self {
        Self::new(MetadataPrefetchConfig::default())
    }

    /// Spawn metadata pre-fetch pipeline
    /// 
    /// Returns a receiver channel that yields ObjectMetadata as it's fetched.
    /// The pipeline spawns worker tasks that fetch metadata concurrently.
    /// 
    /// # Arguments
    /// * `uris` - Iterator of URIs to fetch metadata for
    /// 
    /// # Returns
    /// Receiver channel that yields ObjectMetadata results
    pub async fn prefetch_metadata<I>(
        &self,
        uris: I,
    ) -> mpsc::Receiver<ObjectMetadata>
    where
        I: IntoIterator<Item = String> + Send + 'static,
        I::IntoIter: Send,
    {
        let (tx, rx) = mpsc::channel(self.config.buffer_size);
        let num_workers = self.config.num_workers;

        // Spawn metadata fetching pipeline
        tokio::spawn(async move {
            // Channel for distributing URIs to workers
            let (uri_tx, uri_rx) = mpsc::channel::<String>(num_workers * 2);
            let uri_rx = std::sync::Arc::new(tokio::sync::Mutex::new(uri_rx));

            // Spawn worker tasks
            let mut handles = Vec::new();
            for worker_id in 0..num_workers {
                let uri_rx = uri_rx.clone();
                let tx = tx.clone();

                let handle = tokio::spawn(async move {
                    loop {
                        let uri = {
                            let mut rx = uri_rx.lock().await;
                            rx.recv().await
                        };

                        match uri {
                            Some(uri) => {
                                let metadata = fetch_metadata_for_uri(&uri).await;
                                if tx.send(metadata).await.is_err() {
                                    debug!("Worker {}: Receiver dropped, stopping", worker_id);
                                    break;
                                }
                            }
                            None => {
                                debug!("Worker {}: No more URIs, stopping", worker_id);
                                break;
                            }
                        }
                    }
                });
                handles.push(handle);
            }

            // Feed URIs to workers
            for uri in uris {
                if uri_tx.send(uri).await.is_err() {
                    warn!("Failed to send URI to workers (channel closed)");
                    break;
                }
            }

            // Drop sender to signal workers to stop
            drop(uri_tx);

            // Wait for all workers to complete
            for handle in handles {
                let _ = handle.await;
            }
        });

        rx
    }
}

/// Fetch metadata for a single URI
/// 
/// For local files (file:// or direct://), fetches size via std::fs::metadata.
/// For remote URIs (s3://, gs://, az://), size is set to None (would require HEAD).
async fn fetch_metadata_for_uri(uri: &str) -> ObjectMetadata {
    let is_local = uri.starts_with("file://") || uri.starts_with("direct://");

    let size = if is_local {
        // Extract file path from URI
        let path_str = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("direct://"))
            .unwrap_or(uri);

        // Fetch metadata using tokio's fs (async)
        match tokio::fs::metadata(path_str).await {
            Ok(meta) => {
                trace!("Prefetched metadata for {}: {} bytes", uri, meta.len());
                Some(meta.len())
            }
            Err(e) => {
                warn!("Failed to fetch metadata for {}: {}", uri, e);
                None
            }
        }
    } else {
        // For cloud storage, don't fetch metadata (would require HEAD request)
        // Size will be determined during actual I/O operation
        None
    };

    ObjectMetadata {
        uri: uri.to_string(),
        size,
        is_local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_metadata_prefetch_local_files() {
        // Create temp directory with test files
        let temp_dir = TempDir::new().unwrap();
        let mut uris = Vec::new();

        for i in 0..10 {
            let path = temp_dir.path().join(format!("file{}.txt", i));
            let mut file = File::create(&path).unwrap();
            let content = format!("Test content {}", i);
            file.write_all(content.as_bytes()).unwrap();

            let uri = format!("file://{}", path.display());
            uris.push(uri);
        }

        // Create prefetcher and fetch metadata
        let prefetcher = MetadataPrefetcher::with_default_config();
        let mut rx = prefetcher.prefetch_metadata(uris.clone()).await;

        // Collect results
        let mut results = Vec::new();
        while let Some(metadata) = rx.recv().await {
            results.push(metadata);
        }

        // Verify we got metadata for all files
        assert_eq!(results.len(), 10);
        
        // Verify all are marked as local
        assert!(results.iter().all(|m| m.is_local));
        
        // Verify all have size information
        assert!(results.iter().all(|m| m.size.is_some()));
    }

    #[tokio::test]
    async fn test_metadata_prefetch_remote_uris() {
        // Test with remote URIs (should not fetch metadata)
        let uris = vec![
            "s3://bucket/key1".to_string(),
            "gs://bucket/key2".to_string(),
            "az://container/key3".to_string(),
        ];

        let prefetcher = MetadataPrefetcher::with_default_config();
        let mut rx = prefetcher.prefetch_metadata(uris).await;

        let mut results = Vec::new();
        while let Some(metadata) = rx.recv().await {
            results.push(metadata);
        }

        assert_eq!(results.len(), 3);
        
        // All should be marked as not local
        assert!(results.iter().all(|m| !m.is_local));
        
        // All should have no size information (no HEAD request made)
        assert!(results.iter().all(|m| m.size.is_none()));
    }
}
