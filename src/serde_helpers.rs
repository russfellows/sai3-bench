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
            let cleaned = s.replace(',', "")
                .replace('_', "")
                .replace(' ', "");
            
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
            let cleaned = s.replace(',', "")
                .replace('_', "")
                .replace(' ', "");
            
            cleaned.parse::<usize>()
                .map_err(|e| serde::de::Error::custom(format!("Invalid usize: {} (from '{}')", e, s)))
        }
        _ => Err(serde::de::Error::custom("Expected number or string")),
    }
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
}
