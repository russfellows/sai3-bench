//! Deferred retry logic with adaptive strategy (v0.8.52)
//!
//! Implements intelligent retry for failed object creation with adaptive strategies
//! based on failure rates (80-20 rule):
//! - <20% failures: Full retry (10 attempts) - likely transient issues  
//! - 20-80% failures: Limited retry (3 attempts) - potential systemic problems
//! - >80% failures: Skip retry - clear systemic failure, no point retrying

use std::sync::Arc;
use std::time::{Duration, Instant};
use futures::StreamExt;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::workload::{RetryConfig, RetryResult, retry_with_backoff};
use super::error_tracking::PrepareFailure;

/// Result of a successful retry operation
#[derive(Debug, Clone)]
pub struct RetrySuccess {
    pub uri: String,
    pub size: u64,
    pub latency: Duration,
}

/// Results from the deferred retry phase (v0.8.52)
#[derive(Debug, Clone)]
pub struct RetryResults {
    pub successes: Vec<RetrySuccess>,
    pub permanent_failures: Vec<(String, String)>,  // (uri, error_message)
}

/// Determine retry strategy based on failure rate
/// 
/// Returns: (should_retry, max_retries, description)
/// 
/// Strategy:
/// - failure_rate > 80%: Skip retry (systemic failure)
/// - failure_rate 20-80%: Limited retry (3 attempts)
/// - failure_rate < 20%: Full retry (10 attempts)
pub(crate) fn determine_retry_strategy(failed_count: usize, total_attempted: usize) -> (bool, u32, String) {
    if total_attempted == 0 {
        return (false, 0, "No objects attempted".to_string());
    }
    
    let failure_rate = (failed_count as f64 / total_attempted as f64) * 100.0;
    
    if failure_rate > 80.0 {
        (false, 0, format!(
            "Skipping retry: {:.1}% failure rate indicates systemic issue (failed: {}, total: {})",
            failure_rate, failed_count, total_attempted
        ))
    } else if failure_rate >= 20.0 {
        (true, 3, format!(
            "Limited retry: {:.1}% failure rate (3 attempts max)",
            failure_rate
        ))
    } else {
        (true, 10, format!(
            "Full retry: {:.1}% failure rate (10 attempts max)",
            failure_rate
        ))
    }
}

/// Retry all failed objects using more aggressive retry settings
/// This runs AFTER the main prepare phase completes, so it doesn't impact fast path performance
/// 
/// Strategy:
/// - Use fewer concurrent workers (concurrency/4) to avoid overwhelming backend
/// - More aggressive exponential backoff (longer delays)
/// - Adaptive retry count based on failure rate (see determine_retry_strategy)
/// - Fail permanently after all retries exhausted
pub(crate) async fn retry_failed_objects(
    failures: Vec<PrepareFailure>,
    store_cache: &Arc<std::collections::HashMap<String, Arc<Box<dyn s3dlio::ObjectStore>>>>,
    live_stats_tracker: Option<&Arc<crate::live_stats::LiveStatsTracker>>,
    concurrency: usize,
    total_attempted: usize,
) -> RetryResults {
    use futures::stream::FuturesUnordered;
    
    let mut successes = Vec::new();
    let mut permanent_failures = Vec::new();
    
    if failures.is_empty() {
        return RetryResults { successes, permanent_failures };
    }
    
    // v0.8.52: Adaptive retry based on failure rate
    let (should_retry, max_retries, strategy_desc) = determine_retry_strategy(failures.len(), total_attempted);
    
    if !should_retry {
        warn!("🚫 {}", strategy_desc);
        // Mark all failures as permanent
        for failure in failures {
            permanent_failures.push((failure.uri, format!("Skipped retry due to high failure rate: {}", failure.error)));
        }
        return RetryResults { successes, permanent_failures };
    }
    
    info!("🔄 {}", strategy_desc);
    info!("🔄 Retrying {} failed objects with {} workers...", failures.len(), concurrency);
    
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    
    // Adaptive retry config based on failure rate
    let retry_config = RetryConfig {
        max_retries,               // Adaptive: 3 for high failure, 10 for low
        initial_delay_ms: 500,     // Start with longer delay
        max_delay_ms: 30000,       // Up to 30 seconds between attempts
        backoff_multiplier: 2.0,
        jitter_factor: 0.3,
    };
    
    for failure in failures {
        let sem2 = sem.clone();
        let stores = store_cache.clone();
        let retry_cfg = retry_config.clone();
        let tracker = live_stats_tracker.cloned();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Extract store_uri from full URI (scheme://bucket or file:///path)
            let store_uri = if let Some(pos) = failure.uri.rfind('/') {
                failure.uri[..=pos].to_string()
            } else {
                failure.uri.clone()  // Fallback: use full URI as store URI
            };
            
            // Get cached store instance
            let store = match stores.get(&store_uri) {
                Some(s) => s.clone(),
                None => {
                    // Store not in cache - might be different URI format
                    // Try to find a matching store by prefix
                    let matching_store = stores.iter()
                        .find(|(uri, _)| failure.uri.starts_with(*uri))
                        .map(|(_, store)| store.clone());
                    
                    match matching_store {
                        Some(s) => s,
                        None => return (None, Some((
                            failure.uri.clone(),
                            format!("Store not found in cache for URI: {}", failure.uri)
                        ))),
                    }
                }
            };
            
            // Regenerate data for retry (using same parameters as original)
            // For simplicity, use Random fill pattern (most common case)
            // TODO: Store fill pattern in PrepareFailure if we need exact regeneration
            let data = crate::data_gen_pool::generate_data_optimized(
                failure.size as usize,
                1,  // Default dedup
                0,  // Default compress
            );
            
            let put_start = Instant::now();
            let uri_for_retry = failure.uri.clone();
            
            let put_result = retry_with_backoff(
                &format!("RETRY {}", &failure.uri),
                &retry_cfg,
                || {
                    let store_ref = store.clone();
                    let uri_ref = uri_for_retry.clone();
                    let data_ref = data.clone();
                    async move {
                        store_ref.put(&uri_ref, data_ref).await
                            .map_err(|e| anyhow::anyhow!("{}", e))
                    }
                }
            ).await;
            
            match put_result {
                RetryResult::Success(_) => {
                    let latency = put_start.elapsed();
                    
                    // Record in live stats if available
                    if let Some(ref t) = tracker {
                        t.record_put(failure.size as usize, latency);
                    }
                    
                    (Some(RetrySuccess {
                        uri: failure.uri,
                        size: failure.size,
                        latency,
                    }), None)
                }
                RetryResult::Failed(e) => {
                    (None, Some((failure.uri, format!("{}", e))))
                }
            }
        }));
    }
    
    // Collect results
    while let Some(result) = futs.next().await {
        match result {
            Ok((Some(success), None)) => {
                successes.push(success);
            }
            Ok((None, Some(failure))) => {
                permanent_failures.push(failure);
            }
            Ok(_) => {
                // Shouldn't happen (both Some or both None)
                warn!("Unexpected retry result state");
            }
            Err(e) => {
                warn!("Retry task panicked: {}", e);
            }
        }
    }
    
    RetryResults { successes, permanent_failures }
}
