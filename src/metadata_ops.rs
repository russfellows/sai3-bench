// src/metadata_ops.rs
//
// Metadata operations trait and types for cross-backend testing
//
// Provides unified abstraction for filesystem-style metadata operations
// across all storage backends (file://, direct://, s3://, gs://, az://)

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::SystemTime;

/// Metadata operations that can be tested across different storage backends
/// 
/// Implementations vary by backend:
/// - File/DirectIO: Full POSIX semantics (native mkdir, rename, chmod, etc.)
/// - Cloud (S3/GCS/Azure): Emulated operations (prefix markers, copy+delete for move)
#[async_trait]
pub trait MetadataOperations: Send + Sync {
    /// Create a directory or prefix (semantics vary by backend)
    /// 
    /// - File/DirectIO: Creates actual directory with mkdir()
    /// - Cloud: Creates empty marker object at path/.keep
    async fn mkdir(&self, path: &str) -> Result<()>;
    
    /// Remove an empty directory or prefix
    /// 
    /// - File/DirectIO: Removes directory with rmdir() (must be empty unless recursive)
    /// - Cloud: Deletes all objects under prefix
    async fn rmdir(&self, path: &str, recursive: bool) -> Result<()>;
    
    /// Copy an object/file to a new location
    /// 
    /// - File/DirectIO: Copies file data with std::fs::copy()
    /// - Cloud: Uses native server-side copy (S3 CopyObject, GCS rewriteFrom, Azure Copy Blob)
    async fn copy(&self, source: &str, destination: &str) -> Result<u64>;
    
    /// Move/rename an object/file
    /// 
    /// - File/DirectIO: Atomic rename with std::fs::rename()
    /// - Cloud: Emulated with copy + delete (not atomic)
    async fn move_obj(&self, source: &str, destination: &str) -> Result<u64>;
    
    /// Set attributes (chmod/chown for POSIX, metadata for cloud)
    /// 
    /// - File/DirectIO: Sets POSIX permissions/ownership
    /// - Cloud: Updates object metadata (content-type, custom headers)
    async fn set_attr(&self, path: &str, attrs: &MetadataAttributes) -> Result<()>;
    
    /// Get attributes (stat for POSIX, HEAD for cloud)
    /// 
    /// Returns unified metadata structure with backend-specific fields populated
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata>;
    
    /// Test access permissions
    /// 
    /// - File/DirectIO: Uses access() syscall to test permissions
    /// - Cloud: Tests existence with HEAD request
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool>;
}

/// Unified metadata representation across backends
/// 
/// For cloud backends (S3/GCS/Azure), populated from ObjectStore::stat()
/// For POSIX backends (file://,direct://), populated from filesystem metadata
#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    /// Object/file path
    pub path: String,
    
    /// Size in bytes
    pub size: u64,
    
    /// Last modified timestamp
    pub modified: Option<SystemTime>,
    
    // Standard HTTP/Cloud metadata (all backends can provide some of these)
    
    /// MIME content type (e.g., "application/json", "text/plain")
    pub content_type: Option<String>,
    
    /// Cache-Control header (HTTP caching directives)
    pub cache_control: Option<String>,
    
    /// Content-Encoding header (e.g., "gzip", "br")
    pub content_encoding: Option<String>,
    
    /// Content-Language header (e.g., "en-US")
    pub content_language: Option<String>,
    
    /// Content-Disposition header (download filename hint)
    pub content_disposition: Option<String>,
    
    /// ETag/version identifier (for conditional requests)
    pub etag: Option<String>,
    
    /// Expiration time (HTTP Expires header)
    pub expires: Option<String>,
    
    // POSIX-specific fields (file://, direct://)
    
    /// File permissions as mode bits (e.g., 0o644)
    pub permissions: Option<u32>,
    
    /// Owner user ID
    pub owner_uid: Option<u32>,
    
    /// Owner group ID
    pub owner_gid: Option<u32>,
    
    // Cloud-specific fields (s3://, gs://, az://)
    
    /// Storage class/tier (e.g., "STANDARD", "GLACIER", "NEARLINE", "ARCHIVE")
    pub storage_class: Option<String>,
    
    /// Server-side encryption type (e.g., "AES256", "aws:kms")
    pub server_side_encryption: Option<String>,
    
    /// KMS key ID (for SSE-KMS encryption)
    pub ssekms_key_id: Option<String>,
    
    /// Object version ID (for versioned buckets)
    pub version_id: Option<String>,
    
    /// Replication status (e.g., "COMPLETED", "PENDING")
    pub replication_status: Option<String>,
    
    /// Custom metadata key-value pairs (user-defined, x-amz-meta-* for S3)
    pub custom_metadata: HashMap<String, String>,
}

