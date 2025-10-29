// src/cloud_metadata.rs
//
// Cloud storage metadata operations for s3://, gs://, and az:// backends
//
// Provides emulated/adapted metadata operations for object storage:
// - Directories → prefix markers (.keep files)
// - Move → copy + delete (not atomic)
// - Copy → native server-side copy
// - Permissions → not applicable (stat for existence checks)

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use s3dlio::object_store::ObjectStore;

use crate::metadata_ops::{
    AccessMode, MetadataAttributes, MetadataOperations, ObjectMetadata,
};

/// Cloud storage metadata operations handler
/// 
/// Adapts filesystem-style operations to object storage semantics
pub struct CloudMetadataOps {
    store: Box<dyn ObjectStore>,
    base_uri: String,
}

impl CloudMetadataOps {
    /// Create new cloud metadata handler
    /// 
    /// # Arguments
    /// * `store` - ObjectStore instance for the cloud backend
    /// * `base_uri` - Base URI like "s3://bucket/prefix/" or "gs://bucket/"
    pub fn new(store: Box<dyn ObjectStore>, base_uri: String) -> Self {
        Self { store, base_uri }
    }
    
    /// Normalize path for cloud storage (remove leading slash, ensure trailing slash for dirs)
    fn normalize_path(&self, path: &str, is_dir: bool) -> String {
        let clean = path.strip_prefix('/').unwrap_or(path);
        
        if is_dir && !clean.is_empty() && !clean.ends_with('/') {
            format!("{}/", clean)
        } else {
            clean.to_string()
        }
    }
}

#[async_trait]
impl MetadataOperations for CloudMetadataOps {
    async fn mkdir(&self, path: &str) -> Result<()> {
        let dir_path = self.normalize_path(path, true);
        let marker_path = format!("{}.keep", dir_path);
        
        debug!("mkdir (cloud): creating prefix marker at {}", marker_path);
        
        // Create empty marker object to establish prefix
        self.store.put(&marker_path, &[])
            .await
            .context(format!("Failed to create prefix marker: {}", marker_path))?;
        
        info!("Created cloud prefix marker: {}", marker_path);
        Ok(())
    }
    
    async fn rmdir(&self, path: &str, recursive: bool) -> Result<()> {
        let dir_path = self.normalize_path(path, true);
        
        debug!("rmdir (cloud): {} (recursive={})", dir_path, recursive);
        
        // List all objects under the prefix
        let objects = self.store.list(&dir_path, true)
            .await
            .context(format!("Failed to list objects under: {}", dir_path))?;
        
        if objects.is_empty() {
            debug!("No objects found under prefix: {}", dir_path);
            return Ok(());
        }
        
        if !recursive && objects.len() > 1 {
            // If not recursive, only allow if it's just the .keep marker
            let only_marker = objects.len() == 1 && objects[0].ends_with(".keep");
            if !only_marker {
                anyhow::bail!(
                    "Directory not empty (contains {} objects). Use recursive=true to delete all",
                    objects.len()
                );
            }
        }
        
        // Delete all objects under prefix
        for obj in objects {
            self.store.delete(&obj)
                .await
                .context(format!("Failed to delete: {}", obj))?;
        }
        
        info!("Deleted {} objects under prefix: {}", objects.len(), dir_path);
        Ok(())
    }
    
    async fn copy(&self, source: &str, destination: &str) -> Result<u64> {
        let src = self.normalize_path(source, false);
        let dst = self.normalize_path(destination, false);
        
        debug!("copy (cloud): {} -> {}", src, dst);
        
        // Get source object size for return value
        let src_meta = self.store.stat(&src)
            .await
            .context(format!("Failed to stat source: {}", src))?;
        let size = src_meta.size;
        
        // Use native copy operation
        self.store.copy(&src, &dst)
            .await
            .context(format!("Failed to copy {} to {}", src, dst))?;
        
        info!("Copied {} bytes: {} -> {}", size, src, dst);
        Ok(size)
    }
    
    async fn move_obj(&self, source: &str, destination: &str) -> Result<u64> {
        let src = self.normalize_path(source, false);
        let dst = self.normalize_path(destination, false);
        
        debug!("move (cloud): {} -> {} (emulated with copy+delete)", src, dst);
        
        // Cloud storage has no atomic move - emulate with copy + delete
        let size = self.copy(&src, &dst).await?;
        
        self.store.delete(&src)
            .await
            .context(format!("Failed to delete source after copy: {}", src))?;
        
        info!("Moved {} bytes: {} -> {}", size, src, dst);
        Ok(size)
    }
    
