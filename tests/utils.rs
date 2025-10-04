// tests/utils.rs

// Integration tests for io-bench helpers

use regex::Regex;
use io_bench::{bucket_index, glob_to_regex};

/// All your bucket-index cutoffs should match exactly.
#[test]
fn test_bucket_index_boundaries() {
    let bounds = [
        (0, 0),
        (1, 1),
        (8 * 1024, 1),
        (8 * 1024 + 1, 2),
        (64 * 1024, 2),
        (64 * 1024 + 1, 3),
        (512 * 1024, 3),
        (512 * 1024 + 1, 4),
        (4 * 1024 * 1024, 4),
        (4 * 1024 * 1024 + 1, 5),
        (32 * 1024 * 1024, 5),
        (32 * 1024 * 1024 + 1, 6),
        (256 * 1024 * 1024, 6),
        (256 * 1024 * 1024 + 1, 7),
        (2 * 1024 * 1024 * 1024, 7),
        (2 * 1024 * 1024 * 1024 + 1, 8),
    ];
    for (nbytes, expected) in bounds {
        assert_eq!(bucket_index(nbytes), expected,
            "bucket_index({}) should be {}", nbytes, expected);
    }
}

/// Tests that '*' in a glob becomes '.*' in the regex, and is correctly anchored.
#[test]
fn test_glob_to_regex() {
    let glob = "*foo*";
    let re_str = glob_to_regex(glob);
    assert_eq!(re_str, "^.*foo.*$");

    let re = Regex::new(&re_str).unwrap();
    assert!(re.is_match("foobar"));
    assert!(!re.is_match("barbaz"));
}

