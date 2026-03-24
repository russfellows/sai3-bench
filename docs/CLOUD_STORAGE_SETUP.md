# Cloud Storage Setup Guide

## Overview
This guide covers authentication and configuration for all cloud storage backends supported by sai3-bench: Amazon S3, Azure Blob Storage, and Google Cloud Storage.

---

## Amazon S3 (s3://)

### Prerequisites
- AWS CLI installed and configured
- Access to an S3 bucket
- IAM credentials with appropriate permissions

### Authentication Setup

#### Option 1: AWS CLI Configuration (Recommended)
```bash
# Configure AWS CLI (interactive)
aws configure

# This creates ~/.aws/credentials with:
# [default]
# aws_access_key_id = YOUR_ACCESS_KEY
# aws_secret_access_key = YOUR_SECRET_KEY
# region = us-west-2
```

#### Option 2: Environment Variables
```bash
export AWS_ACCESS_KEY_ID="your-access-key-id"
export AWS_SECRET_ACCESS_KEY="your-secret-access-key"
export AWS_REGION="us-west-2"  # Optional, defaults to us-east-1
```

#### Option 3: Environment File (.env)
```bash
# AWS S3 configuration
AWS_ACCESS_KEY_ID=your-access-key-id
AWS_SECRET_ACCESS_KEY=your-secret-access-key
AWS_REGION=us-west-2
AWS_ENDPOINT_URL=https://s3.us-west-2.amazonaws.com  # Optional
```

### URI Format
```
s3://BUCKET_NAME/path/to/object
```

**Examples**:
```bash
# Bucket: my-benchmark-bucket
s3://my-benchmark-bucket/
s3://my-benchmark-bucket/test-data/
s3://my-benchmark-bucket/test-data/object.dat
```

### CLI Usage Examples

#### Health Check
```bash
sai3-bench util health --uri "s3://my-benchmark-bucket/"
```

#### List Objects
```bash
sai3-bench util list --uri "s3://my-benchmark-bucket/test-data/"
```

#### Upload Objects
```bash
sai3-bench put \
  --uri "s3://my-benchmark-bucket/uploads/" \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 10
```

#### Download Objects
```bash
sai3-bench get \
  --uri "s3://my-benchmark-bucket/uploads/*" \
  --jobs 10
```

#### Workload Configuration (s3_workload.yaml)
```yaml
target: "s3://my-benchmark-bucket/benchmark/"
duration: 60s
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "s3://my-benchmark-bucket/benchmark/data/"
      count: 1000
      min_size: 4096
      max_size: 1048576
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 70
  
  - op: put
    path: "data/"
    weight: 25
    size_distribution:
      type: lognormal
      mean: 65536
      std_dev: 32768
      min: 1024
      max: 1048576
  
  - op: list
    path: "data/"
    weight: 5
```

### S3-Compatible Storage (MinIO, etc.)
For S3-compatible endpoints, use `AWS_ENDPOINT_URL`:
```bash
export AWS_ENDPOINT_URL="https://minio.example.com"
export AWS_ACCESS_KEY_ID="minioadmin"
export AWS_SECRET_ACCESS_KEY="minioadmin"

sai3-bench util health --uri "s3://my-bucket/"
```

### Performance Characteristics
- **Latency**: 50-200ms (region/network dependent)
- **Throughput**: High (100+ ops/sec with sufficient concurrency)
- **Best concurrency**: 16-64 for mixed workloads
- **Large file optimization**: Range download optimization (s3dlio v0.9.50+) can improve throughput by 76% for files ≥64MB
  - **See**: [S3DLIO Performance Tuning Guide](S3DLIO_PERFORMANCE_TUNING.md) for configuration details

---

## Azure Blob Storage (az://)

### Prerequisites
- Azure CLI installed and authenticated
- Access to an Azure Storage Account
- Container created in the storage account

### Authentication Setup

#### Step 1: Azure CLI Authentication
```bash
# Login to Azure (interactive)
az login

# Verify authentication
az account show
```

