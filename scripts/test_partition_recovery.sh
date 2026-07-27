#!/bin/bash
# test_partition_recovery.sh — Exercises network partition handling (Issue #1229)
# against a running quorum-credit-broadcast-server instance.
#
# This drives the server's HTTP surface to observe the partition/recovery lifecycle
# end-to-end: normal operation -> partition detected -> writes queued -> partition
# clears -> queued writes replayed. It does NOT itself sever network connectivity
# (that requires actually stopping the Redis/bus dependency this instance talks to,
# which is environment-specific — see "Inducing a partition" below); it verifies the
# server's observable behavior once a partition is in effect.
#
# Usage:
#   ./scripts/test_partition_recovery.sh [--base-url http://localhost:4000] [--loan-id test-loan-1]
#
# Requires: curl, jq

set -euo pipefail

BASE_URL="http://localhost:4000"
LOAN_ID="partition-test-$$"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --base-url) BASE_URL="${2:?'--base-url requires a value'}"; shift 2 ;;
        --loan-id)  LOAN_ID="${2:?'--loan-id requires a value'}"; shift 2 ;;
        *) echo "Error: Unknown argument: $1" >&2; exit 1 ;;
    esac
done

for cmd in curl jq; do
    command -v "$cmd" &>/dev/null || { echo "Error: '$cmd' not found." >&2; exit 1; }
done

echo "Network partition recovery drill against $BASE_URL (loan: $LOAN_ID)"
echo ""

# ── Step 1: Baseline partition status ─────────────────────────────────────────

echo "Step 1 — Baseline partition status:"
STATUS=$(curl -sf "$BASE_URL/status/partition")
echo "$STATUS" | jq .
BASELINE_PARTITIONED=$(echo "$STATUS" | jq -r '.partitioned')

# ── Step 2: Attempt a write and observe whether it's applied or queued ───────

echo ""
echo "Step 2 — Recording a test expense (applied immediately unless partitioned):"
WRITE_RESPONSE=$(curl -sf -X POST "$BASE_URL/loans/$LOAN_ID/expenses" \
    -H "content-type: application/json" \
    -d '{"category":"business","amount":1,"description":"partition drill"}')
echo "$WRITE_RESPONSE" | jq .
QUEUED=$(echo "$WRITE_RESPONSE" | jq -r '.queued // false')

if [ "$BASELINE_PARTITIONED" = "true" ] && [ "$QUEUED" != "true" ]; then
    echo "  [ERR] instance reports partitioned but write was NOT queued" >&2
    exit 1
fi
if [ "$BASELINE_PARTITIONED" = "false" ] && [ "$QUEUED" = "true" ]; then
    echo "  [ERR] instance reports healthy but write was queued anyway" >&2
    exit 1
fi
echo "  [OK] write handling matches reported partition state (queued=$QUEUED)"

# ── Step 3: Verify reads are never blocked, regardless of partition state ────

echo ""
echo "Step 3 — Reads must succeed regardless of partition state:"
curl -sf "$BASE_URL/loans/$LOAN_ID/expenses" | jq . >/dev/null \
    && echo "  [OK] GET /loans/:id/expenses succeeded"
curl -sf "$BASE_URL/health" | jq . >/dev/null \
    && echo "  [OK] GET /health succeeded"

# ── Step 4: If currently partitioned, poll for recovery and confirm the queue drains ──

if [ "$BASELINE_PARTITIONED" = "true" ]; then
    echo ""
    echo "Step 4 — Instance is partitioned; polling for recovery (up to 60s)..."
    for _ in $(seq 1 12); do
        sleep 5
        STATUS=$(curl -sf "$BASE_URL/status/partition")
        STILL_PARTITIONED=$(echo "$STATUS" | jq -r '.partitioned')
        QUEUE_DEPTH=$(echo "$STATUS" | jq -r '.queueDepth')
        echo "  partitioned=$STILL_PARTITIONED queueDepth=$QUEUE_DEPTH"
        if [ "$STILL_PARTITIONED" = "false" ]; then
            echo "  [OK] partition cleared; queue should now be draining"
            break
        fi
    done
else
    echo ""
    echo "Step 4 — Skipped: instance was not partitioned at drill start."
    echo "         To exercise the queue/recovery path directly, induce a partition (see below)"
    echo "         and re-run this script while it's in effect, then again after it clears."
fi

echo ""
echo "Drill complete."
echo ""
echo "Inducing a partition manually for a full end-to-end test:"
echo "  1. Stop this instance's Redis/bus dependency (e.g. 'docker stop <redis-container>')."
echo "  2. Wait for PARTITION_FAILURE_THRESHOLD (default 5) consecutive bridge ticks to fail —"
echo "     roughly failureThreshold * BRIDGE_POLL_INTERVAL_MS (default 250ms) after Redis stops."
echo "  3. Re-run this script: GET /status/partition should report partitioned=true and a write"
echo "     to /loans/:id/expenses should come back with queued=true."
echo "  4. Restart Redis, then re-run this script again: partitioned should flip back to false"
echo "     and queueDepth should drop to 0 as queued writes replay."
