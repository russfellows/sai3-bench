// src/size_generator.rs
//
// Object size generation with support for realistic distributions
//

use anyhow::{anyhow, Context, Result};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, LogNormal, Uniform};
use serde::{Deserialize, Serialize};

/// Specification for object sizes - either fixed or distributed
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SizeSpec {
    /// Fixed size in bytes (backward compatible with old object_size field)
    Fixed(u64),
    
    /// Distribution-based size generation
    Distribution(SizeDistribution),
}

impl SizeSpec {
    /// Get a fixed size if this is a Fixed spec, otherwise None
    pub fn as_fixed(&self) -> Option<u64> {
        match self {
            SizeSpec::Fixed(size) => Some(*size),
            _ => None,
        }
    }
}

/// Distribution configuration for object sizes
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SizeDistribution {
    /// Type of distribution
    #[serde(rename = "type")]
    pub dist_type: DistributionType,
    
    /// Minimum size in bytes (floor)
    #[serde(default)]
    pub min: Option<u64>,
    
    /// Maximum size in bytes (ceiling)
    #[serde(default)]
    pub max: Option<u64>,
    
    /// Distribution-specific parameters
    #[serde(flatten)]
    pub params: DistributionParams,
}

/// Type of size distribution
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DistributionType {
    /// Uniform distribution (evenly distributed between min and max)
    Uniform,
    
    /// Lognormal distribution (realistic - many small, few large)
    Lognormal,
}

/// Parameters specific to distribution types
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DistributionParams {
    /// Mean size for lognormal distribution (in bytes)
    #[serde(default)]
    pub mean: Option<u64>,
    
    /// Standard deviation for lognormal distribution (in bytes)
    #[serde(default)]
    pub std_dev: Option<u64>,
}

/// Generator for object sizes based on a specification
/// 
/// v0.7.9: Uses seeded RNG for deterministic size generation.
/// With the same seed, generates identical size sequences.
pub struct SizeGenerator {
    generator: SizeGeneratorImpl,
    rng: StdRng,
}

enum SizeGeneratorImpl {
    Fixed(u64),
    Uniform {
        dist: Uniform<u64>,
        min: u64,
        max: u64,
    },
    LogNormal {
        dist: LogNormal<f64>,
        min: u64,
        max: u64,
    },
}

impl SizeGenerator {
    /// Create a new size generator from a specification with default seed (0)
    /// 
    /// For deterministic generation, use `new_with_seed()` instead.
    pub fn new(spec: &SizeSpec) -> Result<Self> {
        Self::new_with_seed(spec, 0)
    }
    
    /// Create a new size generator with a specific seed for deterministic generation
    /// 
    /// The same seed with the same spec will always produce the same sequence of sizes.
    /// This enables gap-filling to recreate exact file sizes based on file index.
    pub fn new_with_seed(spec: &SizeSpec, seed: u64) -> Result<Self> {
        let generator = match spec {
            SizeSpec::Fixed(size) => {
                if *size == 0 {
                    return Err(anyhow!("Object size must be greater than 0"));
                }
                SizeGeneratorImpl::Fixed(*size)
            }
            
            SizeSpec::Distribution(dist) => match dist.dist_type {
                DistributionType::Uniform => {
                    let min = dist.min.unwrap_or(1024); // Default 1 KB
                    let max = dist.max.unwrap_or(1048576); // Default 1 MB
                    
                    if min > max {
                        return Err(anyhow!("Uniform distribution: min ({}) > max ({})", min, max));
                    }
                    if min == 0 {
                        return Err(anyhow!("Uniform distribution: min must be > 0"));
                    }
                    
                    let uniform_dist = Uniform::new_inclusive(min, max)?;
                    SizeGeneratorImpl::Uniform {
                        dist: uniform_dist,
                        min,
                        max,
                    }
                }
                
                DistributionType::Lognormal => {
                    let mean = dist.params.mean
                        .ok_or_else(|| anyhow!("Lognormal distribution requires 'mean' parameter"))?;
                    let std_dev = dist.params.std_dev
                        .ok_or_else(|| anyhow!("Lognormal distribution requires 'std_dev' parameter"))?;
                    
                    if mean == 0 {
                        return Err(anyhow!("Lognormal distribution: mean must be > 0"));
                    }
                    if std_dev == 0 {
                        return Err(anyhow!("Lognormal distribution: std_dev must be > 0"));
                    }
                    
                    let min = dist.min.unwrap_or(1); // Default 1 byte
                    let max = dist.max.unwrap_or(u64::MAX); // No upper limit by default
                    
                    if min > max {
                        return Err(anyhow!("Lognormal distribution: min ({}) > max ({})", min, max));
                    }
                    
                    // Convert mean and std_dev to log-space parameters
                    // For lognormal: if X ~ LogNormal(μ, σ), then E[X] = exp(μ + σ²/2)
                    // We want to specify the desired mean and std_dev in linear space
                    let mean_f = mean as f64;
                    let std_dev_f = std_dev as f64;
                    
                    // Calculate log-space parameters from linear-space mean and variance
                    // Var[X] = (exp(σ²) - 1) * exp(2μ + σ²)
                    // Mean[X] = exp(μ + σ²/2)
                    let variance = std_dev_f * std_dev_f;
                    let mean_squared = mean_f * mean_f;
                    
                    // φ² = ln(1 + variance/mean²)
                    let phi_squared = (1.0 + variance / mean_squared).ln();
                    let phi = phi_squared.sqrt();
                    
                    // μ = ln(mean) - φ²/2
                    let mu = mean_f.ln() - phi_squared / 2.0;
                    
                    let lognormal_dist = LogNormal::new(mu, phi)
                        .context("Failed to create lognormal distribution")?;
                    
                    SizeGeneratorImpl::LogNormal {
                        dist: lognormal_dist,
                        min,
                        max,
                    }
                }
            },
        };
        
        Ok(SizeGenerator { 
            generator,
            rng: StdRng::seed_from_u64(seed),
        })
    }
    
