# Metadata Cache Flush Policy (v0.8.60)

## Overview

The metadata cache uses fjall v3 LSM-tree KV store to track object states during preparation. To prevent blocking I/O operations, writes use configurable flush policies.

## Flush Policies

**Default**: `AsyncInterval(30)` - periodic background flush every 30 seconds

### Three Modes

1. **Immediate** - Synchronous flush after every write (safe but slow)
   ```rust
   cache.set_flush_policy(FlushPolicy::Immediate);
   ```

2. **BatchSize(N)** - Flush when N pending writes accumulate
   ```rust
   cache.set_batch_flush(100);  // Flush every 100 writes
   ```

3. **AsyncInterval(secs)** - Periodic background flush (zero blocking)
   ```rust
   cache.enable_async_flush(30);  // Flush every 30 seconds
   ```

## Agent-Specific Cache Paths (Distributed Mode)

When `num_agents > 1`, each agent gets its own endpoint cache to prevent lock contention:

```
/mnt/test/target/
├── .sai3-cache-agent-0/    # Agent 0's cache (fjall database)
├── .sai3-cache-agent-1/    # Agent 1's cache (fjall database)
└── prepared-000000.dat     # Actual prepared objects (shared)
```

**Single-agent mode** uses `sai3-kv-cache/` (backward compatible).

## Performance Characteristics

- **AsyncInterval mode**: 10K writes in <500ms (zero blocking)
- **Immediate mode**: ~50ms per write (synchronous flush)
- **Batch mode**: Flushes only when threshold reached
- **Atomic counters**: `pending_write_count()` tracks unflushed operations

## Critical Methods

- `maybe_flush()` - Conditional flush based on policy (hot path)
- `force_flush()` - Blocking flush for critical checkpoints (barriers, cleanup)
- `pending_write_count()` - Returns number of unflushed writes

## Implementation Notes

All `persist()` calls replaced with `maybe_flush()` in these methods:
- `plan_object()`, `mark_creating()`, `mark_created()`, `mark_failed()`, `mark_deleted()`

Use `force_flush()` only at critical sync points (stage barriers, final cleanup).
