//! Pre-flight validation framework for sai3-bench
//!
//! This module provides comprehensive validation of filesystem access, object storage
//! connectivity, and system resources before executing workloads. It helps catch
//! configuration errors early with actionable error messages.
//!
//! # Architecture
//!
//! Pre-flight validation runs as a separate phase before PREPARE:
//! 1. Controller sends PRE_FLIGHT request to all agents
//! 2. Each agent validates its local environment
//! 3. Agents report validation results back to controller
//! 4. Controller aggregates and displays results
//! 5. If any agent fails validation, workload is aborted
//!
//! # Progressive Testing
//!
//! Filesystem: stat → list → read → write → mkdir → delete
//! Object Storage: head → list → get → put → delete
//!
//! Tests proceed from least to most invasive, providing detailed diagnostics
//! at each step.

pub mod filesystem;
pub mod object_storage;
pub mod distributed; // Distributed configuration validation (v0.8.23+)

use std::fmt;

/// Type of error encountered during validation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorType {
    /// Invalid credentials or missing authentication
    Authentication,
    /// Access denied or insufficient permissions
    Permission,
    /// DNS, connectivity, or timeout issues
    Network,
    /// Invalid configuration or missing fields
    Configuration,
    /// Insufficient memory, disk space, or file descriptors
    Resource,
    /// OS errors, mount failures, or system issues
    System,
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorType::Authentication => write!(f, "Authentication"),
            ErrorType::Permission => write!(f, "Permission"),
            ErrorType::Network => write!(f, "Network"),
            ErrorType::Configuration => write!(f, "Configuration"),
            ErrorType::Resource => write!(f, "Resource"),
            ErrorType::System => write!(f, "System"),
        }
    }
}

/// Severity level of validation result
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResultLevel {
    /// Test passed successfully
    Success,
    /// Informational message (e.g., mount type detected)
    Info,
    /// Potential issue but not fatal
    Warning,
    /// Fatal issue - must fix before proceeding
    Error,
}

impl fmt::Display for ResultLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResultLevel::Success => write!(f, "SUCCESS"),
            ResultLevel::Info => write!(f, "INFO"),
            ResultLevel::Warning => write!(f, "WARNING"),
            ResultLevel::Error => write!(f, "ERROR"),
        }
    }
}

/// Result of a single validation test
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Severity level
    pub level: ResultLevel,
    /// Error type (None for Success)
    pub error_type: Option<ErrorType>,
    /// What happened or what was detected
    pub message: String,
    /// How to fix it or additional context
    pub suggestion: String,
    /// Technical details (uid/gid, stack trace, etc.)
    pub details: Option<String>,
    /// Test phase identifier (e.g., "stat", "list", "read")
    pub test_phase: String,
}

impl ValidationResult {
    /// Create an error result
    pub fn error(
        error_type: ErrorType,
        test_phase: impl Into<String>,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            level: ResultLevel::Error,
            error_type: Some(error_type),
            message: message.into(),
            suggestion: suggestion.into(),
            details: None,
            test_phase: test_phase.into(),
        }
    }

    /// Create a warning result
    pub fn warning(
        error_type: ErrorType,
        test_phase: impl Into<String>,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            level: ResultLevel::Warning,
            error_type: Some(error_type),
            message: message.into(),
            suggestion: suggestion.into(),
            details: None,
            test_phase: test_phase.into(),
        }
    }

    /// Create an info result
    pub fn info(
        error_type: ErrorType,
        test_phase: impl Into<String>,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            level: ResultLevel::Info,
            error_type: Some(error_type),
            message: message.into(),
            suggestion: suggestion.into(),
            details: None,
            test_phase: test_phase.into(),
        }
    }

    /// Create a success result
    pub fn success(test_phase: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: ResultLevel::Success,
            error_type: None,
            message: message.into(),
            suggestion: String::new(),
            details: None,
            test_phase: test_phase.into(),
        }
    }

    /// Add technical details to this result
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Get icon for this result level
    pub fn icon(&self) -> &'static str {
        match self.level {
            ResultLevel::Success => "✅",
            ResultLevel::Info => "ℹ️",
            ResultLevel::Warning => "⚠️",
            ResultLevel::Error => "❌",
        }
    }

    /// Get icon for error type
    pub fn type_icon(&self) -> &'static str {
        match self.error_type {
            Some(ErrorType::Authentication) => "🔐",
            Some(ErrorType::Permission) => "🚫",
            Some(ErrorType::Network) => "🌐",
            Some(ErrorType::Configuration) => "⚙️",
            Some(ErrorType::Resource) => "💾",
            Some(ErrorType::System) => "⚠️",
            None => "",
        }
    }
}