    /// Generate a single object size
    /// 
    /// v0.7.9: Takes &mut self to update RNG state for deterministic sequence.
    pub fn generate(&mut self) -> u64 {
        match &self.generator {
            SizeGeneratorImpl::Fixed(size) => *size,
            
            SizeGeneratorImpl::Uniform { dist, .. } => {
                dist.sample(&mut self.rng)
            }
            
            SizeGeneratorImpl::LogNormal { dist, min, max } => {
                // Sample from lognormal and clamp to [min, max]
                // Rejection sampling: keep trying until we get a value in range
                // (usually converges quickly for reasonable parameters)
                loop {
                    let sample = dist.sample(&mut self.rng);
                    let size = sample.round() as u64;
                    
                    if size >= *min && size <= *max {
                        return size;
                    }
                    
                    // Fallback: clamp if we've tried too many times
                    // This prevents infinite loops with unrealistic parameters
                    return size.clamp(*min, *max);
                }
            }
        }
    }
    
    /// Get the expected mean size (useful for logging/debugging)
    /// 
    /// v0.7.9: Takes &mut self for LogNormal case which needs to sample from RNG
    pub fn expected_mean(&mut self) -> u64 {
        match &self.generator {
            SizeGeneratorImpl::Fixed(size) => *size,
            SizeGeneratorImpl::Uniform { min, max, .. } => (min + max) / 2,
            SizeGeneratorImpl::LogNormal { .. } => {
                // For lognormal, the configured mean is the expected value
                // We don't store it separately, so just sample a few times
                let samples: Vec<u64> = (0..100).map(|_| self.generate()).collect();
                samples.iter().sum::<u64>() / samples.len() as u64
            }
        }
    }
    
    /// Get a description of this generator (for logging)
    /// 
    /// v0.7.9: Takes &mut self for LogNormal case which calls expected_mean()
    pub fn description(&mut self) -> String {
        match &self.generator {
            SizeGeneratorImpl::Fixed(size) => format!("fixed {}", human_bytes(*size)),
            SizeGeneratorImpl::Uniform { min, max, .. } => {
                format!("uniform {}-{}", human_bytes(*min), human_bytes(*max))
            }
            SizeGeneratorImpl::LogNormal { min, max, .. } => {
                // Copy values first to avoid borrow issues when calling expected_mean()
                let min_val = *min;
                let max_val = *max;
                let mean = self.expected_mean();
                format!("lognormal (mean ~{}, range {}-{})", 
                    human_bytes(mean), 
                    human_bytes(min_val), 
                    human_bytes(max_val))
            }
        }
    }
}

