# Scale-Out vs Scale-Up Testing Guide

## Quick Reference

| Strategy | VMs | Containers/VM | Total Agents | Network BW | Cost | Use Case |
|----------|-----|---------------|--------------|------------|------|----------|
| **Scale-Out** | 8 | 1 | 8 | 8× | Higher | Max throughput, fault tolerance |
| **Scale-Up** | 1 | 8 | 8 | 1× | Lower | Cost optimization, low latency |
| **Hybrid** | 4 | 2 | 8 | 4× | Medium | Balanced approach |

## Example Scenarios

### Scenario 1: High-Throughput Cloud Storage Testing

**Goal**: Saturate network bandwidth to cloud storage

```yaml
# Scale-Out: 8 VMs × n2-standard-8 = 64 vCPU, 8 NICs
# Result: ~20 Gbps aggregate bandwidth to GCS
target: "gs://benchmark-bucket/"
distributed:
  agents:
    - { address: "vm1:7167", id: "agent-1" }
    - { address: "vm2:7167", id: "agent-2" }
    # ... vm3-vm8
```

**Why scale-out**: Each VM has dedicated NIC → 8× network bandwidth

### Scenario 2: Cost-Optimized CPU-Intensive Workload

**Goal**: Maximize CPU utilization, minimize cloud costs

```yaml
# Scale-Up: 1 VM × n2-standard-64 = 64 vCPU, 1 NIC, 8 containers
# Result: Lower cost than 8× n2-standard-8, same CPU
target: "gs://benchmark-bucket/"
distributed:
  agents:
    - { address: "big-vm:7167", id: "c1", listen_port: 7167 }
    - { address: "big-vm:7168", id: "c2", listen_port: 7168 }
    # ... c3-c8
```

**Why scale-up**: 1 large VM typically 10-20% cheaper than 8 small VMs

### Scenario 3: Multi-Region Latency Testing

**Goal**: Test from multiple geographic locations

```yaml
# Scale-Out: VMs in different regions
distributed:
  agents:
    - { address: "us-east-vm:7167", id: "us-east" }
    - { address: "us-west-vm:7167", id: "us-west" }
    - { address: "eu-west-vm:7167", id: "eu-west" }
    - { address: "ap-south-vm:7167", id: "ap-south" }
```

**Why scale-out**: Must use separate VMs in different regions

### Scenario 4: Hybrid Approach

**Goal**: Balance cost, throughput, and flexibility

```yaml
# 4 VMs × 2 containers = 8 agents, 4 NICs
distributed:
  agents:
    # VM 1: 2 containers
    - { address: "vm1:7167", id: "vm1-c1", listen_port: 7167 }
    - { address: "vm1:7168", id: "vm1-c2", listen_port: 7168 }
    # VM 2: 2 containers
    - { address: "vm2:7167", id: "vm2-c1", listen_port: 7167 }
    - { address: "vm2:7168", id: "vm2-c2", listen_port: 7168 }
    # VM 3-4: similar
```

**Why hybrid**: Good balance of network bandwidth, cost, and deployment complexity

## Performance Characteristics

### Network Bandwidth

- **Scale-Out (8 VMs)**: 8× 10 Gbps = 80 Gbps theoretical
- **Scale-Up (1 VM)**: 1× 10 Gbps = 10 Gbps theoretical
- **Real-world**: Cloud storage often bottleneck before network

### CPU Utilization

- **Scale-Out**: Better NUMA locality per VM
- **Scale-Up**: More efficient CPU cache sharing
- **Difference**: Usually < 5% for I/O-bound workloads

### Cost Comparison (GCP example)

- **8× n2-standard-8**: 8 × $0.3896/hr = $3.12/hr
- **1× n2-standard-64**: $3.1168/hr (≈ $0.20/hr savings = 6% cheaper)
- **Hybrid (4× n2-standard-16)**: 4 × $0.7792/hr = $3.12/hr (same cost, 4× bandwidth)

