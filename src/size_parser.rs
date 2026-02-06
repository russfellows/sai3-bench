//! Size string parsing utilities
//!
//! Supports both decimal (KB, MB, GB, TB) and binary (KiB, MiB, GiB, TiB) suffixes.
//! Also supports case-insensitive shortcuts (k, m, g, t).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Deserializer};

/// Parse a size string into bytes
///
/// Supports:
/// - Powers of 10: KB, MB, GB, TB (or k, m, g, t)
/// - Powers of 2: KiB, MiB, GiB, TiB (or Ki, Mi, Gi, Ti)
/// - Raw numbers: "1048576" → 1048576 bytes
///
/// Examples:
/// - "8MB" → 8,000,000 bytes
/// - "8MiB" → 8,388,608 bytes
/// - "1.5GB" → 1,500,000,000 bytes
/// - "1k" → 1,000 bytes
/// - "1Ki" → 1,024 bytes
pub fn parse_size(input: &str) -> Result<u64> {
    let input = input.trim();
    
    // Try parsing as raw number first
    if let Ok(num) = input.parse::<u64>() {
        return Ok(num);
    }
    
    // Try parsing as float with suffix
    let (number_part, suffix) = split_number_suffix(input)?;
    
    let value: f64 = number_part.parse()
        .map_err(|_| anyhow!("Invalid number: {}", number_part))?;
    
    if value < 0.0 {
        return Err(anyhow!("Size cannot be negative: {}", input));
    }
    
    let multiplier = parse_suffix(suffix)?;
    let bytes = (value * multiplier as f64).round() as u64;
    
    Ok(bytes)
}

/// Split input into number and suffix parts
fn split_number_suffix(input: &str) -> Result<(&str, &str)> {
    // Find where the suffix starts (first non-digit, non-dot character)
    let suffix_start = input.find(|c: char| !c.is_ascii_digit() && c != '.')
        .ok_or_else(|| anyhow!("No suffix found in: {}", input))?;
    
    let number_part = &input[..suffix_start];
    let suffix = &input[suffix_start..];
    
    if number_part.is_empty() {
        return Err(anyhow!("No number found in: {}", input));
    }
    
    Ok((number_part, suffix))
}

/// Parse suffix to byte multiplier
fn parse_suffix(suffix: &str) -> Result<u64> {
    let suffix_upper = suffix.to_uppercase();
    
    match suffix_upper.as_str() {
        // Powers of 10 (decimal)
        "K" | "KB" => Ok(1_000),
        "M" | "MB" => Ok(1_000_000),
        "G" | "GB" => Ok(1_000_000_000),
        "T" | "TB" => Ok(1_000_000_000_000),
        
        // Powers of 2 (binary)
        "KI" | "KIB" => Ok(1_024),
        "MI" | "MIB" => Ok(1_048_576),
        "GI" | "GIB" => Ok(1_073_741_824),
        "TI" | "TIB" => Ok(1_099_511_627_776),
        
        _ => Err(anyhow!("Unknown size suffix: {}. Supported: k/KB/KiB, m/MB/MiB, g/GB/GiB, t/TB/TiB", suffix)),
    }
}