/// Helper to format bytes in human-readable form
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    
    if unit_idx == 0 {
        format!("{}{}", bytes, UNITS[0])
    } else {
        format!("{:.2}{}", size, UNITS[unit_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_fixed_size() {
        let spec = SizeSpec::Fixed(1048576);
        let mut generator = SizeGenerator::new(&spec).unwrap();
        
        for _ in 0..100 {
            assert_eq!(generator.generate(), 1048576);
        }
        
        assert_eq!(generator.expected_mean(), 1048576);
    }
    
    #[test]
    fn test_uniform_distribution() {
        let spec = SizeSpec::Distribution(SizeDistribution {
            dist_type: DistributionType::Uniform,
            min: Some(1024),
            max: Some(10240),
            params: DistributionParams {
                mean: None,
                std_dev: None,
            },
        });
        
        let mut generator = SizeGenerator::new(&spec).unwrap();
        
        // Generate samples and verify they're in range
        for _ in 0..1000 {
            let size = generator.generate();
            assert!(size >= 1024, "Size {} below minimum", size);
            assert!(size <= 10240, "Size {} above maximum", size);
        }
        
        // Check mean is approximately correct
        let mean = generator.expected_mean();
        assert!(mean >= 5000 && mean <= 6500, "Mean {} outside expected range", mean);
    }
    
    #[test]
    fn test_lognormal_distribution() {
        let spec = SizeSpec::Distribution(SizeDistribution {
            dist_type: DistributionType::Lognormal,
            min: Some(1024),
            max: Some(10485760), // 10 MB
            params: DistributionParams {
                mean: Some(1048576), // 1 MB
                std_dev: Some(524288), // 512 KB
            },
        });
        
        let mut generator = SizeGenerator::new(&spec).unwrap();
        
        // Generate samples and verify they're in range
        let mut samples = Vec::new();
        for _ in 0..1000 {
            let size = generator.generate();
            assert!(size >= 1024, "Size {} below minimum", size);
            assert!(size <= 10485760, "Size {} above maximum", size);
            samples.push(size);
        }
        
        // Verify distribution properties
        let mean: f64 = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        
        // Mean should be reasonably close to 1 MB (allow some variance)
        assert!(mean >= 800_000.0 && mean <= 1_300_000.0, 
            "Mean {} outside expected range for lognormal", mean);
        
        // Most samples should be below the mean (characteristic of lognormal)
        let below_mean = samples.iter().filter(|&&s| (s as f64) < mean).count();
        let ratio = below_mean as f64 / samples.len() as f64;
        assert!(ratio > 0.5, "Lognormal should have >50% samples below mean, got {}", ratio);
    }
    
    #[test]
    fn test_invalid_specs() {
        // Zero size
        let spec = SizeSpec::Fixed(0);
        assert!(SizeGenerator::new(&spec).is_err());
        
        // Min > max for uniform
        let spec = SizeSpec::Distribution(SizeDistribution {
            dist_type: DistributionType::Uniform,
            min: Some(10000),
            max: Some(1000),
            params: DistributionParams { mean: None, std_dev: None },
        });
        assert!(SizeGenerator::new(&spec).is_err());
        
        // Lognormal missing mean
        let spec = SizeSpec::Distribution(SizeDistribution {
            dist_type: DistributionType::Lognormal,
            min: Some(1024),
            max: Some(10240),
            params: DistributionParams { mean: None, std_dev: Some(1024) },
        });
        assert!(SizeGenerator::new(&spec).is_err());
    }
    
    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(500), "500B");
        assert_eq!(human_bytes(1024), "1.00KB");
        assert_eq!(human_bytes(1536), "1.50KB");
        assert_eq!(human_bytes(1048576), "1.00MB");
        assert_eq!(human_bytes(1073741824), "1.00GB");
    }
    
    #[test]
    fn test_deterministic_generation() {
        // Same seed should produce same sequence
        let spec = SizeSpec::Distribution(SizeDistribution {
            dist_type: DistributionType::Uniform,
            min: Some(1024),
            max: Some(10240),
            params: DistributionParams { mean: None, std_dev: None },
        });
        
        let mut gen1 = SizeGenerator::new_with_seed(&spec, 42).unwrap();
        let mut gen2 = SizeGenerator::new_with_seed(&spec, 42).unwrap();
        
        // Generate 100 sizes from each - should be identical
        for _ in 0..100 {
            let size1 = gen1.generate();
            let size2 = gen2.generate();
            assert_eq!(size1, size2, "Same seed should produce same sequence");
        }
        
        // Different seed should produce different sequence
        let mut gen3 = SizeGenerator::new_with_seed(&spec, 99).unwrap();
        let _size_from_gen1 = gen1.generate();
        let _size_from_gen3 = gen3.generate();
        
        // Very unlikely to match (though technically possible)
        // Run this multiple times to be confident
        let mut differences = 0;
        for _ in 0..50 {
            if gen1.generate() != gen3.generate() {
                differences += 1;
            }
        }
        assert!(differences > 40, "Different seeds should produce different sequences");
    }
}