#### Step 2: Environment Variables
```bash
# Storage account name
export AZURE_STORAGE_ACCOUNT="your-storage-account-name"

# Storage account key (get from Azure CLI)
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list \
  --account-name your-storage-account-name \
  --query [0].value -o tsv)"
```

#### Step 3: Environment File (.env)
```bash
# Azure Blob Storage configuration
AZURE_STORAGE_ACCOUNT=your-storage-account-name
# Get key with: az storage account keys list --account-name NAME --query [0].value -o tsv
```

### URI Format
**CRITICAL**: Azure URIs must include the storage account name:

```
az://STORAGE_ACCOUNT/CONTAINER/path
```

**Examples**:
```bash
# Storage Account: mystorageaccount
# Container: mycontainer

✅ Correct:
az://mystorageaccount/mycontainer/
az://mystorageaccount/mycontainer/test-data/
az://mystorageaccount/mycontainer/test-data/object.dat

❌ Incorrect (will fail/hang):
az://mycontainer/
az://mycontainer/test-data/
```

### CLI Usage Examples

#### Health Check
```bash
export AZURE_STORAGE_ACCOUNT="mystorageaccount"
export AZURE_STORAGE_ACCOUNT_KEY="$(az storage account keys list --account-name mystorageaccount --query [0].value -o tsv)"

sai3-bench util health --uri "az://mystorageaccount/mycontainer/"
```

#### List Objects
```bash
sai3-bench util list --uri "az://mystorageaccount/mycontainer/test-data/"
```

#### Upload Objects
```bash
sai3-bench put \
  --uri "az://mystorageaccount/mycontainer/uploads/" \
  --object-size 524288 \
  --objects 50 \
  --concurrency 4
```

#### Download Objects
```bash
sai3-bench get \
  --uri "az://mystorageaccount/mycontainer/uploads/*" \
  --jobs 4
```

#### Workload Configuration (azure_workload.yaml)
```yaml
target: "az://mystorageaccount/mycontainer/benchmark/"
duration: 60s
concurrency: 8

prepare:
  ensure_objects:
    - base_uri: "az://mystorageaccount/mycontainer/benchmark/data/"
      count: 500
      min_size: 65536
      max_size: 1048576
      fill: zero     # DO NOT USE - only for testing zero-data behavior

workload:
  - op: get
    path: "data/*"
    weight: 60
  
  - op: put
    path: "data/"
    weight: 30
    size_distribution:
      type: uniform
      min: 65536
      max: 1048576
  
  - op: list
    path: "data/"
    weight: 10
```

### Performance Characteristics
- **Latency**: Network/region dependent; expect single-digit ms on same-region or LAN, higher over WAN
- **Throughput**: Comparable to S3/GCS on same-region deployments
- **Best concurrency**: 16-64 for mixed workloads (tune to your network)
- **Note**: Performance figures vary significantly based on network topology. Always benchmark in your target environment.

---

## Google Cloud Storage (gs:// or gcs://)

### Prerequisites
- Google Cloud SDK (gcloud) installed
- Service account or user credentials
- Access to a GCS bucket

### Authentication Setup

s3dlio requires two things before any `gs://` operation will succeed:
1. Valid GCP credentials (via one of the options below)
2. The `GCLOUD_PROJECT` environment variable set to your GCP project ID

**Quick debug checklist** — if GCS access is failing, run through these steps:

```bash
# Step 1: authenticate
gcloud auth login

# Step 2: export your project ID (required by s3dlio)
export GCLOUD_PROJECT=your-project-id   # e.g. signal65-testing

# Step 3: verify access with s3-cli (ships with s3dlio)
s3-cli list-buckets gs://
```

Expected output:
```
Listing containers for 'gs://'...

Found N container(s):
Name                    URI                    Creation Date
-----------------------------------------------------------
my-bucket               gs://my-bucket         ...
```

If `list-buckets` succeeds, sai3-bench will work.

> **On GCE VMs**: `gcloud auth login` warns that personal credentials may be
> visible to others on the VM and recommends service accounts instead.
> Either path works — service accounts are preferred for production.

