// Debugging script to test Azure Blob Storage via s3dlio
use s3dlio::store_for_uri;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Environment variables:");
    for (key, value) in std::env::vars() {
        if key.starts_with("AZURE_") {
            println!("  {}: {}", key, value);
        }
    }
    
    // Use environment variables for configuration
    let storage_account = std::env::var("AZURE_STORAGE_ACCOUNT")
        .expect("AZURE_STORAGE_ACCOUNT environment variable not set");
    let container = std::env::var("AZURE_BLOB_CONTAINER")
        .unwrap_or_else(|_| "s3dlio".to_string());
    
    let uri = format!("az://{}/{}/", storage_account, container);
    println!("Testing URI: {}", uri);
    
    println!("Creating store...");
    let store = store_for_uri(&uri)?;
    println!("Store created successfully for URI: {}", uri);
    
    println!("Attempting to list objects...");
    match store.list(&uri, false).await {
        Ok(objects) => {
            println!("List successful! Found {} objects:", objects.len());
            for obj in objects.iter().take(5) {
                println!("  {}", obj);
            }
        },
        Err(e) => {
            println!("List failed: {}", e);
            println!("Error details: {:#?}", e);
        },
    }
    
    Ok(())
}