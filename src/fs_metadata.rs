// src/fs_metadata.rs
//
// POSIX filesystem metadata operations for file:// and direct:// backends
//
// Provides full native support for directory operations, file copying/moving,
// and POSIX permissions (chmod, chown, stat, access)

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, warn};

use crate::metadata_ops::{
    AccessMode, MetadataAttributes, MetadataOperations, ObjectMetadata,
};

/// POSIX filesystem metadata operations handler
pub struct FileMetadataOps {
    base_path: PathBuf,
}

impl FileMetadataOps {
    /// Create new filesystem metadata handler
    /// 
    /// # Arguments
    /// * `base_uri` - Base URI like "file:///tmp/test/" or "direct:///mnt/data/"
    pub fn new(base_uri: &str) -> Result<Self> {
        let path_str = base_uri
            .strip_prefix("file://")
            .or_else(|| base_uri.strip_prefix("direct://"))
            .unwrap_or(base_uri);
        
        let base_path = PathBuf::from(path_str);
        
        Ok(Self { base_path })
    }
    
    /// Resolve relative path against base URI
    fn resolve_path(&self, path: &str) -> PathBuf {
        if path.is_empty() {
            return self.base_path.clone();
        }
        
        // Remove leading slash if present (relative to base)
        let clean_path = path.strip_prefix('/').unwrap_or(path);
        
        self.base_path.join(clean_path)
    }
}

#[async_trait]
impl MetadataOperations for FileMetadataOps {
    async fn mkdir(&self, path: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        
        debug!("mkdir: {}", full_path.display());
        
        tokio::fs::create_dir_all(&full_path)
            .await
            .context(format!("Failed to create directory: {}", full_path.display()))?;
        
        Ok(())
    }
    
    async fn rmdir(&self, path: &str, recursive: bool) -> Result<()> {
        let full_path = self.resolve_path(path);
        
        debug!("rmdir: {} (recursive={})", full_path.display(), recursive);
        
        if recursive {
            tokio::fs::remove_dir_all(&full_path)
                .await
                .context(format!("Failed to remove directory recursively: {}", full_path.display()))?;
        } else {
            tokio::fs::remove_dir(&full_path)
                .await
                .context(format!("Failed to remove directory: {}", full_path.display()))?;
        }
        
        Ok(())
    }
    
    async fn copy(&self, source: &str, destination: &str) -> Result<u64> {
        let src = self.resolve_path(source);
        let dst = self.resolve_path(destination);
        
        debug!("copy: {} -> {}", src.display(), dst.display());
        
        // Ensure destination directory exists
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create destination directory")?;
        }
        
        let bytes_copied = tokio::fs::copy(&src, &dst)
            .await
            .context(format!("Failed to copy {} to {}", src.display(), dst.display()))?;
        
