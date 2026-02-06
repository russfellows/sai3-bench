//! Tests for base_uri handling in isolated multi-endpoint mode
//!
//! These tests verify the fix for the distributed workload crash bug where
//! all agents tried to access the same base_uri instead of using their own endpoints.

#[cfg(test)]
mod base_uri_tests {
    use crate::config::EnsureSpec;

    /// Test 1: base_uri explicitly provided - should use it regardless of multi_endpoint
    #[test]
    fn test_explicit_base_uri_no_multi_endpoint() {
        let spec = EnsureSpec {
            base_uri: Some("file:///data/test/".to_string()),
            use_multi_endpoint: false,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///data/test/");
    }

    /// Test 2: base_uri provided with multi_endpoint - should still use explicit base_uri
    #[test]
    fn test_explicit_base_uri_with_multi_endpoint() {
        let spec = EnsureSpec {
            base_uri: Some("file:///data/test/".to_string()),
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "file:///mnt/filesys1/".to_string(),
            "file:///mnt/filesys2/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///data/test/");
    }

    /// Test 3: No base_uri with multi_endpoint - should use first endpoint (THE FIX)
    #[test]
    fn test_no_base_uri_uses_first_endpoint() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "file:///mnt/scratch/dir1/benchmark/".to_string(),
            "file:///mnt/scratch/dir2/benchmark/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///mnt/scratch/dir1/benchmark/");
    }

    /// Test 4: No base_uri, no multi_endpoint - should fail with clear error
    #[test]
    fn test_no_base_uri_no_multi_endpoint_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: false,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("base_uri is required"));
        assert!(err_msg.contains("use_multi_endpoint=false"));
    }

    /// Test 5: multi_endpoint=true but no endpoints provided - should fail
    #[test]
    fn test_multi_endpoint_no_endpoints_provided_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let result = spec.get_base_uri(None);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no multi_endpoint configuration provided"));
    }

    /// Test 6: multi_endpoint=true but empty endpoint list - should fail
    #[test]
    fn test_multi_endpoint_empty_list_fails() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints: Vec<String> = vec![];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("multi_endpoint list is empty"));
    }

    /// Test 7: Simulate isolated mode - different agents get different endpoints
    #[test]
    fn test_isolated_mode_different_agents() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        // Agent 1 endpoints
        let agent1_endpoints = vec!["file:///mnt/filesys1/benchmark/".to_string()];
        let result1 = spec.get_base_uri(Some(&agent1_endpoints));
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), "file:///mnt/filesys1/benchmark/");

        // Agent 2 endpoints
        let agent2_endpoints = vec!["file:///mnt/filesys5/benchmark/".to_string()];
        let result2 = spec.get_base_uri(Some(&agent2_endpoints));
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "file:///mnt/filesys5/benchmark/");

        // Agent 3 endpoints
        let agent3_endpoints = vec!["file:///mnt/filesys9/benchmark/".to_string()];
        let result3 = spec.get_base_uri(Some(&agent3_endpoints));
        assert!(result3.is_ok());
        assert_eq!(result3.unwrap(), "file:///mnt/filesys9/benchmark/");

        // Agent 4 endpoints
        let agent4_endpoints = vec!["file:///mnt/filesys13/benchmark/".to_string()];
        let result4 = spec.get_base_uri(Some(&agent4_endpoints));
        assert!(result4.is_ok());
        assert_eq!(result4.unwrap(), "file:///mnt/filesys13/benchmark/");
    }

    /// Test 8: S3 URIs work correctly
    #[test]
    fn test_s3_uris() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "s3://bucket1/prefix/".to_string(),
            "s3://bucket2/prefix/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "s3://bucket1/prefix/");
    }

    /// Test 9: Azure URIs work correctly
    #[test]
    fn test_azure_uris() {
        let spec = EnsureSpec {
            base_uri: None,
            use_multi_endpoint: true,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        let endpoints = vec![
            "az://container1/path/".to_string(),
            "az://container2/path/".to_string(),
        ];

        let result = spec.get_base_uri(Some(&endpoints));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "az://container1/path/");
    }

    /// Test 10: Regression test - old behavior still works
    #[test]
    fn test_backward_compatibility_explicit_base_uri() {
        let spec = EnsureSpec {
            base_uri: Some("file:///old/path/".to_string()),
            use_multi_endpoint: false,
            count: 100,
            min_size: None,
            max_size: None,
            size_spec: None,
            fill: crate::config::FillPattern::Zero,
            dedup_factor: 1,
            compress_factor: 1,
        };

        // Old code path - no multi_endpoint config provided
        let result = spec.get_base_uri(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file:///old/path/");
    }
}