/// Attributes to set on an object/file
#[derive(Debug, Clone)]
pub enum MetadataAttributes {
    /// POSIX permissions and ownership (file://, direct://)
    Posix {
        /// File mode bits (e.g., 0o644 for rw-r--r--)
        mode: Option<u32>,
        
        /// Owner user ID (requires elevated privileges)
        uid: Option<u32>,
        
        /// Owner group ID (requires elevated privileges)
        gid: Option<u32>,
    },
    
    /// Cloud storage metadata (s3://, gs://, az://)
    /// 
    /// These map to HTTP headers and cloud-specific metadata:
    /// - S3: x-amz-meta-* for custom, standard headers for others
    /// - GCS: x-goog-meta-* for custom, standard headers for others  
    /// - Azure: x-ms-meta-* for custom, standard headers for others
    Cloud {
        /// MIME content type (e.g., "application/json")
        content_type: Option<String>,
        
        /// Cache control header (e.g., "max-age=3600, public")
        cache_control: Option<String>,
        
        /// Content encoding (e.g., "gzip")
        content_encoding: Option<String>,
        
        /// Content language (e.g., "en-US")
        content_language: Option<String>,
        
        /// Content disposition (e.g., "attachment; filename=file.txt")
        content_disposition: Option<String>,
        
        /// Expiration time (HTTP Expires header)
        expires: Option<String>,
        
        /// Storage class/tier (e.g., "GLACIER", "NEARLINE", "ARCHIVE")
        /// Note: Changing storage class may require object copy
        storage_class: Option<String>,
        
        /// Custom metadata key-value pairs (x-amz-meta-*, x-goog-meta-*, x-ms-meta-*)
        custom: HashMap<String, String>,
    },
}

/// Access permission test modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    /// Test read permission
    Read,
    
    /// Test write permission
    Write,
    
    /// Test execute permission
    Execute,
    
    /// Test existence only
    Exists,
}

impl AccessMode {
    /// Convert to POSIX access() mode bits
    pub fn to_posix_mode(&self) -> i32 {
        match self {
            AccessMode::Read => libc::R_OK,
            AccessMode::Write => libc::W_OK,
            AccessMode::Execute => libc::X_OK,
            AccessMode::Exists => libc::F_OK,
        }
    }
}

impl std::fmt::Display for AccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessMode::Read => write!(f, "read"),
            AccessMode::Write => write!(f, "write"),
            AccessMode::Execute => write!(f, "execute"),
            AccessMode::Exists => write!(f, "exists"),
        }
    }
}

impl std::str::FromStr for AccessMode {
    type Err = anyhow::Error;
    
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "read" | "r" => Ok(AccessMode::Read),
            "write" | "w" => Ok(AccessMode::Write),
            "execute" | "x" => Ok(AccessMode::Execute),
            "exists" | "e" | "f" => Ok(AccessMode::Exists),
            _ => Err(anyhow::anyhow!("Invalid access mode: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_access_mode_to_posix() {
        assert_eq!(AccessMode::Read.to_posix_mode(), libc::R_OK);
        assert_eq!(AccessMode::Write.to_posix_mode(), libc::W_OK);
        assert_eq!(AccessMode::Execute.to_posix_mode(), libc::X_OK);
        assert_eq!(AccessMode::Exists.to_posix_mode(), libc::F_OK);
    }
    
    #[test]
    fn test_access_mode_from_str() {
        assert_eq!("read".parse::<AccessMode>().unwrap(), AccessMode::Read);
        assert_eq!("r".parse::<AccessMode>().unwrap(), AccessMode::Read);
        assert_eq!("write".parse::<AccessMode>().unwrap(), AccessMode::Write);
        assert_eq!("w".parse::<AccessMode>().unwrap(), AccessMode::Write);
        assert_eq!("execute".parse::<AccessMode>().unwrap(), AccessMode::Execute);
        assert_eq!("x".parse::<AccessMode>().unwrap(), AccessMode::Execute);
        assert_eq!("exists".parse::<AccessMode>().unwrap(), AccessMode::Exists);
        assert_eq!("e".parse::<AccessMode>().unwrap(), AccessMode::Exists);
        
        assert!("invalid".parse::<AccessMode>().is_err());
    }
    
    #[test]
    fn test_access_mode_display() {
        assert_eq!(format!("{}", AccessMode::Read), "read");
        assert_eq!(format!("{}", AccessMode::Write), "write");
        assert_eq!(format!("{}", AccessMode::Execute), "execute");
        assert_eq!(format!("{}", AccessMode::Exists), "exists");
    }
}