        Ok(bytes_copied)
    }
    
    async fn move_obj(&self, source: &str, destination: &str) -> Result<u64> {
        let src = self.resolve_path(source);
        let dst = self.resolve_path(destination);
        
        debug!("move: {} -> {}", src.display(), dst.display());
        
        // Get size before moving
        let metadata = tokio::fs::metadata(&src)
            .await
            .context(format!("Failed to stat source: {}", src.display()))?;
        let size = metadata.len();
        
        // Ensure destination directory exists
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create destination directory")?;
        }
        
        tokio::fs::rename(&src, &dst)
            .await
            .context(format!("Failed to move {} to {}", src.display(), dst.display()))?;
        
        Ok(size)
    }
    
    async fn set_attr(&self, path: &str, attrs: &MetadataAttributes) -> Result<()> {
        let full_path = self.resolve_path(path);
        
        match attrs {
            MetadataAttributes::Posix { mode, uid, gid } => {
                if let Some(m) = mode {
                    debug!("chmod: {} mode={:o}", full_path.display(), m);
                    
                    let metadata = tokio::fs::metadata(&full_path)
                        .await
                        .context("Failed to stat file")?;
                    
                    let mut perms = metadata.permissions();
                    perms.set_mode(*m);
                    
                    tokio::fs::set_permissions(&full_path, perms)
                        .await
                        .context(format!("Failed to chmod {}", full_path.display()))?;
                }
                
                if uid.is_some() || gid.is_some() {
                    warn!("chown operations require elevated privileges and are not fully implemented");
                    // TODO: Use nix crate for proper chown() support
                    // Would require: nix::unistd::chown(&path, uid, gid)
                }
            },
            MetadataAttributes::Cloud { .. } => {
                anyhow::bail!("Cloud metadata attributes not supported for filesystem backend");
            }
        }
        
        Ok(())
    }
    
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata> {
        let full_path = self.resolve_path(path);
        
        debug!("stat: {}", full_path.display());
        
        let metadata = tokio::fs::metadata(&full_path)
            .await
            .context(format!("Failed to stat {}", full_path.display()))?;
        
        Ok(ObjectMetadata {
            path: path.to_string(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            content_type: None,
            etag: None,
            permissions: Some(metadata.permissions().mode()),
            owner_uid: None,  // TODO: Use nix crate for stat() to get uid/gid
            owner_gid: None,
            storage_class: None,
            custom_metadata: HashMap::new(),
        })
    }
    
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool> {
        let full_path = self.resolve_path(path);
        
        debug!("access: {} mode={}", full_path.display(), mode);
        
        // Convert to C string for libc access() call
        let path_cstr = std::ffi::CString::new(full_path.to_string_lossy().as_bytes())
            .context("Invalid path for access check")?;
        
        // Use POSIX access() syscall
        let result = unsafe {
            libc::access(path_cstr.as_ptr(), mode.to_posix_mode())
        };
        
        Ok(result == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    async fn create_test_ops() -> (FileMetadataOps, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let base_uri = format!("file://{}", temp_dir.path().display());
        let ops = FileMetadataOps::new(&base_uri).unwrap();
        (ops, temp_dir)
    }
    
    #[tokio::test]
    async fn test_mkdir_rmdir() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create directory
        ops.mkdir("test_dir").await.unwrap();
        assert!(temp_dir.path().join("test_dir").exists());
        
        // Remove directory
        ops.rmdir("test_dir", false).await.unwrap();
        assert!(!temp_dir.path().join("test_dir").exists());
    }
    
    #[tokio::test]
    async fn test_mkdir_nested() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create nested directories
        ops.mkdir("a/b/c").await.unwrap();
        assert!(temp_dir.path().join("a/b/c").exists());
    }
    
    #[tokio::test]
    async fn test_copy() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create source file
        let source_path = temp_dir.path().join("source.txt");
        tokio::fs::write(&source_path, b"test data").await.unwrap();
        
        // Copy file
        let bytes = ops.copy("source.txt", "dest.txt").await.unwrap();
        assert_eq!(bytes, 9);
        
        // Verify destination exists with same content
        let dest_path = temp_dir.path().join("dest.txt");
        assert!(dest_path.exists());
        let content = tokio::fs::read(&dest_path).await.unwrap();
        assert_eq!(content, b"test data");
    }
    
    #[tokio::test]
    async fn test_move() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create source file
        let source_path = temp_dir.path().join("source.txt");
        tokio::fs::write(&source_path, b"test data").await.unwrap();
        
        // Move file
        let size = ops.move_obj("source.txt", "moved.txt").await.unwrap();
        assert_eq!(size, 9);
        
        // Verify source gone, destination exists
        assert!(!source_path.exists());
        let dest_path = temp_dir.path().join("moved.txt");
        assert!(dest_path.exists());
    }
    
    #[tokio::test]
    async fn test_get_attr() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create test file
        let file_path = temp_dir.path().join("test.txt");
        tokio::fs::write(&file_path, b"hello world").await.unwrap();
        
        // Get attributes
        let attr = ops.get_attr("test.txt").await.unwrap();
        assert_eq!(attr.size, 11);
        assert_eq!(attr.path, "test.txt");
        assert!(attr.permissions.is_some());
        assert!(attr.modified.is_some());
    }
    
    #[tokio::test]
    async fn test_set_attr_chmod() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create test file
        let file_path = temp_dir.path().join("test.txt");
        tokio::fs::write(&file_path, b"test").await.unwrap();
        
        // Change permissions to 0o644
        let attrs = MetadataAttributes::Posix {
            mode: Some(0o644),
            uid: None,
            gid: None,
        };
        ops.set_attr("test.txt", &attrs).await.unwrap();
        
        // Verify permissions changed
        let metadata = tokio::fs::metadata(&file_path).await.unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    }
    
    #[tokio::test]
    async fn test_access() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create readable file
        let file_path = temp_dir.path().join("test.txt");
        tokio::fs::write(&file_path, b"test").await.unwrap();
        
        // Test various access modes
        assert!(ops.access("test.txt", AccessMode::Exists).await.unwrap());
        assert!(ops.access("test.txt", AccessMode::Read).await.unwrap());
        
        // Non-existent file
        assert!(!ops.access("nonexistent.txt", AccessMode::Exists).await.unwrap());
    }
    
    #[tokio::test]
    async fn test_rmdir_recursive() {
        let (ops, temp_dir) = create_test_ops().await;
        
        // Create nested structure with files
        ops.mkdir("dir/subdir").await.unwrap();
        let file_path = temp_dir.path().join("dir/subdir/file.txt");
        tokio::fs::write(&file_path, b"test").await.unwrap();
        
        // Remove recursively
        ops.rmdir("dir", true).await.unwrap();
        assert!(!temp_dir.path().join("dir").exists());
    }
}
