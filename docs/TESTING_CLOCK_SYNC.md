# Testing Clock Synchronization

## Overview

sai3-bench v0.8.4 implements clock synchronization for distributed workloads to ensure all agents start at exactly the same time. This is critical for accurate benchmarking when using multiple agents.

However, testing this feature with local agents (on the same machine) is difficult because they all share the same system clock. To solve this, we've implemented **simulated clock skew** for testing purposes.

## Simulated Clock Skew

The agent binary supports an optional environment variable `SAI3_AGENT_CLOCK_SKEW_MS` that artificially offsets the agent's reported timestamp. This allows testing the clock synchronization protocol without needing different physical machines.

### Usage

```bash
# Agent with clock 5 seconds ahead
SAI3_AGENT_CLOCK_SKEW_MS=5000 ./target/release/sai3bench-agent --listen 0.0.0.0:7761

# Agent with clock 3 seconds behind (negative value)
SAI3_AGENT_CLOCK_SKEW_MS=-3000 ./target/release/sai3bench-agent --listen 0.0.0.0:7762

# Agent with no skew (normal operation)
./target/release/sai3bench-agent --listen 0.0.0.0:7763
```

When an agent starts with this variable set, it will:
1. Print a test mode message: `[TEST MODE] Simulating clock skew: 5000 ms`
2. Add the offset to all timestamps it reports to the controller
3. Behave exactly as if its system clock had that offset

### How It Works

The `get_agent_timestamp_ns()` helper function in `agent.rs`:
- Gets the current system time (nanoseconds since UNIX_EPOCH)
- Checks for the `SAI3_AGENT_CLOCK_SKEW_MS` environment variable
- If set, adds the offset (in milliseconds converted to nanoseconds)
- Returns the adjusted timestamp

This simulates what would happen if the agent's system clock was genuinely offset by that amount.

## Test Script

We provide an automated test script that verifies clock synchronization with three agents having different clock skews:

```bash
./tests/test_clock_sync.sh
```

This script:
1. Starts 3 agents with different clock skews (+5s, -3s, +1s)
2. Runs a simple file workload via the controller
3. Verifies that the controller detects the clock offsets
4. Checks that all agents start the workload at the same absolute time

### Expected Output

Controller should log messages like:
```
Agent 127.0.0.1:7761 clock offset: 5000123456 ns (5000.12 ms)
Agent 127.0.0.1:7762 clock offset: -2999876543 ns (-2999.88 ms)
Agent 127.0.0.1:7763 clock offset: 1000234567 ns (1000.23 ms)
WARNING: Agent 127.0.0.1:7761 offset > 1000ms, may indicate clock skew
```

All agents should start their workloads within milliseconds of each other (in absolute epoch time), despite their different clock offsets.

## Testing Scenarios

### Scenario 1: Moderate Skew
Test with ±1-2 second offsets (typical of NTP-synced systems):
```bash
SAI3_AGENT_CLOCK_SKEW_MS=1500 ./agent1 &
SAI3_AGENT_CLOCK_SKEW_MS=-800 ./agent2 &
```

### Scenario 2: Large Skew
Test with 5+ second offsets (warning threshold):
```bash
SAI3_AGENT_CLOCK_SKEW_MS=5000 ./agent1 &
SAI3_AGENT_CLOCK_SKEW_MS=-10000 ./agent2 &
```
Controller should log warnings about large offsets.

### Scenario 3: Mixed Production + Test
One agent with skew, others normal:
```bash
SAI3_AGENT_CLOCK_SKEW_MS=3000 ./agent1 &
./agent2 &  # No skew
./agent3 &  # No skew
```

## Production Use

**IMPORTANT**: `SAI3_AGENT_CLOCK_SKEW_MS` is for testing only. Do not use in production.

In production deployments:
- Agents report their actual system time
- No artificial offsets are applied
- Clock synchronization happens automatically
- NTP should be running on all machines for best results

## Architecture Notes

The clock synchronization protocol works as follows:

1. **Agent READY**: Agent sends its timestamp in the READY message
2. **Controller Calculates Offset**: `offset = agent_time - controller_time`
3. **Controller Sets Start Time**: Picks absolute epoch time for workload start
4. **Agents Wait**: Each agent waits until that absolute epoch time
5. **Coordinated Start**: All agents start at the same instant

The key insight is that both controller and agents measure time from UNIX_EPOCH (January 1, 1970). Even if clocks are skewed, the absolute epoch timestamp is the same reference point for everyone.

**Example**:
- Controller (clock at 1000s): "Start at epoch 1010s"
- Agent 1 (clock at 1005s, +5s skew): Waits 5 seconds → starts at epoch 1010s ✓
- Agent 2 (clock at 997s, -3s skew): Waits 13 seconds → starts at epoch 1010s ✓

The clock skew cancels out because both measure against the same absolute reference.

## Verification

To verify the synchronization is working:

1. **Check controller logs** for clock offset calculations
2. **Check agent logs** for coordinated start timing
3. **Examine op-logs** (if enabled) - timestamps should align
4. **Compare workload durations** - should be nearly identical across agents

If agents start at different times, the workload durations will differ significantly, indicating a synchronization problem.

## Future Enhancements

Potential improvements to clock synchronization:
- RTT measurement for more accurate offset calculation
- Periodic re-synchronization during long workloads
- Automatic detection and warning of excessive clock drift
- PTP (Precision Time Protocol) support for sub-millisecond accuracy

## References

- [NTP - Network Time Protocol](https://en.wikipedia.org/wiki/Network_Time_Protocol)
- [PTP - Precision Time Protocol](https://en.wikipedia.org/wiki/Precision_Time_Protocol)
- [Distributed Systems: Clock Synchronization](https://en.wikipedia.org/wiki/Clock_synchronization)
