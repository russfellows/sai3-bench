// Debugging script to test ObjectStore usage with file system
use s3dlio::store_for_uri;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uri = "file:///tmp/sai3bench-test/";
    let store = store_for_uri(uri)?;
    
    println!("Store created successfully for URI: {}", uri);
    
    // Try to put a test object
    let test_data = b"hello world";
    let path = "test-file.txt";
    
    println!("Attempting to put object at path: {}", path);
    
    match store.put(path, test_data).await {
        Ok(_) => println!("Put successful!"),
        Err(e) => println!("Put failed: {}", e),
    }
    
    Ok(())
}