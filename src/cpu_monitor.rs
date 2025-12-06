// src/cpu_monitor.rs
//
// Lightweight CPU utilization monitoring for sai3-bench
// Tracks User, System, and Wait (IO-wait) CPU time per agent

use std::fs;
use std::time::{Duration, Instant};
use anyhow::{Context, Result};

/// CPU utilization percentages over a sampling period
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuUtilization {
    pub user_percent: f64,    // User mode CPU time
    pub system_percent: f64,  // Kernel mode CPU time  
    pub iowait_percent: f64,  // Waiting for I/O completion
    pub total_percent: f64,   // Total CPU utilization (user + system + iowait)
}

/// CPU time counters from /proc/stat (Linux)
#[derive(Debug, Clone, Copy, Default)]
struct CpuTime {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuTime {
    /// Parse /proc/stat line: "cpu USER NICE SYSTEM IDLE IOWAIT IRQ SOFTIRQ STEAL ..."
    fn from_proc_stat() -> Result<Self> {
        let content = fs::read_to_string("/proc/stat")
            .context("Failed to read /proc/stat")?;
        
        let first_line = content.lines()
            .next()
            .context("Empty /proc/stat")?;
        
        if !first_line.starts_with("cpu ") {
            anyhow::bail!("Invalid /proc/stat format");
        }
        
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 9 {
            anyhow::bail!("Insufficient fields in /proc/stat cpu line");
        }
        
        Ok(CpuTime {
            user:    parts[1].parse().context("Invalid user time")?,
            nice:    parts[2].parse().context("Invalid nice time")?,
            system:  parts[3].parse().context("Invalid system time")?,
            idle:    parts[4].parse().context("Invalid idle time")?,
            iowait:  parts[5].parse().context("Invalid iowait time")?,
            irq:     parts[6].parse().context("Invalid irq time")?,
            softirq: parts[7].parse().context("Invalid softirq time")?,
            steal:   parts[8].parse().context("Invalid steal time")?,
        })
    }
    
    /// Calculate total CPU time (all categories)
    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + 
        self.iowait + self.irq + self.softirq + self.steal
    }
}

/// CPU monitor with configurable sampling interval
pub struct CpuMonitor {
    last_sample: Option<CpuTime>,
    last_sample_time: Instant,
    min_interval: Duration,
}

impl CpuMonitor {
    /// Create a new CPU monitor with 1-second minimum sampling interval
    pub fn new() -> Self {
        Self {
            last_sample: None,
            last_sample_time: Instant::now(),
            min_interval: Duration::from_secs(1),  // Sample every 1 second
        }
    }
    
    /// Get current CPU utilization (samples if interval elapsed)
    /// Returns None if not enough time has passed for accurate measurement
    pub fn sample(&mut self) -> Result<Option<CpuUtilization>> {
        let now = Instant::now();
        let current = CpuTime::from_proc_stat()?;
        
        // First sample - store and return None
        if self.last_sample.is_none() {
            self.last_sample = Some(current);
            self.last_sample_time = now;
            return Ok(None);
        }
        
        let last_time = self.last_sample.unwrap();
        
        // Check if enough time has passed (allow 80% of interval for flexibility)
        let elapsed = now.duration_since(self.last_sample_time);
        if elapsed < self.min_interval * 4 / 5 {
            return Ok(None); // Too soon, skip sample
        }
        
        // Calculate deltas
        let user_delta = current.user.saturating_sub(last_time.user) + 
                        current.nice.saturating_sub(last_time.nice);
        let system_delta = current.system.saturating_sub(last_time.system) + 
                          current.irq.saturating_sub(last_time.irq) + 
                          current.softirq.saturating_sub(last_time.softirq);
        let iowait_delta = current.iowait.saturating_sub(last_time.iowait);
        let total_delta = current.total().saturating_sub(last_time.total());
        
        // Avoid division by zero
        if total_delta == 0 {
            self.last_sample = Some(current);
            self.last_sample_time = now;
            return Ok(Some(CpuUtilization::default()));
        }
        
        // Calculate percentages
        let user_percent = (user_delta as f64 / total_delta as f64) * 100.0;
        let system_percent = (system_delta as f64 / total_delta as f64) * 100.0;
        let iowait_percent = (iowait_delta as f64 / total_delta as f64) * 100.0;
        let total_percent = user_percent + system_percent + iowait_percent;
        
        // Update last sample
        self.last_sample = Some(current);
        self.last_sample_time = now;
        
        Ok(Some(CpuUtilization {
            user_percent,
            system_percent,
            iowait_percent,
            total_percent,
        }))
    }
    
    /// Force an immediate sample regardless of interval
    /// Useful for final measurements at workload completion
    pub fn sample_now(&mut self) -> Result<Option<CpuUtilization>> {
        // Temporarily set last sample to old time to force sampling
        if self.last_sample.is_some() {
            self.last_sample_time = Instant::now() - self.min_interval;
        }
        self.sample()
    }
}

impl Default for CpuMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    
    #[test]
    fn test_cpu_time_parsing() {
        // This test only works on Linux with /proc/stat
        if let Ok(cpu_time) = CpuTime::from_proc_stat() {
            assert!(cpu_time.user > 0);
            assert!(cpu_time.total() > 0);
            println!("CPU time: user={}, system={}, total={}", 
                     cpu_time.user, cpu_time.system, cpu_time.total());
        }
    }
    
    #[test]
    fn test_cpu_monitor_basic() {
        let mut monitor = CpuMonitor::new();
        
        // First sample should return None (establishing baseline)
        if let Ok(result) = monitor.sample() {
            assert!(result.is_none(), "First sample should be None");
        }
        
        // Wait less than interval - should return None
        thread::sleep(Duration::from_millis(crate::constants::CPU_MONITOR_TEST_SLEEP_MS));
        if let Ok(result) = monitor.sample() {
            assert!(result.is_none(), "Too-soon sample should be None");
        }
        
        // Force sample now - should return Some
        if let Ok(Some(util)) = monitor.sample_now() {
            println!("CPU utilization: user={:.1}%, system={:.1}%, iowait={:.1}%, total={:.1}%",
                     util.user_percent, util.system_percent, util.iowait_percent, util.total_percent);
            // Sanity check: percentages should be 0-100
            assert!(util.total_percent >= 0.0 && util.total_percent <= 100.0);
        }
    }
}