/// Deserialize a size value that can be either a number or a string with suffix
pub fn deserialize_size<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SizeValue {
        Number(u64),
        String(String),
    }
    
    match Option::<SizeValue>::deserialize(deserializer)? {
        None => Ok(None),
        Some(SizeValue::Number(n)) => Ok(Some(n)),
        Some(SizeValue::String(s)) => {
            parse_size(&s).map(Some).map_err(serde::de::Error::custom)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_raw_numbers() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1048576").unwrap(), 1048576);
        assert_eq!(parse_size("0").unwrap(), 0);
    }
    
    #[test]
    fn test_parse_decimal_kb() {
        assert_eq!(parse_size("1KB").unwrap(), 1_000);
        assert_eq!(parse_size("1kb").unwrap(), 1_000);
        assert_eq!(parse_size("1K").unwrap(), 1_000);
        assert_eq!(parse_size("1k").unwrap(), 1_000);
        assert_eq!(parse_size("10k").unwrap(), 10_000);
    }
    
    #[test]
    fn test_parse_decimal_mb() {
        assert_eq!(parse_size("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_size("1mb").unwrap(), 1_000_000);
        assert_eq!(parse_size("1M").unwrap(), 1_000_000);
        assert_eq!(parse_size("1m").unwrap(), 1_000_000);
        assert_eq!(parse_size("8MB").unwrap(), 8_000_000);
    }
    
    #[test]
    fn test_parse_decimal_gb() {
        assert_eq!(parse_size("1GB").unwrap(), 1_000_000_000);
        assert_eq!(parse_size("1gb").unwrap(), 1_000_000_000);
        assert_eq!(parse_size("1G").unwrap(), 1_000_000_000);
        assert_eq!(parse_size("1g").unwrap(), 1_000_000_000);
    }
    
    #[test]
    fn test_parse_decimal_tb() {
        assert_eq!(parse_size("1TB").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_size("1tb").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_size("1T").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_size("1t").unwrap(), 1_000_000_000_000);
    }
    
    #[test]
    fn test_parse_binary_kib() {
        assert_eq!(parse_size("1KiB").unwrap(), 1_024);
        assert_eq!(parse_size("1kib").unwrap(), 1_024);
        assert_eq!(parse_size("1Ki").unwrap(), 1_024);
        assert_eq!(parse_size("1ki").unwrap(), 1_024);
    }
    
    #[test]
    fn test_parse_binary_mib() {
        assert_eq!(parse_size("1MiB").unwrap(), 1_048_576);
        assert_eq!(parse_size("1mib").unwrap(), 1_048_576);
        assert_eq!(parse_size("1Mi").unwrap(), 1_048_576);
        assert_eq!(parse_size("1mi").unwrap(), 1_048_576);
        assert_eq!(parse_size("8MiB").unwrap(), 8_388_608);
    }
    
    #[test]
    fn test_parse_binary_gib() {
        assert_eq!(parse_size("1GiB").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1gib").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1Gi").unwrap(), 1_073_741_824);
    }
    
    #[test]
    fn test_parse_binary_tib() {
        assert_eq!(parse_size("1TiB").unwrap(), 1_099_511_627_776);
        assert_eq!(parse_size("1tib").unwrap(), 1_099_511_627_776);
        assert_eq!(parse_size("1Ti").unwrap(), 1_099_511_627_776);
    }
    
    #[test]
    fn test_parse_fractional() {
        assert_eq!(parse_size("1.5MB").unwrap(), 1_500_000);
        assert_eq!(parse_size("0.5GB").unwrap(), 500_000_000);
        assert_eq!(parse_size("2.5MiB").unwrap(), 2_621_440);
    }
    
    #[test]
    fn test_parse_whitespace() {
        assert_eq!(parse_size("  8MB  ").unwrap(), 8_000_000);
        assert_eq!(parse_size(" 1.5GiB ").unwrap(), 1_610_612_736);
    }
    
    #[test]
    fn test_realistic_sizes() {
        // Your example: 8 MB mean
        assert_eq!(parse_size("8MB").unwrap(), 8_000_000);
        // Binary equivalent
        assert_eq!(parse_size("8MiB").unwrap(), 8_388_608);
        
        // 1 MB std_dev
        assert_eq!(parse_size("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_size("1MiB").unwrap(), 1_048_576);
        
        // 16 MB max
        assert_eq!(parse_size("16MB").unwrap(), 16_000_000);
        assert_eq!(parse_size("16MiB").unwrap(), 16_777_216);
    }
    
    #[test]
    fn test_errors() {
        assert!(parse_size("").is_err());
        assert!(parse_size("MB").is_err());
        assert!(parse_size("-1MB").is_err());
        assert!(parse_size("1XB").is_err());
        assert!(parse_size("abc").is_err());
    }
}
