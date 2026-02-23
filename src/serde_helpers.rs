// Serde deserializers for numeric types with optional thousand separator support
// Allows users to write more readable YAML: count: 64,032,768 instead of count: 64032768

use serde::{Deserialize, Deserializer};

/// Deserialize u64 with optional thousand separators (comma, underscore, space)
/// Examples: "64032768", "64,032,768", "64_032_768", "64 032 768"
pub fn deserialize_u64_with_separators<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    // First try to deserialize as a plain u64 (most common case)
    let value = serde_yaml::Value::deserialize(deserializer)?;
    
    match value {
        serde_yaml::Value::Number(n) => {
            // Direct number - no separators
            n.as_u64()
                .ok_or_else(|| serde::de::Error::custom("Expected u64"))
        }
        serde_yaml::Value::String(s) => {
            // String representation - might have separators
            let cleaned = s.replace([',', '_', ' '], "");
            
            cleaned.parse::<u64>()
                .map_err(|e| serde::de::Error::custom(format!("Invalid u64: {} (from '{}')", e, s)))
        }
        _ => Err(serde::de::Error::custom("Expected number or string")),
    }
}

/// Deserialize usize with optional thousand separators
/// Examples: "193", "1,234", "331_776"
pub fn deserialize_usize_with_separators<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    
    match value {
        serde_yaml::Value::Number(n) => {
            n.as_u64()
                .and_then(|u| usize::try_from(u).ok())
                .ok_or_else(|| serde::de::Error::custom("Expected usize"))
        }
        serde_yaml::Value::String(s) => {
            let cleaned = s.replace([',', '_', ' '], "");
            
            cleaned.parse::<usize>()
                .map_err(|e| serde::de::Error::custom(format!("Invalid usize: {} (from '{}')", e, s)))
        }
        _ => Err(serde::de::Error::custom("Expected number or string")),
    }
}

/// Deserialize duration in seconds with optional time unit suffixes
/// Examples: 
///   - Plain integers: 60, 300, 7200 (interpreted as seconds)
///   - With units: "60s", "5m", "2h", "1d"
/// 
/// Supported units:
///   - s: seconds
///   - m: minutes (60 seconds)
///   - h: hours (3600 seconds)
///   - d: days (86400 seconds)
pub fn deserialize_duration_seconds<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    
    match value {
        serde_yaml::Value::Number(n) => {
            // Plain number - interpret as seconds
            n.as_u64()
                .ok_or_else(|| serde::de::Error::custom("Expected u64 for duration"))
        }
        serde_yaml::Value::String(s) => {
            parse_duration_string(&s)
                .map_err(serde::de::Error::custom)
        }
        _ => Err(serde::de::Error::custom("Expected number or string for duration")),
    }
}

/// Parse a duration string to seconds
/// Supports: "60s", "5m", "2h", "1d", or plain numbers like "300"
fn parse_duration_string(s: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    
    // Try parsing as plain number first (backward compatibility)
    if let Ok(n) = trimmed.parse::<u64>() {
        return Ok(n);
    }
    
    // Check for time unit suffix
    if trimmed.is_empty() {
        return Err("Empty duration string".to_string());
    }
    
    // Extract numeric part and unit
    let (num_str, unit) = if let Some(stripped) = trimmed.strip_suffix('s') {
        (stripped, 's')
    } else if let Some(stripped) = trimmed.strip_suffix('m') {
        (stripped, 'm')
    } else if let Some(stripped) = trimmed.strip_suffix('h') {
        (stripped, 'h')
    } else if let Some(stripped) = trimmed.strip_suffix('d') {
        (stripped, 'd')
    } else {
        return Err(format!("Invalid duration format '{}': must be a number or end with s/m/h/d", s));
    };
    
    // Parse the numeric part
    let num = num_str.trim().parse::<u64>()
        .map_err(|e| format!("Invalid number in duration '{}': {}", s, e))?;
    
    // Convert to seconds based on unit
    let seconds = match unit {
        's' => num,
        'm' => num.checked_mul(60)
            .ok_or_else(|| format!("Duration overflow: {} minutes is too large", num))?,
        'h' => num.checked_mul(3600)
            .ok_or_else(|| format!("Duration overflow: {} hours is too large", num))?,
        'd' => num.checked_mul(86400)
            .ok_or_else(|| format!("Duration overflow: {} days is too large", num))?,
        _ => unreachable!(),
    };
    
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    
    #[derive(Debug, Deserialize, PartialEq)]
    struct TestU64 {
        #[serde(deserialize_with = "deserialize_u64_with_separators")]
        value: u64,
    }
    
    #[derive(Debug, Deserialize, PartialEq)]
    struct TestUsize {
        #[serde(deserialize_with = "deserialize_usize_with_separators")]
        value: usize,
    }
    
    #[derive(Debug, Deserialize, PartialEq)]
    struct TestDuration {
        #[serde(deserialize_with = "deserialize_duration_seconds")]
        timeout: u64,
    }
    
    #[test]
    fn test_u64_plain_number() {
        let yaml = "value: 64032768";
        let result: TestU64 = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 64032768);
    }
    
    #[test]
    fn test_u64_with_commas() {
        let yaml = "value: \"64,032,768\"";
        let result: TestU64 = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 64032768);
    }
    
    #[test]
    fn test_u64_with_underscores() {
        let yaml = "value: \"64_032_768\"";
        let result: TestU64 = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 64032768);
    }
    
    #[test]
    fn test_u64_with_spaces() {
        let yaml = "value: \"64 032 768\"";
        let result: TestU64 = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 64032768);
    }
    
    #[test]
    fn test_usize_plain_number() {
        let yaml = "value: 193";
        let result: TestUsize = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 193);
    }
    
    #[test]
    fn test_usize_with_commas() {
        let yaml = "value: \"331,776\"";
        let result: TestUsize = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.value, 331776);
    }
    
    // Duration deserializer tests
    
    #[test]
    fn test_duration_plain_seconds() {
        let yaml = "timeout: 300";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 300);
    }
    
    #[test]
    fn test_duration_seconds_suffix() {
        let yaml = "timeout: \"60s\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 60);
    }
    
    #[test]
    fn test_duration_minutes() {
        let yaml = "timeout: \"5m\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 300);  // 5 * 60
    }
    
    #[test]
    fn test_duration_hours() {
        let yaml = "timeout: \"2h\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 7200);  // 2 * 3600
    }
    
    #[test]
    fn test_duration_days() {
        let yaml = "timeout: \"1d\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 86400);  // 1 * 86400
    }
    
    #[test]
    fn test_duration_zero() {
        let yaml = "timeout: \"0s\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 0);
    }
    
    #[test]
    fn test_duration_large_minutes() {
        let yaml = "timeout: \"120m\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 7200);  // 120 * 60
    }
    
    #[test]
    fn test_duration_plain_string_number() {
        // Backward compatibility: plain string numbers work
        let yaml = "timeout: \"600\"";
        let result: TestDuration = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(result.timeout, 600);
    }
    
    #[test]
    fn test_duration_invalid_unit() {
        let yaml = "timeout: \"5x\"";
        let result: Result<TestDuration, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }
    
    #[test]
    fn test_duration_invalid_number() {
        let yaml = "timeout: \"abc\"";
        let result: Result<TestDuration, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }
}