### Latency

- **Scale-Out**: Higher inter-agent latency (irrelevant for independent workloads)
- **Scale-Up**: Lower container-to-container latency (rarely matters)
- **Impact**: Negligible for cloud storage testing

## When to Use Each Strategy

### Use Scale-Out When

✅ Network bandwidth is critical  
✅ Testing from multiple regions  
✅ Need fault tolerance (one VM failure doesn't stop all)  
✅ Comparing geographic latency differences  
✅ Testing distributed coordination overhead  

### Use Scale-Up When

✅ Cost optimization is priority  
✅ Single-region testing  
✅ CPU-bound workloads  
✅ Simpler deployment/management  
✅ Testing maximum per-node load  

### Use Hybrid When

✅ Need balanced network bandwidth and cost  
✅ Fault tolerance with some redundancy  
✅ Flexible capacity scaling  
✅ Testing both horizontal and vertical scaling  

## Configuration Templates

### Scale-Out (8 VMs)

```yaml
# See examples/distributed-scale-out.yaml
distributed:
  agents:
    - { address: "vm1:7167", id: "agent-1" }
    - { address: "vm2:7167", id: "agent-2" }
    - { address: "vm3:7167", id: "agent-3" }
    - { address: "vm4:7167", id: "agent-4" }
    - { address: "vm5:7167", id: "agent-5" }
    - { address: "vm6:7167", id: "agent-6" }
    - { address: "vm7:7167", id: "agent-7" }
    - { address: "vm8:7167", id: "agent-8" }
```

### Scale-Up (1 VM, 8 containers)

```yaml
# See examples/distributed-scale-up.yaml
distributed:
  agents:
    - { address: "big-vm:7167", id: "c1", listen_port: 7167 }
    - { address: "big-vm:7168", id: "c2", listen_port: 7168 }
    - { address: "big-vm:7169", id: "c3", listen_port: 7169 }
    - { address: "big-vm:7170", id: "c4", listen_port: 7170 }
    - { address: "big-vm:7171", id: "c5", listen_port: 7171 }
    - { address: "big-vm:7172", id: "c6", listen_port: 7172 }
    - { address: "big-vm:7173", id: "c7", listen_port: 7173 }
    - { address: "big-vm:7174", id: "c8", listen_port: 7174 }
```

### Hybrid (4 VMs, 2 containers each)

```yaml
distributed:
  agents:
    - { address: "vm1:7167", id: "vm1-c1", listen_port: 7167 }
    - { address: "vm1:7168", id: "vm1-c2", listen_port: 7168 }
    - { address: "vm2:7167", id: "vm2-c1", listen_port: 7167 }
    - { address: "vm2:7168", id: "vm2-c2", listen_port: 7168 }
    - { address: "vm3:7167", id: "vm3-c1", listen_port: 7167 }
    - { address: "vm3:7168", id: "vm3-c2", listen_port: 7168 }
    - { address: "vm4:7167", id: "vm4-c1", listen_port: 7167 }
    - { address: "vm4:7168", id: "vm4-c2", listen_port: 7168 }
```

## Testing Workflow

1. **Start with scale-up locally**: Test config with `local_docker_test.sh`
2. **Deploy scale-out**: Use cloud VMs for realistic network testing
3. **Compare results**: Analyze throughput, latency, cost trade-offs
4. **Optimize**: Choose strategy based on your specific requirements

## Advanced: Dynamic Scaling

You can even combine both strategies dynamically:

```bash
# Morning: Scale-up (1 big VM) for development testing
sai3bench-ctl run --config scale-up-dev.yaml

# Afternoon: Scale-out (8 VMs) for load testing
sai3bench-ctl run --config scale-out-prod.yaml

# Results directory automatically captures which strategy was used
```

Both patterns use identical workload definitions - only the `distributed.agents` section changes!
