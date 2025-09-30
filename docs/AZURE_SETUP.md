# Azure Blob Storage Configuration Guide

## Overview
This guide covers proper setup and configuration for Azure Blob Storage backend support in s3-bench using the `az://` URI scheme.

## Prerequisites
- Azure CLI installed and authenticated
- Access to an Azure Storage Account
- Container created in the storage account

## Authentication Setup

### 1. Azure CLI Authentication
```bash
# Login to Azure (interactive)
az login

# Verify authentication
az account show
```

### 2. Required Environment Variables
s3dlio requires these specific environment variables for Azure Blob Storage access:

```bash
# Storage account name
export AZURE_STORAGE_ACCOUNT="your-storage-account-name"

# Storage account key (get from Azure CLI)
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list --account-name your-storage-account-name --query [0].value -o tsv)"
```

### 3. Environment Configuration File
Add to your `.env` file:

```bash
# Azure Blob Storage configuration
AZURE_STORAGE_ACCOUNT=your-storage-account-name
# AZURE_STORAGE_ACCOUNT_KEY will be set by: $(az storage account keys list --account-name your-storage-account-name --query [0].value -o tsv)
```

## URI Format

### Critical: Correct URI Format
**IMPORTANT**: Azure URIs must include the storage account name in the path:

```
✅ Correct:   az://STORAGE_ACCOUNT/CONTAINER/path
❌ Incorrect: az://CONTAINER/path
```

### Examples
```bash
# Storage Account: mystorageaccount
# Container: mycontainer

# Correct formats:
az://mystorageaccount/mycontainer/
az://mystorageaccount/mycontainer/path/to/object.txt
az://mystorageaccount/mycontainer/data/

# Incorrect format (will hang/fail):
az://mycontainer/
az://mycontainer/path/to/object.txt
```

## CLI Usage Examples

### Health Check
```bash
export AZURE_STORAGE_ACCOUNT="mystorageaccount"
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list --account-name mystorageaccount --query [0].value -o tsv)"

s3-bench health --uri "az://mystorageaccount/mycontainer/"
```

### List Objects
```bash
s3-bench list --uri "az://mystorageaccount/mycontainer/"
```

### Upload Objects
```bash
s3-bench put --uri "az://mystorageaccount/mycontainer/test.dat" --object-size 1048576 --objects 1
```

### Download Objects
```bash
s3-bench get --uri "az://mystorageaccount/mycontainer/test.dat"
```

### Get Object Statistics
```bash
s3-bench stat --uri "az://mystorageaccount/mycontainer/test.dat"
```

### Delete Objects
```bash
s3-bench delete --uri "az://mystorageaccount/mycontainer/test.dat"
```

## Workload Configuration

### Example: azure_test.yaml
```yaml
# Azure Blob Storage workload configuration
target: "az://mystorageaccount/mycontainer/"

workload:
  - op: put
    path: "benchmark/object-*"
    object_size: 1048576  # 1MB objects
    weight: 30
  
  - op: get
    path: "benchmark/*"   # Get existing objects
    weight: 70

concurrency: 2
duration: 30s
```

### Running Workload
```bash
# Ensure authentication is set
export AZURE_STORAGE_ACCOUNT="mystorageaccount"
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list --account-name mystorageaccount --query [0].value -o tsv)"

# Run workload
s3-bench run --config azure_test.yaml
```

## Performance Characteristics
- **Latency**: ~700ms typical for small objects (network dependent)
- **Throughput**: ~2-3 operations/second for mixed workloads
- **Concurrency**: Limited by Azure Blob Storage rate limits
- **Network**: Performance heavily dependent on network connectivity to Azure

## Troubleshooting

### Common Issues

#### 1. Hanging/Timeout on Operations
**Cause**: Incorrect URI format using container name as storage account
**Solution**: Use `az://STORAGE_ACCOUNT/CONTAINER/` format

#### 2. Authentication Failed
**Cause**: Missing or incorrect environment variables
**Solution**: Verify `AZURE_STORAGE_ACCOUNT` and `AZURE_STORAGE_ACCOUNT_KEY` are set correctly

#### 3. Container Not Found
**Cause**: Container doesn't exist or incorrect permissions
**Solution**: Verify container exists and account has access

### Debug Commands
```bash
# Test Azure CLI access
az storage blob list --account-name mystorageaccount --container-name mycontainer --num-results 1

# Test s3dlio CLI directly
../s3dlio/target/release/s3-cli -vv ls az://mystorageaccount/mycontainer/

# Test with verbose logging
s3-bench -vv health --uri "az://mystorageaccount/mycontainer/"
```

## Security Notes
- Store account keys securely, preferably in environment variables
- Consider using Azure AD authentication for production environments
- Rotate storage account keys regularly
- Use Azure Key Vault for production credential management