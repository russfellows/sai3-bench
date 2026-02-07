//! Error tracking for prepare and listing phases
//!
//! Provides resilient error handling with configurable thresholds for total and 
//! consecutive errors, enabling graceful degradation during transient failures.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::constants::{
    DEFAULT_PREPARE_MAX_ERRORS, 
    DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS,
    DEFAULT_LISTING_MAX_ERRORS,
    DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS,
};

/// Error tracking for prepare phase resilience (v0.8.13)
/// 
/// Tracks errors during object creation to implement configurable thresholds:
/// - Total error count (abort if exceeded)
/// - Consecutive error count (abort if backend seems completely down)
/// - Failed objects list for potential retry or reporting
#[derive(Clone)]
pub struct PrepareErrorTracker {
    total_errors: Arc<AtomicU64>,
    consecutive_errors: Arc<AtomicU64>,
    max_total_errors: u64,
    max_consecutive_errors: u64,
    failed_objects: Arc<std::sync::Mutex<Vec<PrepareFailure>>>,
}

/// Record of a failed prepare operation
#[derive(Debug, Clone)]
pub struct PrepareFailure {
    pub uri: String,
    pub size: u64,
    pub error: String,
    pub timestamp: Instant,
}

impl Default for PrepareErrorTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PrepareErrorTracker {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_PREPARE_MAX_ERRORS, DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS)
    }
    
    pub fn with_thresholds(max_total: u64, max_consecutive: u64) -> Self {
        Self {
            total_errors: Arc::new(AtomicU64::new(0)),
            consecutive_errors: Arc::new(AtomicU64::new(0)),
            max_total_errors: max_total,
            max_consecutive_errors: max_consecutive,
            failed_objects: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    
    /// Record a successful operation (resets consecutive error counter)
    pub fn record_success(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }
    
    /// Record an error and check if thresholds are exceeded
    /// Returns: (should_abort, total_errors, consecutive_errors)
    pub fn record_error(&self, uri: &str, size: u64, error: &str) -> (bool, u64, u64) {
        let total = self.total_errors.fetch_add(1, Ordering::Relaxed) + 1;
        let consecutive = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Record failure for potential retry/reporting
        {
            let mut failures = self.failed_objects.lock().unwrap();
            failures.push(PrepareFailure {
                uri: uri.to_string(),
                size,
                error: error.to_string(),
                timestamp: Instant::now(),
            });
        }
        
        let should_abort = total >= self.max_total_errors || consecutive >= self.max_consecutive_errors;
        
        (should_abort, total, consecutive)
    }
    
    pub fn get_stats(&self) -> (u64, u64) {
        let total = self.total_errors.load(Ordering::Relaxed);
        let consecutive = self.consecutive_errors.load(Ordering::Relaxed);
        (total, consecutive)
    }
    
    pub fn get_failures(&self) -> Vec<PrepareFailure> {
        self.failed_objects.lock().unwrap().clone()
    }
    
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }
}

// ============================================================================
// Listing Error Tracking
// ============================================================================

/// Error tracking for listing phase resilience (v0.8.14)
/// 
/// Similar to PrepareErrorTracker, but for LIST operations.
/// LIST can fail on transient network issues; we don't want to abort immediately.
#[derive(Clone)]
pub struct ListingErrorTracker {
    total_errors: Arc<AtomicU64>,
    consecutive_errors: Arc<AtomicU64>,
    max_total_errors: u64,
    max_consecutive_errors: u64,
    error_messages: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ListingErrorTracker {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_LISTING_MAX_ERRORS, DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS)
    }
    
    pub fn with_thresholds(max_total: u64, max_consecutive: u64) -> Self {
        Self {
            total_errors: Arc::new(AtomicU64::new(0)),
            consecutive_errors: Arc::new(AtomicU64::new(0)),
            max_total_errors: max_total,
            max_consecutive_errors: max_consecutive,
            error_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    
    /// Record a successful item retrieval (resets consecutive error counter)
    pub fn record_success(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }
    
    /// Record an error and check if thresholds are exceeded
    /// Returns: (should_abort, total_errors, consecutive_errors)
    pub fn record_error(&self, error_msg: &str) -> (bool, u64, u64) {
        let total = self.total_errors.fetch_add(1, Ordering::Relaxed) + 1;
        let consecutive = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Store first 20 error messages for debugging
        {
            let mut errors = self.error_messages.lock().unwrap();
            if errors.len() < 20 {
                errors.push(error_msg.to_string());
            }
        }
        
        let should_abort = total >= self.max_total_errors || consecutive >= self.max_consecutive_errors;
        
        (should_abort, total, consecutive)
    }
    
    pub fn get_stats(&self) -> (u64, u64) {
        (self.total_errors.load(Ordering::Relaxed), 
         self.consecutive_errors.load(Ordering::Relaxed))
    }
    
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }
    
    pub fn get_error_messages(&self) -> Vec<String> {
        self.error_messages.lock().unwrap().clone()
    }
}

impl Default for ListingErrorTracker {
    fn default() -> Self {
        Self::new()
    }
}
