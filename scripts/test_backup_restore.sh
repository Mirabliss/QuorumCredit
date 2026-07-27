#!/bin/bash
# test_backup_restore.sh — Automated backup/restore testing for QuorumCredit (Issue #1225).
#
# Runs backup.sh against the target network, restores the result into a
# disposable staging contract via restore.sh, and verifies the restored state
# is correct before recording a pass/fail record. Intended to run weekly (see
# .github/workflows/backup-restore-test.yml) so restore capability is proven
# on a schedule instead of only being discovered to be broken during a real
# incident.
#
# This script tests the MECHANICS of backup + restore (archive integrity,
# manifest completeness, scenario 6 replay), not disaster judgment calls
# (scenarios 1-5 in restore.sh remain operator-guided runbooks — see
# docs/backup-recovery-guide.md).
#
# Usage:
#   ./scripts/test_backup_restore.sh [--network <network>]
#
# Required environment variables (or .env entries):
#   CONTRACT_ID          — Contract to back up (typically a staging deployment)
#   ADMIN_KEY            — Secret key for read-only queries (S...)
#
# Optional environment variables:
#   NETWORK               — Stellar network (default: testnet)
#   STAGING_CONTRACT_ID   — Contract to restore INTO for the restore drill (default: same as CONTRACT_ID,
#                           dry-run only in that case — see below)
#   STAGING_ADMIN_KEY     — Admin key for the staging restore target (default: ADMIN_KEY)
#   BACKUP_TEST_HISTORY   — Path to the JSONL coverage/success-rate log (default: backups/test-results/history.jsonl)
#   BACKUP_TEST_EXECUTE   — "true" to actually --execute the restore against STAGING_CONTRACT_ID (default: false,
#                           i.e. dry-run only). Only set this when STAGING_CONTRACT_ID is a real disposable
#                           staging deployment — never mainnet.
#
# Exit code is non-zero if the drill fails, so this can gate CI / page on-call.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ENV_FILE="$PROJECT_ROOT/.env"
if [ -f "$ENV_FILE" ]; then
    set -o allexport
    # shellcheck source=/dev/null
    source "$ENV_FILE"
    set +o allexport
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --network) NETWORK="${2:?'--network requires a value'}"; shift 2 ;;
        *) echo "Error: Unknown argument: $1" >&2; exit 1 ;;
    esac
done

NETWORK="${NETWORK:-testnet}"
STAGING_CONTRACT_ID="${STAGING_CONTRACT_ID:-${CONTRACT_ID:-}}"
STAGING_ADMIN_KEY="${STAGING_ADMIN_KEY:-${ADMIN_KEY:-}}"
BACKUP_TEST_HISTORY="${BACKUP_TEST_HISTORY:-$PROJECT_ROOT/backups/test-results/history.jsonl}"
BACKUP_TEST_EXECUTE="${BACKUP_TEST_EXECUTE:-false}"

mkdir -p "$(dirname "$BACKUP_TEST_HISTORY")"

for var in CONTRACT_ID ADMIN_KEY; do
    if [ -z "${!var:-}" ]; then
        echo "Error: $var is not set." >&2
        exit 1
    fi
done

if ! command -v jq &>/dev/null; then
    echo "Error: 'jq' not found." >&2
    exit 1
fi

START_TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
START_EPOCH=$(date -u +%s)
RUN_ID="drill_$(date -u +%Y%m%d_%H%M%SZ)"
FAILURE_REASON=""
STATUS="pass"

echo "QuorumCredit backup/restore test drill — $RUN_ID"
echo "  Network            : $NETWORK"
echo "  Source contract     : $CONTRACT_ID"
echo "  Staging contract    : ${STAGING_CONTRACT_ID:-<none>}"
echo "  Execute restore     : $BACKUP_TEST_EXECUTE"
echo ""

# ── Step 1: Take a fresh backup ────────────────────────────────────────────────

echo "Step 1/4 — Running backup.sh..."
BACKUP_LOG=$(mktemp)
if CONTRACT_ID="$CONTRACT_ID" ADMIN_KEY="$ADMIN_KEY" NETWORK="$NETWORK" \
    "$SCRIPT_DIR/backup.sh" > "$BACKUP_LOG" 2>&1; then
    echo "  [OK] backup.sh completed"
else
    STATUS="fail"
    FAILURE_REASON="backup.sh exited non-zero"
    echo "  [ERR] backup.sh failed — see below" >&2
    cat "$BACKUP_LOG" >&2
