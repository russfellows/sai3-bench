//! Filesystem validation with progressive access testing
//!
//! Tests proceed from least to most invasive:
//! 1. stat - Check directory exists and get metadata
//! 2. list - Test directory listing (readdir)
//! 3. read - Test reading existing file or create test file to read
//! 4. write - Test creating .sai3bench_preflight_test file
//! 5. mkdir - Test creating subdirectory (important for prepare phase)
//! 6. delete - Test deleting test file/directory
//!
//! Also checks:
//! - User/group ID vs directory ownership
//! - Mount point detection (NFS, local, etc.)
//! - Disk space availability (for PUT workloads)
//! - Quota limits (if detectable)

use super::{ErrorType, ResultLevel, ValidationResult, ValidationSummary};
use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Information about detected mount point
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub fs_type: String,
    pub device: String,
    pub mount_point: PathBuf,
}

/// Validate filesystem access with progressive testing
///
/// # Arguments
/// * `data_root` - Directory path to validate
/// * `check_write` - Whether to check write permissions (set false for read-only workloads)
/// * `required_space_bytes` - Minimum required disk space (for PUT workloads)
///
/// # Returns
/// ValidationSummary with all test results
pub async fn validate_filesystem(
    data_root: &Path,
    check_write: bool,
    required_space_bytes: Option<u64>,
) -> Result<ValidationSummary> {
    let mut results = Vec::new();

    // 1. User/Group ID Analysis
    results.extend(check_user_identity());

    // 2. Test stat access (most basic - does directory exist?)
    match test_stat_access(data_root).await {
        Ok(result) => {
            results.push(result.clone());
            // If stat failed fatally, don't proceed with other tests
            if result.level == ResultLevel::Error {
                return Ok(ValidationSummary::new(results));
            }
        }
        Err(e) => {
            results.push(ValidationResult::error(
                ErrorType::System,
                "stat",
                format!("Failed to check directory: {}", e),
                "Ensure directory exists and is accessible",
            ));
            return Ok(ValidationSummary::new(results));
        }
    }

    // 3. Directory Ownership Analysis
    if let Ok(ownership_result) = check_directory_ownership(data_root).await {
        results.push(ownership_result);
    }

    // 4. Mount Point Detection (informational)
    if let Ok(mount_info) = detect_mount_point(data_root).await {
        results.push(ValidationResult::info(
            ErrorType::System,
            "mount",
            format!(
                "Mount type: {} ({})",
                mount_info.fs_type, mount_info.device
            ),
            format!("Mount point: {}", mount_info.mount_point.display()),
        ));
    }

    // 5. Test list access (directory listing)
    match test_list_access(data_root).await {
        Ok(result) => {
            results.push(result.clone());
            if result.level == ResultLevel::Error {
                return Ok(ValidationSummary::new(results));
            }
        }
        Err(e) => {
            results.push(ValidationResult::error(
                ErrorType::Permission,
                "list",
                format!("Failed to list directory: {}", e),
                "Check read permission on directory",
            ));
            return Ok(ValidationSummary::new(results));
        }
    }

    // 6. Test read access
    match test_read_access(data_root).await {
        Ok(result) => results.push(result),
        Err(e) => {
            results.push(ValidationResult::error(
                ErrorType::Permission,
                "read",
                format!("Failed to test read access: {}", e),
                "Check read permission on directory and files",
            ));
        }
    }

    // Only test write operations if requested (skip for read-only workloads)
    if check_write {
        // 7. Test write access
        match test_write_access(data_root).await {
            Ok(result) => {
                results.push(result.clone());
                if result.level == ResultLevel::Error {
                    return Ok(ValidationSummary::new(results));
                }
            }
            Err(e) => {
                results.push(ValidationResult::error(
                    ErrorType::Permission,
                    "write",
                    format!("Failed to test write access: {}", e),
                    "Check write permission on directory",
                ));
                return Ok(ValidationSummary::new(results));
            }
        }

        // 8. Test mkdir access (important for prepare phase)
        match test_mkdir_access(data_root).await {
            Ok(result) => results.push(result),
            Err(e) => {
                results.push(ValidationResult::error(
                    ErrorType::Permission,
                    "mkdir",
                    format!("Failed to test mkdir access: {}", e),
                    "Check write permission and ability to create subdirectories",
                ));
            }
        }

        // 9. Test delete access
        match test_delete_access(data_root).await {
            Ok(result) => results.push(result),
            Err(e) => {
                results.push(ValidationResult::error(
                    ErrorType::Permission,
                    "delete",
                    format!("Failed to test delete access: {}", e),
                    "Check delete permission on directory",
                ));
            }
        }

        // 10. Check disk space (for PUT workloads)
        if let Some(required_bytes) = required_space_bytes {
            match check_disk_space(data_root, required_bytes).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(ValidationResult::warning(
                        ErrorType::Resource,
                        "disk-space",
                        format!("Failed to check disk space: {}", e),
                        "Workload may fail if disk is full",
                    ));
                }
            }
        }

        // 11. Check quota limits (if easy to detect - best effort)
        if let Ok(Some(quota_result)) = check_quota(data_root).await {
            results.push(quota_result);
        }
    }

    Ok(ValidationSummary::new(results))
}

