//! I/O rate control for workload execution (v0.7.1)
//!
//! Implements inter-arrival time scheduling to control the rate at which
//! operations are issued (not their completion rate). Inspired by rdf-bench's
//! rate control mechanism but adapted for async Rust architecture.
//!
//! # Key Concepts
//!
//! - **Inter-arrival time**: Time between consecutive operation starts
//! - **Distribution**: How inter-arrival times are distributed (exponential, uniform, deterministic)
//! - **Per-worker throttling**: Each worker independently throttles based on its share of target IOPS
//!
//! # Example
//!
//! ```rust
//! use sai3_bench::config::{IoRateConfig, IopsTarget, ArrivalDistribution};
//! use sai3_bench::rate_controller::RateController;
//!
//! let config = IoRateConfig {
//!     iops: IopsTarget::Fixed(1000),
//!     distribution: ArrivalDistribution::Exponential,
//! };
//!
//! let controller = RateController::new(config, 16); // 16 workers
//!
//! // In worker loop:
//! // controller.wait_for_next().await;  // Throttle before each operation
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{sleep, interval, Interval, MissedTickBehavior};
use tokio::sync::Mutex;
use rand::rngs::StdRng;
use rand::SeedableRng;
use rand_distr::{Distribution, Exp};

use crate::config::{IoRateConfig, IopsTarget, ArrivalDistribution};

/// Rate controller for throttling operation starts
///
/// This controller calculates inter-arrival times based on the configured
/// IOPS target and distribution, then uses async sleep/interval to throttle
/// operation starts. Each worker gets its own controller instance (via Arc clone).
///
/// # Accuracy Notes
///
/// - **Exponential**: Uses `tokio::sleep()` - variability masks timer inaccuracy
/// - **Uniform**: Uses `tokio::time::Interval` - drift compensation, ~1ms granularity
/// - **Deterministic**: Uses `tokio::sleep()` with drift tracking
/// - **Recommended**: < 5,000 IOPS for ±5% accuracy tolerance
pub struct RateController {
    /// Original configuration
    config: IoRateConfig,
    /// Per-worker target inter-arrival time (microseconds)
    /// Zero means no throttling (unlimited)
    inter_arrival_micros: f64,
    /// Exponential distribution generator (for realistic arrivals)
    /// Only used when distribution is Exponential
    exp_dist: Option<Exp<f64>>,
    /// RNG for sampling from exponential distribution (Send-safe)
    /// Only used when distribution is Exponential
    /// Uses tokio::sync::Mutex for thread-safe access
    rng: Mutex<StdRng>,
    /// Interval timer for uniform distribution (drift compensation)
    /// Only used when distribution is Uniform
    /// Uses tokio::sync::Mutex for async-aware interior mutability (tick() requires &mut)
    uniform_interval: Mutex<Option<Interval>>,
    /// Start time for rate tracking
    start_time: Instant,
    /// Operations issued so far (for deterministic mode and statistics)
    ops_issued: Arc<AtomicU64>,
}

impl RateController {
    /// Create a new rate controller
    ///
    /// # Arguments
    ///
    /// * `config` - Rate control configuration
    /// * `workers` - Number of concurrent workers
    ///
    /// # Returns
    ///
    /// A new RateController instance. If `iops` is `IopsTarget::Max`,
    /// the controller will not throttle (wait_for_next returns immediately).
    pub fn new(config: IoRateConfig, workers: usize) -> Self {
        let inter_arrival_micros = match config.iops {
            IopsTarget::Max => 0.0,  // No throttling
            IopsTarget::Fixed(iops) => {
                // Calculate per-worker inter-arrival time
                // total_iops = workers * ops_per_worker_per_sec
                // inter_arrival = 1,000,000 / ops_per_worker_per_sec
                let ops_per_worker_per_sec = (iops as f64) / (workers as f64);
                1_000_000.0 / ops_per_worker_per_sec
            }
        };
        
        // Create exponential distribution if needed
        // Lambda parameter is 1 / mean inter-arrival time
        let exp_dist = if matches!(config.distribution, ArrivalDistribution::Exponential) 
                          && inter_arrival_micros > 0.0 {
            // Exponential distribution with mean = inter_arrival_micros
            Some(Exp::new(1.0 / inter_arrival_micros).unwrap())
        } else {
            None
        };
        
        // Create interval timer for uniform distribution (better drift compensation)
        let uniform_interval = if matches!(config.distribution, ArrivalDistribution::Uniform)
                                  && inter_arrival_micros > 0.0 {
            let duration = Duration::from_micros(inter_arrival_micros as u64);
            let mut interval_timer = interval(duration);
            // Delay: skip missed ticks (don't play catch-up)
            interval_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
            Mutex::new(Some(interval_timer))
        } else {
            Mutex::new(None)
        };
        
        Self {
            config,
            inter_arrival_micros,
            exp_dist,
            rng: Mutex::new(StdRng::seed_from_u64(rand::random())),
            uniform_interval,
            start_time: Instant::now(),
            ops_issued: Arc::new(AtomicU64::new(0)),
        }
    }
    