#### Option 1: Interactive user login (GCE or local)

```bash
gcloud auth login
# Follow the browser/code flow, then:
export GCLOUD_PROJECT=your-project-id
```

#### Option 2: Application Default Credentials (scripts / CI)

```bash
gcloud auth application-default login
export GCLOUD_PROJECT=your-project-id
# ADC credentials written to ~/.config/gcloud/application_default_credentials.json
```

#### Option 3: Service Account Key File (recommended for production)

```bash
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/service-account-key.json"
export GCLOUD_PROJECT=your-project-id
```

#### Option 4: Environment File (.env)

```bash
# Google Cloud Storage configuration
GOOGLE_APPLICATION_CREDENTIALS=/home/user/.config/gcloud/service-account-key.json
GCLOUD_PROJECT=your-project-id
```

### URI Format
Both `gs://` and `gcs://` schemes are supported:

```
gs://BUCKET_NAME/path/to/object
gcs://BUCKET_NAME/path/to/object
```

**Examples**:
```bash
# Bucket: my-gcs-bucket
gs://my-gcs-bucket/
gs://my-gcs-bucket/test-data/
gs://my-gcs-bucket/test-data/object.dat

# Alternative scheme (equivalent):
gcs://my-gcs-bucket/test-data/
```

### CLI Usage Examples

#### Health Check
```bash
# Ensure credentials and project are set (see Authentication Setup above)
export GCLOUD_PROJECT=your-project-id

sai3-bench util health --uri "gs://my-gcs-bucket/"
```

#### List Objects
```bash
sai3-bench util list --uri "gs://my-gcs-bucket/test-data/"
```

#### Upload Objects
```bash
sai3-bench put \
  --uri "gs://my-gcs-bucket/uploads/" \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 16
```

#### Download Objects
```bash
sai3-bench get \
  --uri "gs://my-gcs-bucket/uploads/*" \
  --jobs 16
```

#### Workload Configuration (gcs_workload.yaml)
```yaml
target: "gs://my-gcs-bucket/benchmark/"
duration: 60s
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "gs://my-gcs-bucket/benchmark/data/"
      count: 2000
      min_size: 16384
      max_size: 1048576
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 60
  
  - op: put
    path: "data/"
    weight: 30
    size_distribution:
      type: lognormal
      mean: 262144
      std_dev: 131072
      min: 4096
      max: 2097152
  
  - op: stat
    path: "data/*"
    weight: 5
  
  - op: list
    path: "data/"
    weight: 5
```

### Performance Characteristics
- **Latency**: 50-150ms (region/network dependent)
- **Throughput**: High (similar to S3, 100+ ops/sec standard; RAPID buckets achieve multi-GB/s)
- **Best concurrency**: 16-64 for mixed workloads
- **RAPID (Hyperdisk ML)**: Fully supported as of sai3-bench v0.8.86 / s3dlio v0.9.84.
  Uses `BidiWriteObject` for PUTs and `BidiReadObject` for GETs.
  Auto-detected per bucket, or force via `gcs_rapid_mode: true` in `s3dlio_optimization`.
  See [GCS_INTEGRATION.md](GCS_INTEGRATION.md) for YAML examples.

---

## Comparison Summary

| Feature | S3 | Azure Blob | GCS |
|---------|----|-----------|----|
| **URI Scheme** | `s3://bucket/path` | `az://account/container/path` | `gs://bucket/path` |
| **Typical Latency** | 50-200ms | Network-dependent | 50-150ms |
| **Throughput** | High (100+ ops/s) | Network-dependent | High (100+ ops/s) |
| **Best Concurrency** | 16-64 | 16-64 | 16-64 |
| **Auth Method** | AWS credentials | Account key | Service account / ADC |
| **Rate Limits** | Generous | Restrictive | Generous |
| **RangeEngine Benefit** | Yes (≥64MB) | Yes (≥64MB) | No (RAPID uses bidi streaming) |

---