/// Summary of all validation results
#[derive(Debug, Clone)]
pub struct ValidationSummary {
    /// All validation results
    pub results: Vec<ValidationResult>,
}

impl ValidationSummary {
    /// Create a new validation summary
    pub fn new(results: Vec<ValidationResult>) -> Self {
        Self { results }
    }

    /// Check if any errors were encountered
    pub fn has_errors(&self) -> bool {
        self.results.iter().any(|r| r.level == ResultLevel::Error)
    }

    /// Check if any warnings were encountered
    pub fn has_warnings(&self) -> bool {
        self.results
            .iter()
            .any(|r| r.level == ResultLevel::Warning)
    }

    /// Count total errors
    pub fn error_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.level == ResultLevel::Error)
            .count()
    }

    /// Count total warnings
    pub fn warning_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.level == ResultLevel::Warning)
            .count()
    }

    /// Count total info messages
    pub fn info_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.level == ResultLevel::Info)
            .count()
    }

    /// Count total success messages
    pub fn success_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.level == ResultLevel::Success)
            .count()
    }

    /// Check if validation passed (no errors, warnings OK)
    pub fn passed(&self) -> bool {
        !self.has_errors()
    }

    /// Display results to console
    pub fn display(&self, agent_id: Option<&str>) {
        let prefix = agent_id
            .map(|id| format!("Agent {}: ", id))
            .unwrap_or_default();

        println!(
            "\n📊 {}Pre-flight Results: {} errors, {} warnings, {} info",
            prefix,
            self.error_count(),
            self.warning_count(),
            self.info_count()
        );

        for result in &self.results {
            self.display_result(result);
        }
    }

    /// Display a single validation result
    fn display_result(&self, result: &ValidationResult) {
        let icon = result.icon();
        let type_icon = result.type_icon();

        println!(
            "   {} {} [{}] {}",
            icon, result.level, result.test_phase, result.message
        );

        if !result.suggestion.is_empty() {
            println!("      {} 💡 {}", type_icon, result.suggestion);
        }

        if let Some(ref details) = result.details {
            println!("      📋 Details: {}", details);
        }
    }
}

impl Default for ValidationSummary {
    fn default() -> Self {
        Self {
            results: Vec::new(),
        }
    }
}

/// Display validation results in a consistent format for both standalone and distributed modes
/// Returns a tuple of (passed: bool, error_count: usize, warning_count: usize)
pub fn display_validation_results(
    results: &[ValidationResult],
    agent_name: Option<&str>,
) -> (bool, usize, usize) {
    let mut error_count = 0;
    let mut warning_count = 0;
    
    for result in results {
        if result.level == ResultLevel::Error {
            error_count += 1;
        } else if result.level == ResultLevel::Warning {
            warning_count += 1;
        }
        
        let icon = match result.level {
            ResultLevel::Success => "✓",
            ResultLevel::Info => "ℹ",
            ResultLevel::Warning => "⚠",
            ResultLevel::Error => "✗",
        };
        
        let error_tag = if let Some(ref err_type) = result.error_type {
            match err_type {
                ErrorType::Authentication => "[AUTH]",
                ErrorType::Permission => "[PERM]",
                ErrorType::Network => "[NET]",
                ErrorType::Configuration => "[CONFIG]",
                ErrorType::Resource => "[RESOURCE]",
                ErrorType::System => "[SYSTEM]",
            }
        } else {
            "[UNKNOWN]"
        };
        
        if let Some(agent) = agent_name {
            println!("  {} {} {} (agent: {})", icon, error_tag, result.message, agent);
        } else {
            println!("  {} {} {}", icon, error_tag, result.message);
        }
        
        if !result.suggestion.is_empty() {
            println!("      → {}", result.suggestion);
        }
        
        if let Some(ref details) = result.details {
            if !details.is_empty() {
                println!("      Details: {}", details);
            }
        }
    }
    
    let passed = error_count == 0;
    (passed, error_count, warning_count)
}