/// Check current user and group identity
fn check_user_identity() -> Vec<ValidationResult> {
    let mut results = Vec::new();

    // Get current user/group
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    // Get username and groupname (best effort)
    let username = users::get_user_by_uid(uid)
        .map(|u| u.name().to_string_lossy().to_string())
        .unwrap_or_else(|| format!("uid={}", uid));

    let groupname = users::get_group_by_gid(gid)
        .map(|g| g.name().to_string_lossy().to_string())
        .unwrap_or_else(|| format!("gid={}", gid));

    results.push(ValidationResult::info(
        ErrorType::System,
        "identity",
        format!("Current user: uid={} ({}), gid={} ({})", uid, username, gid, groupname),
        "Process is running with these credentials",
    ));

    results
}

/// Test stat access (check if directory exists and is accessible)
async fn test_stat_access(path: &Path) -> Result<ValidationResult> {
    match fs::metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                Ok(ValidationResult::error(
                    ErrorType::Configuration,
                    "stat",
                    format!("Path exists but is not a directory: {}", path.display()),
                    "Ensure data_root points to a directory, not a file",
                ))
            } else {
                Ok(ValidationResult::success(
                    "stat",
                    "Directory exists and is accessible",
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(ValidationResult::error(
                ErrorType::Configuration,
                "stat",
                format!("Directory does not exist: {}", path.display()),
                format!("Create directory: mkdir -p {}", path.display()),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Ok(ValidationResult::error(
                ErrorType::Permission,
                "stat",
                format!("Permission denied accessing directory: {}", path.display()),
                format!("Grant read permission: chmod +r {}", path.display()),
            ))
        }
        Err(e) => {
            Ok(ValidationResult::error(
                ErrorType::System,
                "stat",
                format!("Failed to access directory: {}", e),
                "Check filesystem health and mount status",
            ))
        }
    }
}

/// Check directory ownership and compare with current user
async fn check_directory_ownership(path: &Path) -> Result<ValidationResult> {
    let metadata = fs::metadata(path).context("Failed to get directory metadata")?;
    let owner_uid = metadata.uid();
    let owner_gid = metadata.gid();
    let current_uid = unsafe { libc::getuid() };
    let current_gid = unsafe { libc::getgid() };

    // Get usernames/groupnames (best effort)
    let owner_user = users::get_user_by_uid(owner_uid)
        .map(|u| u.name().to_string_lossy().to_string())
        .unwrap_or_else(|| format!("uid={}", owner_uid));

    let owner_group = users::get_group_by_gid(owner_gid)
        .map(|g| g.name().to_string_lossy().to_string())
        .unwrap_or_else(|| format!("gid={}", owner_gid));

    // Get permission bits
    let mode = metadata.mode();
    let perms = format!("{:o}", mode & 0o777);

    let ownership_msg = format!(
        "Directory owner: uid={} ({}), gid={} ({}), perms={}",
        owner_uid, owner_user, owner_gid, owner_group, perms
    );

    // Check if user owns directory or is in owner group
    let has_ownership = current_uid == owner_uid || current_uid == 0; // root always has access
    let in_group = current_gid == owner_gid || check_supplementary_groups(owner_gid);

    if !has_ownership && !in_group {
        Ok(ValidationResult::info(
            ErrorType::Permission,
            "ownership",
            ownership_msg,
            format!(
                "User is not owner and not in group - relying on 'other' permissions ({})",
                (mode & 0o7) as u8
            ),
        ))
    } else {
        Ok(ValidationResult::info(
            ErrorType::System,
            "ownership",
            ownership_msg,
            "Ownership permissions look good",
        ))
    }
}

/// Check if current user is in supplementary group
fn check_supplementary_groups(target_gid: u32) -> bool {
    // Get supplementary groups count
    let ngroups: libc::c_int = unsafe {
        libc::getgroups(0, std::ptr::null_mut())
    };

    if ngroups <= 0 {
        return false;
    }

    let mut groups = vec![0u32; ngroups as usize];
    unsafe {
        libc::getgroups(ngroups, groups.as_mut_ptr() as *mut libc::gid_t);
    }

    groups.contains(&target_gid)
}

/// Test list access (directory listing)
async fn test_list_access(path: &Path) -> Result<ValidationResult> {
    match fs::read_dir(path) {
        Ok(_entries) => Ok(ValidationResult::success("list", "Directory listing successful")),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Ok(ValidationResult::error(
                ErrorType::Permission,
                "list",
                format!("Permission denied listing directory: {}", path.display()),
                format!("Grant read+execute permission: chmod +rx {}", path.display()),
            ))
        }
        Err(e) => Ok(ValidationResult::error(
            ErrorType::System,
            "list",
            format!("Failed to list directory: {}", e),
            "Check filesystem health",
        )),
    }
}

/// Test read access
async fn test_read_access(path: &Path) -> Result<ValidationResult> {
    // Try to read first file in directory, or report success if no files exist
    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    // Try to open first file we find
                    match fs::File::open(entry.path()) {
                        Ok(_) => {
                            return Ok(ValidationResult::success(
                                "read",
                                format!("Read access confirmed (tested {})", entry.path().display()),
                            ));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                            return Ok(ValidationResult::error(
                                ErrorType::Permission,
                                "read",
                                format!("Permission denied reading file: {}", entry.path().display()),
                                "Grant read permission on files",
                            ));
                        }
                        Err(e) => {
                            return Ok(ValidationResult::warning(
                                ErrorType::System,
                                "read",
                                format!("Failed to read file: {}", e),
                                "Some files may not be readable",
                            ));
                        }
                    }
                }
            }

            // If we get here, no files were found in directory
            Ok(ValidationResult::success(
                "read",
                "Directory is empty - read access will be tested during workload",
            ))
        }
        Err(e) => Ok(ValidationResult::error(
            ErrorType::Permission,
            "read",
            format!("Failed to list directory for read test: {}", e),
            "Check read permission on directory",
        )),
    }
}