## Troubleshooting

### Common Issues Across All Backends

#### 1. Authentication Errors
**Symptoms**: "Access Denied", "Unauthorized", "403 Forbidden"

**Solutions**:
- **S3**: Verify `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` are set
- **Azure**: Check `AZURE_STORAGE_ACCOUNT` and `AZURE_STORAGE_ACCOUNT_KEY`
- **GCS**: Verify `GOOGLE_APPLICATION_CREDENTIALS` points to valid JSON file, or run `gcloud auth login`; also confirm `GCLOUD_PROJECT` is exported

#### 2. Bucket/Container Not Found
**Symptoms**: "NoSuchBucket", "Container not found", "404 Not Found"

**Solutions**:
- Verify bucket/container exists: `aws s3 ls` / `az storage container list` / `gsutil ls`
- Check URI format is correct for the backend
- For Azure: Ensure storage account name is in URI

#### 3. Network/Timeout Issues
**Symptoms**: "Connection timeout", "Request timeout", hanging operations

**Solutions**:
- Check network connectivity to cloud provider
- Reduce concurrency if rate-limited
- For Azure: Verify correct URI format (must include storage account)
- Enable verbose logging: `sai3-bench -vv ...`

#### 4. Permission Errors
**Symptoms**: "Access Denied" for specific operations (e.g., PUT works but DELETE fails)

**Solutions**:
- **S3**: Check IAM policy grants required permissions (s3:PutObject, s3:GetObject, s3:DeleteObject, s3:ListBucket)
- **Azure**: Verify storage account key has full access, or check RBAC roles
- **GCS**: Ensure service account has Storage Object Admin or equivalent role

### Debug Commands

#### Test Cloud Provider CLI Access
```bash
# S3
aws s3 ls s3://my-bucket/ --region us-west-2

# Azure
az storage blob list --account-name mystorageaccount --container-name mycontainer --num-results 5

# GCS
gsutil ls gs://my-gcs-bucket/
```

#### Test with Verbose Logging
```bash
# Enable detailed tracing
sai3-bench -vv health --uri "s3://my-bucket/"
sai3-bench -vv health --uri "az://account/container/"
sai3-bench -vv health --uri "gs://my-bucket/"
```

#### Verify Environment Variables
```bash
# S3
echo $AWS_ACCESS_KEY_ID
echo $AWS_SECRET_ACCESS_KEY
echo $AWS_REGION

# Azure
echo $AZURE_STORAGE_ACCOUNT
echo $AZURE_STORAGE_ACCOUNT_KEY

# GCS
echo $GOOGLE_APPLICATION_CREDENTIALS
echo $GCLOUD_PROJECT
cat $GOOGLE_APPLICATION_CREDENTIALS | jq .type  # Should show "service_account"
```

---

## Security Best Practices

### Credential Management
1. **Never commit credentials** to version control
2. **Use environment files** (.env) for local development
3. **Use IAM roles** in production (EC2 instance profiles, GKE workload identity)
4. **Rotate keys regularly** (especially storage account keys)
5. **Use least-privilege** IAM policies

### Example .env File
```bash
# .env file (add to .gitignore!)

# AWS S3
AWS_ACCESS_KEY_ID=your-key-id
AWS_SECRET_ACCESS_KEY=your-secret-key
AWS_REGION=us-west-2

# Azure Blob Storage
AZURE_STORAGE_ACCOUNT=yourstorageaccount
AZURE_STORAGE_ACCOUNT_KEY=your-account-key

# Google Cloud Storage
GOOGLE_APPLICATION_CREDENTIALS=/home/user/.config/gcloud/service-account.json
```

### Loading Environment File
```bash
# Load environment variables
source .env

# Or use with sai3-bench
export $(cat .env | xargs) && sai3-bench run --config workload.yaml
```

---

## Next Steps

- Review [USAGE.md](USAGE.md) for general sai3-bench operations
- See [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) for multi-host testing
- Check [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for complete YAML reference
- Explore [test configs](../tests/configs/) for more examples
