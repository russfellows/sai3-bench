// Manual test to verify directory structure matches rdf-bench behavior

use sai3_bench::directory_tree::{DirectoryStructureConfig, DirectoryTree};

fn main() {
    println!("=== Testing Directory Tree Structure ===\n");
    
    // Test 1: width=2, depth=2 (like rdf-bench example)
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 0,
        distribution: "bottom".to_string(),
        dir_mask: "sai3bench.d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config).unwrap();
    
    println!("Configuration: width=2, depth=2");
    println!("Total directories: {}", tree.total_directories());
    println!();
    
    println!("Expected structure (like rdf-bench):");
    println!("  Level 1:");
    println!("    sai3bench.d1_w1.dir");
    println!("    sai3bench.d1_w2.dir");
    println!("  Level 2:");
    println!("    sai3bench.d1_w1.dir/sai3bench.d2_w1.dir");
    println!("    sai3bench.d1_w1.dir/sai3bench.d2_w2.dir");
    println!("    sai3bench.d1_w2.dir/sai3bench.d2_w1.dir  <- Note: width resets to 1 for each parent!");
    println!("    sai3bench.d1_w2.dir/sai3bench.d2_w2.dir");
    println!();
    
    println!("Actual structure:");
    for level in 1..=2 {
        println!("  Level {}:", level);
        if let Some(paths) = tree.paths_at_level(level) {
            for path in paths {
                println!("    {}", path);
            }
        }
    }
    println!();
    
    // Test 2: width=3, depth=3
    let config2 = DirectoryStructureConfig {
        width: 3,
        depth: 3,
        files_per_dir: 0,
        distribution: "bottom".to_string(),
        dir_mask: "d%d_%d".to_string(),
    };
    
    let tree2 = DirectoryTree::new(config2).unwrap();
    
    println!("\n=== Configuration: width=3, depth=3 ===");
    println!("Total directories: {} (expected: 3 + 9 + 27 = 39)", tree2.total_directories());
    println!();
    
    println!("Sample Level 2 paths:");
    if let Some(paths) = tree2.paths_at_level(2) {
        for (i, path) in paths.iter().enumerate().take(12) {
            println!("  [{}] {}", i, path);
        }
    }
}