    async fn set_attr(&self, path: &str, attrs: &MetadataAttributes) -> Result<()> {
        let obj_path = self.normalize_path(path, false);
        
        match attrs {
            MetadataAttributes::Cloud { 
                content_type,
                cache_control,
                content_encoding,
                content_language,
                content_disposition,
                expires,
                storage_class,
                custom 
            } => {
                warn!(
                    "set_attr (cloud) for {} - metadata updates require copying object with new metadata",
                    obj_path
                );
                warn!("  Requested updates:");
                if content_type.is_some() { warn!("    content_type: {:?}", content_type); }
                if cache_control.is_some() { warn!("    cache_control: {:?}", cache_control); }
                if content_encoding.is_some() { warn!("    content_encoding: {:?}", content_encoding); }
                if content_language.is_some() { warn!("    content_language: {:?}", content_language); }
                if content_disposition.is_some() { warn!("    content_disposition: {:?}", content_disposition); }
                if expires.is_some() { warn!("    expires: {:?}", expires); }
                if storage_class.is_some() { warn!("    storage_class: {:?}", storage_class); }
                if !custom.is_empty() { warn!("    custom metadata: {} keys", custom.len()); }
                
                // Cloud storage metadata updates require copying object with new metadata
                // This is backend-specific:
                // - S3: CopyObject with x-amz-metadata-directive: REPLACE
                // - GCS: rewrite() with new metadata
                // - Azure: Copy with x-ms-meta-* headers
                //
                // Not yet implemented in ObjectStore trait - would need:
                //   store.copy_with_metadata(src, dst, metadata)
                // For now, this is a stub showing what metadata can be updated
                
                antml:bail!(
                    "Metadata updates not yet implemented - requires ObjectStore::copy_with_metadata() extension"
                );
            },
            MetadataAttributes::Posix { .. } => {
                anyhow::bail!("POSIX attributes not supported for cloud storage backend");
            }
        }
    }
    
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata> {
        let obj_path = self.normalize_path(path, false);
        
        debug!("get_attr (cloud): stat {}", obj_path);
        
        let s3dlio_meta = self.store.stat(&obj_path)
            .await
            .context(format!("Failed to stat object: {}", obj_path))?;
        
        // Convert s3dlio ObjectMetadata to our unified ObjectMetadata
        // Parse last_modified string to SystemTime if available
        let modified = s3dlio_meta.last_modified.as_ref().and_then(|lm_str| {
            // Try parsing RFC3339/ISO8601 format
            chrono::DateTime::parse_from_rfc3339(lm_str)
                .ok()
                .map(|dt| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(dt.timestamp() as u64))
        });
        
        Ok(ObjectMetadata {
            path: path.to_string(),
            size: s3dlio_meta.size,
            modified,
            content_type: s3dlio_meta.content_type,
            cache_control: s3dlio_meta.cache_control,
            content_encoding: s3dlio_meta.content_encoding,
            content_language: s3dlio_meta.content_language,
            content_disposition: s3dlio_meta.content_disposition,
            etag: s3dlio_meta.e_tag,
            expires: s3dlio_meta.expires,
            permissions: None,
            owner_uid: None,
            owner_gid: None,
            storage_class: s3dlio_meta.storage_class,
            server_side_encryption: s3dlio_meta.server_side_encryption,
            ssekms_key_id: s3dlio_meta.ssekms_key_id,
            version_id: s3dlio_meta.version_id,
            replication_status: s3dlio_meta.replication_status,
            custom_metadata: s3dlio_meta.metadata,
        })
    }
    
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool> {
        let obj_path = self.normalize_path(path, false);
        
        debug!("access (cloud): stat {} mode={}", obj_path, mode);
        
        // For cloud storage, "access" means stat request succeeds
        // All modes (read/write/execute) are treated as existence check
        match self.store.stat(&obj_path).await {
            Ok(_) => Ok(true),
            Err(e) => {
                // Distinguish between "not found" and actual errors
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("not found") || err_str.contains("404") {
                    Ok(false)
                } else {
                    // Real error (permission denied, network issue, etc.)
                    Err(e).context("Access check failed")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use s3dlio::object_store::store_for_uri;
    use tempfile::TempDir;
    
    // Helper to create a test cloud store using file:// backend
    async fn create_test_cloud_ops() -> (CloudMetadataOps, TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let base_uri = format!("file://{}/", temp_dir.path().display());
        let store = store_for_uri(&base_uri).unwrap();
        let ops = CloudMetadataOps::new(store, base_uri.clone());
        (ops, temp_dir, base_uri)
    }
    
    #[tokio::test]
    async fn test_mkdir_creates_marker() {
        let (ops, temp_dir, _) = create_test_cloud_ops().await;
        
        ops.mkdir("test_prefix").await.unwrap();
        
        // Verify .keep marker exists
        let marker_path = temp_dir.path().join("test_prefix/.keep");
        assert!(marker_path.exists());
    }
    
    #[tokio::test]
    async fn test_copy() {
        let (ops, temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create source object
        let store = store_for_uri(&base_uri).unwrap();
        store.put("source.txt", b"test data").await.unwrap();
        
        // Copy object
        let size = ops.copy("source.txt", "dest.txt").await.unwrap();
        assert_eq!(size, 9);
        
        // Verify destination exists
        let dest_path = temp_dir.path().join("dest.txt");
        assert!(dest_path.exists());
        let content = tokio::fs::read(&dest_path).await.unwrap();
        assert_eq!(content, b"test data");
    }
    
    #[tokio::test]
    async fn test_move_deletes_source() {
        let (ops, temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create source object
        let store = store_for_uri(&base_uri).unwrap();
        store.put("source.txt", b"test data").await.unwrap();
        
        // Move object
        let size = ops.move_obj("source.txt", "moved.txt").await.unwrap();
        assert_eq!(size, 9);
        
        // Verify source deleted, destination exists
        assert!(!temp_dir.path().join("source.txt").exists());
        assert!(temp_dir.path().join("moved.txt").exists());
    }
    
    #[tokio::test]
    async fn test_get_attr() {
        let (ops, _temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create test object
        let store = store_for_uri(&base_uri).unwrap();
        store.put("test.txt", b"hello world").await.unwrap();
        
        // Get attributes
        let attr = ops.get_attr("test.txt").await.unwrap();
        assert_eq!(attr.size, 11);
        assert_eq!(attr.path, "test.txt");
        assert!(attr.modified.is_some());
    }
    
    #[tokio::test]
    async fn test_access() {
        let (ops, _temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create test object
        let store = store_for_uri(&base_uri).unwrap();
        store.put("test.txt", b"test").await.unwrap();
        
        // Test access
        assert!(ops.access("test.txt", AccessMode::Exists).await.unwrap());
        assert!(ops.access("test.txt", AccessMode::Read).await.unwrap());
        
        // Non-existent object
        assert!(!ops.access("nonexistent.txt", AccessMode::Exists).await.unwrap());
    }
    
    #[tokio::test]
    async fn test_rmdir_recursive() {
        let (ops, temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create prefix with multiple objects
        let store = store_for_uri(&base_uri).unwrap();
        store.put("prefix/file1.txt", b"test").await.unwrap();
        store.put("prefix/file2.txt", b"test").await.unwrap();
        store.put("prefix/.keep", &[]).await.unwrap();
        
        // Delete recursively
        ops.rmdir("prefix", true).await.unwrap();
        
        // Verify all objects deleted
        assert!(!temp_dir.path().join("prefix/file1.txt").exists());
        assert!(!temp_dir.path().join("prefix/file2.txt").exists());
        assert!(!temp_dir.path().join("prefix/.keep").exists());
    }
    
    #[tokio::test]
    async fn test_rmdir_non_recursive_fails_if_not_empty() {
        let (ops, _temp_dir, base_uri) = create_test_cloud_ops().await;
        
        // Create prefix with multiple objects
        let store = store_for_uri(&base_uri).unwrap();
        store.put("prefix/file1.txt", b"test").await.unwrap();
        store.put("prefix/.keep", &[]).await.unwrap();
        
        // Attempt non-recursive delete should fail
        let result = ops.rmdir("prefix", false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not empty"));
    }
}
