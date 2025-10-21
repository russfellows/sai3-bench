# SSH Setup Automation for Distributed Testing

The `ssh-setup` command automates the painful process of configuring passwordless SSH access to your benchmark VMs.

## Quick Start

```bash
# One command to setup all your VMs:
sai3bench-ctl ssh-setup --hosts ubuntu@vm1.example.com,ubuntu@vm2.example.com,ubuntu@vm3.example.com

# Or with IP addresses:
sai3bench-ctl ssh-setup --hosts ubuntu@10.0.1.10,ubuntu@10.0.1.11,ubuntu@10.0.1.12

# Use default user (current $USER or ubuntu):
sai3bench-ctl ssh-setup --hosts vm1.example.com,vm2.example.com --user ubuntu
```

## What It Does

The `ssh-setup` command automatically:

1. **Generates SSH key pair** (if needed)
   - Location: `~/.ssh/sai3bench_id_rsa`
   - Type: RSA 4096-bit
   - No passphrase (for automation)

2. **Distributes public key** to each VM
   - Uses `ssh-copy-id` for password-based initial auth
   - You'll be prompted for each VM's password once

3. **Verifies passwordless access**
   - Tests `ssh -i ~/.ssh/sai3bench_id_rsa user@host echo OK`
   - Ensures no password prompts

4. **Checks Docker availability**
   - Verifies `docker --version` works on each VM
   - Warns if Docker is missing (with install instructions)

5. **Generates config template**
   - Prints ready-to-use YAML configuration
   - Shows next steps

## Command Options

```bash
sai3bench-ctl ssh-setup [OPTIONS]

Options:
  --hosts <HOSTS>              Comma-separated list of hosts (required)
                               Format: user@host or just host
                               
  --user <USER>                Default SSH user if not in host string
                               Default: $USER or "ubuntu"
                               
  --key-path <PATH>            SSH key location
                               Default: ~/.ssh/sai3bench_id_rsa
                               
  --interactive <BOOL>         Prompt for passwords (default: true)
                               Set to false for non-interactive setups
                               
  --test-only                  Test connectivity only, don't setup
                               Useful for verifying existing setup
```

## Examples

### Setup with password prompts (interactive)

```bash
sai3bench-ctl ssh-setup --hosts ubuntu@vm1.aws.com,ubuntu@vm2.aws.com

# You'll see:
# === Setting up SSH access to ubuntu@vm1.aws.com ===
# Generating SSH key pair: /home/user/.ssh/sai3bench_id_rsa
# ✓ SSH key generated
# Copying SSH key to ubuntu@vm1.aws.com
# Password:  [enter VM password]
# ✓ SSH key copied
# ✓ Passwordless SSH access verified
# ✓ Docker found: Docker version 24.0.6
# ✓ Host vm1.aws.com is ready
# 
# [repeats for vm2...]
#
# === Setup Summary ===
# ✓ Successfully configured: 2/2
#
# === Next Steps ===
# 1. Update your workload YAML with:
#    distributed:
#      ssh:
#        enabled: true
#        user: ubuntu
#        key_path: /home/user/.ssh/sai3bench_id_rsa
#      agents:
#        - address: vm1.aws.com
#        - address: vm2.aws.com
#
# 2. Run distributed test:
#    sai3bench-ctl run --config your-workload.yaml
```

### Test existing setup

```bash
# Verify SSH keys are working:
sai3bench-ctl ssh-setup --hosts vm1,vm2,vm3 --test-only

# Testing SSH Connectivity
# Testing ubuntu@vm1... ✓ OK
# Testing ubuntu@vm2... ✓ OK  
# Testing ubuntu@vm3... ✗ FAILED
#
# Some hosts failed connectivity test. Run 'ssh-setup' to configure them.
```

### Custom SSH key location

```bash
sai3bench-ctl ssh-setup \
  --hosts vm1.gcp.com,vm2.gcp.com \
  --key-path ~/.ssh/benchmark_key \
  --user benchmark-user
```

## Troubleshooting

### "Permission denied (publickey)"

- Run setup again: `sai3bench-ctl ssh-setup --hosts ...`
- Verify key exists: `ls -la ~/.ssh/sai3bench_id_rsa`
- Test manually: `ssh -i ~/.ssh/sai3bench_id_rsa user@host`

### "ssh-copy-id: command not found"

Install openssh-client:
```bash
# Ubuntu/Debian:
sudo apt-get install openssh-client

# macOS:
brew install openssh
```

### "Docker not found on host"

SSH to the VM and install Docker:
```bash
ssh user@host
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER  # Add user to docker group
exit  # Log out and back in
```

Then re-run ssh-setup to verify.

### Manual Setup

If automation fails, follow these manual steps:

```bash
# 1. Generate key
ssh-keygen -t rsa -b 4096 -f ~/.ssh/sai3bench_id_rsa -N '' -C 'sai3bench-automation'

# 2. Copy to each host
ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub user@host1
ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub user@host2

# 3. Verify
ssh -i ~/.ssh/sai3bench_id_rsa user@host1 echo OK
ssh -i ~/.ssh/sai3bench_id_rsa user@host2 echo OK

# 4. Test Docker
ssh -i ~/.ssh/sai3bench_id_rsa user@host1 docker --version
ssh -i ~/.ssh/sai3bench_id_rsa user@host2 docker --version
```

## Security Notes

- **No passphrase**: The generated key has no passphrase for automation
  - Restrict to dedicated benchmark VMs only
  - Use IAM roles for cloud credentials instead of long-lived keys
  - Consider SSH agent forwarding for interactive use

- **Key permissions**: Automatically set to 0600 (owner read/write only)

- **Host key checking**: First connection auto-accepts host keys
  - Vulnerable to MITM on first connect
  - For production, pre-populate `known_hosts` manually

## Integration with Workload Config

After running `ssh-setup`, update your YAML:

```yaml
distributed:
  ssh:
    enabled: true
    user: "ubuntu"
    key_path: "~/.ssh/sai3bench_id_rsa"
  
  deployment:
    deploy_type: "docker"
    image: "sai3bench:v0.6.11"
  
  agents:
    - address: "vm1.example.com"
    - address: "vm2.example.com"
    - address: "vm3.example.com"
```

Then run:
```bash
sai3bench-ctl run --config workload.yaml
```

The controller will automatically:
1. SSH to each VM
2. Start agent containers
3. Run distributed test
4. Collect results
5. Cleanup containers

Zero manual steps! 🎉