    /// Wait until next operation should be issued
    ///
    /// Returns immediately if no rate limiting is configured (iops=max).
    /// Otherwise, sleeps for the calculated inter-arrival time based on
    /// the configured distribution.
    ///
    /// # Distributions
    ///
    /// - **Exponential**: Samples from exponential distribution (Poisson arrivals) using tokio::sleep
    /// - **Uniform**: Uses tokio::time::Interval for drift compensation (~1ms granularity)
    /// - **Deterministic**: Calculates exact delay to maintain precise rate using tokio::sleep
    pub async fn wait_for_next(&self) {
        if self.inter_arrival_micros == 0.0 {
            return;  // No throttling
        }
        
        match self.config.distribution {
            ArrivalDistribution::Exponential => {
                // Poisson arrivals (realistic)
                // Sample from exponential distribution
                let delay_micros = {
                    let mut rng = self.rng.lock().await;
                    self.exp_dist.as_ref().unwrap().sample(&mut *rng)
                };
                
                if delay_micros > 0.0 {
                    let delay = Duration::from_micros(delay_micros as u64);
                    sleep(delay).await;
                }
            }
            ArrivalDistribution::Uniform => {
                // Use interval timer for better drift compensation
                // tick() automatically handles timing and compensates for drift
                // tokio::sync::Mutex is async-aware and can be held across await points
                let mut guard = self.uniform_interval.lock().await;
                if let Some(interval) = guard.as_mut() {
                    interval.tick().await;
                }
            }
            ArrivalDistribution::Deterministic => {
                // Precise timing - calculate exact delay to maintain rate
                // This mode tries to compensate for any drift in timing
                let ops = self.ops_issued.fetch_add(1, Ordering::Relaxed);
                let target_time_micros = (ops as f64) * self.inter_arrival_micros;
                let elapsed_micros = self.start_time.elapsed().as_micros() as f64;
                let delay_micros = target_time_micros - elapsed_micros;
                
                if delay_micros > 0.0 {
                    let delay = Duration::from_micros(delay_micros as u64);
                    sleep(delay).await;
                }
            }
        }
    }
    
    /// Get current actual IOPS rate (for monitoring/debugging)
    ///
    /// Returns the actual achieved IOPS based on elapsed time and
    /// operations issued. Only meaningful in Deterministic mode where
    /// we track operation count.
    pub fn current_rate(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            return 0.0;
        }
        let ops = self.ops_issued.load(Ordering::Relaxed);
        (ops as f64) / elapsed
    }
    
    /// Get target IOPS (for reporting)
    pub fn target_iops(&self) -> Option<u64> {
        match self.config.iops {
            IopsTarget::Max => None,
            IopsTarget::Fixed(iops) => Some(iops),
        }
    }
    
    /// Get configured distribution type (for reporting)
    pub fn distribution(&self) -> ArrivalDistribution {
        self.config.distribution
    }
}

/// Wrapper for optional rate controller
///
/// Makes it easy to conditionally apply rate limiting. If rate control
/// is not configured, wait() becomes a no-op with zero overhead.
#[derive(Clone)]
pub struct OptionalRateController {
    controller: Option<Arc<RateController>>,
}

impl OptionalRateController {
    /// Create a new optional rate controller
    ///
    /// # Arguments
    ///
    /// * `config` - Optional rate control configuration
    /// * `workers` - Number of concurrent workers
    ///
    /// # Returns
    ///
    /// If `config` is `None`, creates a no-op controller that doesn't throttle.
    /// Otherwise, creates a controller with the specified configuration.
    pub fn new(config: Option<IoRateConfig>, workers: usize) -> Self {
        let controller = config.map(|cfg| Arc::new(RateController::new(cfg, workers)));
        Self { controller }
    }
    
    /// Wait for next operation (no-op if rate control is disabled)
    pub async fn wait(&self) {
        if let Some(ref ctrl) = self.controller {
            ctrl.wait_for_next().await;
        }
    }
    
    /// Check if rate control is enabled
    pub fn is_enabled(&self) -> bool {
        self.controller.is_some()
    }
    
    /// Get reference to underlying controller (for statistics)
    pub fn controller(&self) -> Option<&Arc<RateController>> {
        self.controller.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_controller_max_no_throttle() {
        let config = IoRateConfig {
            iops: IopsTarget::Max,
            distribution: ArrivalDistribution::Exponential,
        };
        let controller = RateController::new(config, 16);
        assert_eq!(controller.inter_arrival_micros, 0.0);
        assert!(controller.exp_dist.is_none());
    }

    #[test]
    fn test_rate_controller_fixed_iops() {
        let config = IoRateConfig {
            iops: IopsTarget::Fixed(1000),
            distribution: ArrivalDistribution::Exponential,
        };
        let controller = RateController::new(config, 10);
        
        // 1000 IOPS / 10 workers = 100 ops/sec per worker
        // Inter-arrival = 1,000,000 / 100 = 10,000 microseconds
        assert_eq!(controller.inter_arrival_micros, 10_000.0);
        assert!(controller.exp_dist.is_some());
    }

    #[tokio::test]
    async fn test_rate_controller_uniform_distribution() {
        let config = IoRateConfig {
            iops: IopsTarget::Fixed(5000),
            distribution: ArrivalDistribution::Uniform,
        };
        let controller = RateController::new(config, 20);
        
        // 5000 IOPS / 20 workers = 250 ops/sec per worker
        // Inter-arrival = 1,000,000 / 250 = 4,000 microseconds
        assert_eq!(controller.inter_arrival_micros, 4_000.0);
        // Uniform distribution doesn't use exp_dist
        assert!(controller.exp_dist.is_none());
    }

    #[test]
    fn test_optional_controller_disabled() {
        let optional = OptionalRateController::new(None, 16);
        assert!(!optional.is_enabled());
        assert!(optional.controller().is_none());
    }

    #[test]
    fn test_optional_controller_enabled() {
        let config = IoRateConfig {
            iops: IopsTarget::Fixed(1000),
            distribution: ArrivalDistribution::Exponential,
        };
        let optional = OptionalRateController::new(Some(config), 16);
        assert!(optional.is_enabled());
        assert!(optional.controller().is_some());
    }
}