fi

LATEST_BACKUP_DIR=$(find "$PROJECT_ROOT/backups" -maxdepth 1 -mindepth 1 -type d -name '2*' | sort | tail -1)
LATEST_ARCHIVE=$(find "$PROJECT_ROOT/backups" -maxdepth 1 -name 'backup_*.tar.gz' | sort | tail -1)

# ── Step 2: Verify manifest completeness + checksum ────────────────────────────

COMPLETE="unknown"
CHECKSUM_OK="false"
if [ "$STATUS" = "pass" ] && [ -n "$LATEST_BACKUP_DIR" ] && [ -f "$LATEST_BACKUP_DIR/manifest.json" ]; then
    echo ""
    echo "Step 2/4 — Verifying manifest completeness and checksum..."
    MANIFEST="$LATEST_BACKUP_DIR/manifest.json"
    COMPLETE=$(jq -r '.completeness.complete' "$MANIFEST")
    RECORDED_CHECKSUM=$(jq -r '.checksum' "$MANIFEST" | sed 's/^sha256://')
    RECOMPUTED_CHECKSUM=$(find "$LATEST_BACKUP_DIR" -type f ! -name "manifest.json" | sort | xargs sha256sum | sha256sum | cut -d' ' -f1)

    if [ "$RECORDED_CHECKSUM" = "$RECOMPUTED_CHECKSUM" ]; then
        CHECKSUM_OK="true"
        echo "  [OK] checksum matches recomputed value"
    else
        CHECKSUM_OK="false"
        STATUS="fail"
        FAILURE_REASON="manifest checksum mismatch (backup corrupted or truncated in transit)"
        echo "  [ERR] checksum mismatch: manifest=$RECORDED_CHECKSUM recomputed=$RECOMPUTED_CHECKSUM" >&2
    fi

    if [ "$COMPLETE" = "false" ]; then
        STATUS="fail"
        FAILURE_REASON="${FAILURE_REASON:+$FAILURE_REASON; }backup completeness proof reported incomplete address coverage"
        echo "  [ERR] backup reports incomplete address coverage (see completeness.missing_estimate)" >&2
    else
        echo "  [OK] completeness = $COMPLETE"
    fi

    # Required top-level snapshot files a restorable backup must contain — the
    # minimum an operator needs for scenarios 1-4 in restore.sh.
    for required_file in config.json admins.json paused.json health.json; do
        if [ ! -f "$LATEST_BACKUP_DIR/$required_file" ]; then
            STATUS="fail"
            FAILURE_REASON="${FAILURE_REASON:+$FAILURE_REASON; }missing required snapshot file: $required_file"
            echo "  [ERR] required snapshot file missing: $required_file" >&2
        fi
    done
else
    if [ "$STATUS" = "pass" ]; then
        STATUS="fail"
        FAILURE_REASON="no manifest.json produced by backup.sh"
    fi
fi

# ── Step 3: Restore drill against staging ──────────────────────────────────────

RESTORE_LOG=$(mktemp)
RESTORE_OK="skipped"
if [ "$STATUS" = "pass" ] && [ -n "$STAGING_CONTRACT_ID" ] && [ -n "$LATEST_ARCHIVE" ]; then
    echo ""
    echo "Step 3/4 — Restoring into staging ($STAGING_CONTRACT_ID)..."
    RESTORE_ARGS=(--backup "$LATEST_ARCHIVE" --network "$NETWORK" --scenario 6)
    if [ "$BACKUP_TEST_EXECUTE" = "true" ]; then
        RESTORE_ARGS+=(--execute)
    fi

    if CONTRACT_ID="$STAGING_CONTRACT_ID" ADMIN_KEY="${STAGING_ADMIN_KEY:-$ADMIN_KEY}" \
        RESTORE_STATE_FILE="$LATEST_BACKUP_DIR/.restore-test.state.json" \
        "$SCRIPT_DIR/restore.sh" "${RESTORE_ARGS[@]}" > "$RESTORE_LOG" 2>&1; then
        RESTORE_OK="true"
        echo "  [OK] restore.sh scenario 6 completed against staging"
    else
        RESTORE_OK="false"
        STATUS="fail"
        FAILURE_REASON="${FAILURE_REASON:+$FAILURE_REASON; }restore.sh scenario 6 failed against staging"
        echo "  [ERR] restore.sh failed — see below" >&2
        cat "$RESTORE_LOG" >&2
    fi