/// Test write access by creating a test file
async fn test_write_access(path: &Path) -> Result<ValidationResult> {
    let test_file = path.join(".sai3bench_preflight_test");

    match fs::File::create(&test_file) {
        Ok(mut file) => {
            // Write some test data
            if let Err(e) = file.write_all(b"sai3-bench pre-flight test") {
                return Ok(ValidationResult::error(
                    ErrorType::Permission,
                    "write",
                    format!("Failed to write test file: {}", e),
                    "Check write permission and disk space",
                ));
            }

            Ok(ValidationResult::success(
                "write",
                "Write access confirmed (test file created)",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let metadata = fs::metadata(path)?;
            let owner_uid = metadata.uid();
            let owner_gid = metadata.gid();
            let current_uid = unsafe { libc::getuid() };

            let suggestion = if current_uid == 0 {
                format!("Check directory permissions: chmod +w {}", path.display())
            } else if current_uid != owner_uid {
                format!(
                    "Grant write permission: sudo chmod g+w {} && sudo chgrp {} {}",
                    path.display(),
                    unsafe { libc::getgid() },
                    path.display()
                )
            } else {
                format!("Grant write permission: chmod +w {}", path.display())
            };

            Ok(ValidationResult::error(
                ErrorType::Permission,
                "write",
                format!("Permission denied creating test file: {}", test_file.display()),
                suggestion,
            ).with_details(format!(
                "EACCES: Permission denied - directory owner: uid={}, gid={}",
                owner_uid, owner_gid
            )))
        }
        Err(e) => Ok(ValidationResult::error(
            ErrorType::System,
            "write",
            format!("Failed to create test file: {}", e),
            "Check filesystem health and available space",
        )),
    }
}

/// Test mkdir access (creating subdirectory)
async fn test_mkdir_access(path: &Path) -> Result<ValidationResult> {
    let test_dir = path.join(".sai3bench_preflight_test_dir");

    match fs::create_dir(&test_dir) {
        Ok(()) => Ok(ValidationResult::success(
            "mkdir",
            "Subdirectory creation successful",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Ok(ValidationResult::error(
                ErrorType::Permission,
                "mkdir",
                "Permission denied creating subdirectory",
                format!("Grant write permission: chmod +w {}", path.display()),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Directory already exists from previous test - this is OK
            Ok(ValidationResult::success(
                "mkdir",
                "Subdirectory creation successful (already exists from previous test)",
            ))
        }
        Err(e) => Ok(ValidationResult::error(
            ErrorType::System,
            "mkdir",
            format!("Failed to create subdirectory: {}", e),
            "Check filesystem health",
        )),
    }
}

/// Test delete access by removing test file and directory
async fn test_delete_access(path: &Path) -> Result<ValidationResult> {
    let test_file = path.join(".sai3bench_preflight_test");
    let test_dir = path.join(".sai3bench_preflight_test_dir");

    let mut deleted_items = Vec::new();
    let mut errors = Vec::new();

    // Try to delete test file
    if test_file.exists() {
        match fs::remove_file(&test_file) {
            Ok(()) => deleted_items.push("file"),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                errors.push(format!("Permission denied deleting file: {}", e));
            }
            Err(e) => {
                errors.push(format!("Failed to delete file: {}", e));
            }
        }
    }

    // Try to delete test directory
    if test_dir.exists() {
        match fs::remove_dir(&test_dir) {
            Ok(()) => deleted_items.push("directory"),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                errors.push(format!("Permission denied deleting directory: {}", e));
            }
            Err(e) => {
                errors.push(format!("Failed to delete directory: {}", e));
            }
        }
    }

    if !errors.is_empty() {
        Ok(ValidationResult::error(
            ErrorType::Permission,
            "delete",
            format!("Delete access failed: {}", errors.join(", ")),
            format!("Grant delete permission: chmod +w {}", path.display()),
        ))
    } else if deleted_items.is_empty() {
        Ok(ValidationResult::success(
            "delete",
            "Delete access check completed (no test files to delete)",
        ))
    } else {
        Ok(ValidationResult::success(
            "delete",
            format!(
                "Delete access confirmed (removed test {})",
                deleted_items.join(" and ")
            ),
        ))
    }
}

/// Check available disk space
async fn check_disk_space(path: &Path, required_bytes: u64) -> Result<ValidationResult> {
    let available = fs2::available_space(path).context("Failed to check disk space")?;

    // Add 20% safety margin
    let required_with_margin = (required_bytes as f64 * 1.2) as u64;

    if available < required_with_margin {
        let required_gb = required_bytes as f64 / 1_000_000_000.0;
        let available_gb = available as f64 / 1_000_000_000.0;
        Ok(ValidationResult::error(
            ErrorType::Resource,
            "disk-space",
            format!(
                "Insufficient disk space: need {:.2} GB (+ 20% margin), have {:.2} GB",
                required_gb, available_gb
            ),
            "Free up disk space or reduce num_objects/object_size in configuration",
        ))
    } else {
        let available_gb = available as f64 / 1_000_000_000.0;
        Ok(ValidationResult::success(
            "disk-space",
            format!("Sufficient disk space available: {:.2} GB", available_gb),
        ))
    }
}

/// Detect mount point information (best effort)
async fn detect_mount_point(path: &Path) -> Result<MountInfo> {
    // Read /proc/mounts to find mount point
    let mounts = fs::read_to_string("/proc/mounts").context("Failed to read /proc/mounts")?;

    let mut best_match: Option<MountInfo> = None;
    let mut best_match_len = 0;

    let path_str = path.to_string_lossy();

    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }

        let device = parts[0];
        let mount_point_str = parts[1];
        let fs_type = parts[2];

        // Check if path starts with this mount point
        if path_str.starts_with(mount_point_str) && mount_point_str.len() > best_match_len {
            best_match_len = mount_point_str.len();
            best_match = Some(MountInfo {
                fs_type: fs_type.to_string(),
                device: device.to_string(),
                mount_point: PathBuf::from(mount_point_str),
            });
        }
    }

    best_match.context("No mount point found for path")
}

/// Check quota limits (best effort - may not work on all filesystems)
async fn check_quota(_path: &Path) -> Result<Option<ValidationResult>> {
    // Quota checking is complex and filesystem-specific
    // For now, return None (not implemented)
    // TODO: Implement quota checking for common filesystems (ext4, XFS, NFS)
    Ok(None)
}

// Note: Need to add 'users' crate for username/groupname lookup
// This will be added to Cargo.toml dependencies
