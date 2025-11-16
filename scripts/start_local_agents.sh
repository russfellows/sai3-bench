#!/bin/bash
# Start N local agents for testing

set -e

# Configuration
NUM_AGENTS=${1:-2}
BASE_PORT=${2:-7761}
VERBOSE=${3:-"-v"}
LOG_DIR=${4:-"/tmp"}

# Ensure we're in the right directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

# Kill any existing agents
echo "Stopping any existing agents..."
pkill -9 sai3bench-agent 2>/dev/null || true
sleep 1

# Start agents
echo "Starting $NUM_AGENTS agents (base port: $BASE_PORT)..."
for i in $(seq 0 $((NUM_AGENTS - 1))); do
    PORT=$((BASE_PORT + i))
    LOG_FILE="$LOG_DIR/agent$((i+1)).log"
    
    echo "  Starting agent $((i+1)) on port $PORT (log: $LOG_FILE)"
    ./target/release/sai3bench-agent $VERBOSE --listen "0.0.0.0:$PORT" > "$LOG_FILE" 2>&1 &
    
    # Give it a moment to start
    sleep 0.5
done

sleep 1

# Verify agents are running
echo ""
echo "Running agents:"
ps aux | grep sai3bench-agent | grep -v grep || echo "  ERROR: No agents running!"

echo ""
echo "Agent addresses for controller:"
for i in $(seq 0 $((NUM_AGENTS - 1))); do
    PORT=$((BASE_PORT + i))
    if [ $i -eq 0 ]; then
        echo -n "  127.0.0.1:$PORT"
    else
        echo -n ",127.0.0.1:$PORT"
    fi
done
echo ""