else
    echo ""
    echo "Step 3/4 — Skipped (no STAGING_CONTRACT_ID configured, or backup failed upstream)."
fi

# ── Step 4: Verify restored state correctness ─────────────────────────────────
#
# Re-run a subset of read-only queries against the staging contract after the
# restore drill and diff them against the backed-up values. This is what
# actually proves restore capability — a restore that "completes" but leaves
# staging state diverging from the backup would otherwise look like a pass.

CORRECTNESS="skipped"
if [ "$STATUS" = "pass" ] && [ "$RESTORE_OK" = "true" ] && [ "$BACKUP_TEST_EXECUTE" = "true" ]; then
    echo ""
    echo "Step 4/4 — Verifying restored state correctness..."
    if command -v stellar &>/dev/null; then
        BACKUP_PAUSED=$(jq -c . "$LATEST_BACKUP_DIR/paused.json" 2>/dev/null || echo "null")
        STAGING_PAUSED=$(stellar contract invoke --id "$STAGING_CONTRACT_ID" \
            --source "${STAGING_ADMIN_KEY:-$ADMIN_KEY}" --network "$NETWORK" \
            -- get_paused 2>/dev/null || echo "null")
        if [ "$BACKUP_PAUSED" = "$STAGING_PAUSED" ]; then
            CORRECTNESS="true"
            echo "  [OK] restored paused-state matches backup"
        else
            CORRECTNESS="false"
            STATUS="fail"
            FAILURE_REASON="${FAILURE_REASON:+$FAILURE_REASON; }restored state diverges from backup (get_paused mismatch)"
            echo "  [ERR] restored state mismatch: backup=$BACKUP_PAUSED staging=$STAGING_PAUSED" >&2
        fi
    fi
else
    echo ""
    echo "Step 4/4 — Skipped (dry-run mode or no successful execute-restore to verify)."
fi

END_EPOCH=$(date -u +%s)
DURATION_SECONDS=$((END_EPOCH - START_EPOCH))

# ── Record result to history for coverage/success-rate tracking ──────────────

RESULT_JSON=$(jq -n \
    --arg run_id "$RUN_ID" \
    --arg timestamp "$START_TS" \
    --arg network "$NETWORK" \
    --arg contract_id "$CONTRACT_ID" \
    --arg staging_contract_id "${STAGING_CONTRACT_ID:-}" \
    --arg status "$STATUS" \
    --arg failure_reason "$FAILURE_REASON" \
    --arg completeness "$COMPLETE" \
    --arg checksum_ok "$CHECKSUM_OK" \
    --arg restore_ok "$RESTORE_OK" \
    --arg correctness "$CORRECTNESS" \
    --argjson duration_seconds "$DURATION_SECONDS" \
    '{run_id: $run_id, timestamp: $timestamp, network: $network, contract_id: $contract_id,
      staging_contract_id: $staging_contract_id, status: $status, failure_reason: $failure_reason,
      completeness: $completeness, checksum_ok: $checksum_ok, restore_ok: $restore_ok,
      correctness: $correctness, duration_seconds: $duration_seconds}')

echo "$RESULT_JSON" >> "$BACKUP_TEST_HISTORY"

# ── Coverage / success-rate summary over recorded history ────────────────────

TOTAL_RUNS=$(wc -l < "$BACKUP_TEST_HISTORY" | tr -d ' ')
PASS_RUNS=$(grep -c '"status":"pass"' "$BACKUP_TEST_HISTORY" || true)
SUCCESS_RATE=$(awk -v p="$PASS_RUNS" -v t="$TOTAL_RUNS" 'BEGIN { printf "%.1f", (t > 0 ? (p / t * 100) : 0) }')

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "Drill result: $STATUS"
[ -n "$FAILURE_REASON" ] && echo "  Reason: $FAILURE_REASON"
echo "  Duration: ${DURATION_SECONDS}s"
echo "  History : $BACKUP_TEST_HISTORY"
echo "  Coverage: $PASS_RUNS/$TOTAL_RUNS runs passed all-time ($SUCCESS_RATE% success rate)"
echo "═══════════════════════════════════════════════════════════"

rm -f "$BACKUP_LOG" "$RESTORE_LOG"

if [ "$STATUS" != "pass" ]; then
    exit 1
fi
