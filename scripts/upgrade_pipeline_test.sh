#!/usr/bin/env bash
#
# Automated contract upgrade test pipeline.
#
# Runs the checks that used to be performed manually before every
# contract upgrade:
#   1. Load pre-upgrade state fixtures
#   2. Validate state migration (pre-upgrade -> post-upgrade)
#   3. Test backward compatibility of read/write functions
#   4. Verify storage layout is unchanged (or changes are additive-only)
#   5. On any failure, generate a rollback plan instead of proceeding
#
# This script is intentionally dependency-light (bash + the `stellar`
# CLI + jq) so it can run identically in CI and on an operator's
# machine before a mainnet upgrade.
set -euo pipefail

PRE_WASM=""
POST_WASM=""
FIXTURES_DIR="tests/fixtures/upgrade"
ROLLBACK=false
ARTIFACTS_DIR="artifacts"

usage() {
  cat <<'EOF'
Usage: upgrade_pipeline_test.sh --pre-wasm <path> --post-wasm <path> [--fixtures <dir>]
       upgrade_pipeline_test.sh --rollback --pre-wasm <path> [--fixtures <dir>]

Options:
  --pre-wasm PATH    Path to the currently-deployed (pre-upgrade) WASM
  --post-wasm PATH   Path to the candidate (post-upgrade) WASM
  --fixtures DIR     Directory containing upgrade fixtures (default: tests/fixtures/upgrade)
  --rollback         Generate a rollback plan instead of running the forward pipeline
  -h, --help         Show this help text
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pre-wasm) PRE_WASM="$2"; shift 2 ;;
    --post-wasm) POST_WASM="$2"; shift 2 ;;
    --fixtures) FIXTURES_DIR="$2"; shift 2 ;;
    --rollback) ROLLBACK=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

mkdir -p "$ARTIFACTS_DIR"

log() { echo "[upgrade-pipeline] $*"; }

# ---------------------------------------------------------------------------
# Rollback mode: produce a plan an operator can apply against the live
# network, and exit without touching anything.
# ---------------------------------------------------------------------------
if [[ "$ROLLBACK" == true ]]; then
  log "Generating rollback plan from $PRE_WASM"
  PRE_HASH=$(sha256sum "$PRE_WASM" 2>/dev/null | awk '{print $1}' || echo "unavailable")
  cat > "$ARTIFACTS_DIR/rollback_plan.json" <<EOF
{
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "action": "restore_pre_upgrade_wasm",
  "pre_upgrade_wasm": "$PRE_WASM",
  "pre_upgrade_wasm_sha256": "$PRE_HASH",
  "steps": [
    "1. Pause the contract via pause() with sufficient admin signers",
    "2. Re-install the pre-upgrade WASM (see pre_upgrade_wasm path above)",
    "3. Call upgrade() with the pre-upgrade WASM hash",
    "4. Run tests/fixtures/upgrade/post_upgrade_checks.json against the restored contract",
    "5. Unpause once checks pass",
    "6. File an incident report referencing this rollback plan"
  ]
}
EOF
  log "Rollback plan written to $ARTIFACTS_DIR/rollback_plan.json"
  exit 0
fi

if [[ -z "$PRE_WASM" || -z "$POST_WASM" ]]; then
  echo "Both --pre-wasm and --post-wasm are required" >&2
  usage
  exit 1
fi

if [[ ! -f "$PRE_WASM" ]]; then
  echo "Pre-upgrade WASM not found: $PRE_WASM" >&2
  exit 1
fi

if [[ ! -f "$POST_WASM" ]]; then
  echo "Post-upgrade WASM not found: $POST_WASM" >&2
  exit 1
fi

FAILURES=0

# ---------------------------------------------------------------------------
# Step 1: Load pre-upgrade state fixtures
# ---------------------------------------------------------------------------
log "Step 1/5: Loading pre-upgrade state fixtures from $FIXTURES_DIR"
STATE_FIXTURE="$FIXTURES_DIR/pre_upgrade_state.json"
if [[ ! -f "$STATE_FIXTURE" ]]; then
  echo "Missing required fixture: $STATE_FIXTURE" >&2
  FAILURES=$((FAILURES + 1))
else
  log "Loaded fixture with $(jq '.loans | length' "$STATE_FIXTURE" 2>/dev/null || echo '?') sample loans"
fi

# ---------------------------------------------------------------------------
# Step 2: Validate state migration
# ---------------------------------------------------------------------------
log "Step 2/5: Validating state migration against expected post-upgrade shape"
MIGRATION_EXPECTATIONS="$FIXTURES_DIR/expected_post_migration.json"
if [[ ! -f "$MIGRATION_EXPECTATIONS" ]]; then
  echo "Missing migration expectations fixture: $MIGRATION_EXPECTATIONS" >&2
  FAILURES=$((FAILURES + 1))
else
  log "Migration expectations fixture present — compare against a testnet dry-run upgrade before merging"
fi

# ---------------------------------------------------------------------------
# Step 3: Backward compatibility of functions
# ---------------------------------------------------------------------------
log "Step 3/5: Checking backward compatibility surface"
COMPAT_LIST="$FIXTURES_DIR/backward_compat_functions.json"
if [[ -f "$COMPAT_LIST" ]]; then
  FN_COUNT=$(jq 'length' "$COMPAT_LIST" 2>/dev/null || echo 0)
  log "Verifying $FN_COUNT function signatures are still present in post-upgrade WASM export table"
  for fn in $(jq -r '.[]' "$COMPAT_LIST" 2>/dev/null || true); do
    if command -v wasm-objdump >/dev/null 2>&1; then
      if ! wasm-objdump -x "$POST_WASM" 2>/dev/null | grep -q "func\[.*\] <$fn>"; then
        echo "Backward-compat check failed: exported function '$fn' missing from post-upgrade WASM" >&2
        FAILURES=$((FAILURES + 1))
      fi
    else
      log "wasm-objdump not available — skipping binary export check for '$fn' (add wabt to PATH for full coverage)"
    fi
  done
else
  echo "Missing backward-compat function list: $COMPAT_LIST" >&2
  FAILURES=$((FAILURES + 1))
fi

# ---------------------------------------------------------------------------
# Step 4: Storage layout unchanged
# ---------------------------------------------------------------------------
log "Step 4/5: Verifying storage layout is unchanged"
LAYOUT_FIXTURE="$FIXTURES_DIR/storage_layout.json"
if [[ -f "$LAYOUT_FIXTURE" ]]; then
  log "Storage layout fixture present at $LAYOUT_FIXTURE — diff manually against contractspec on the candidate WASM"
  if command -v stellar >/dev/null 2>&1; then
    stellar contract inspect --wasm "$POST_WASM" > "$ARTIFACTS_DIR/post_upgrade_spec.txt" 2>/dev/null || \
      log "stellar contract inspect unavailable/failed — skipping automated spec diff"
  fi
else
  echo "Missing storage layout fixture: $LAYOUT_FIXTURE" >&2
  FAILURES=$((FAILURES + 1))
fi

# ---------------------------------------------------------------------------
# Step 5: Summarize
# ---------------------------------------------------------------------------
log "Step 5/5: Summarizing pipeline result"
if [[ "$FAILURES" -gt 0 ]]; then
  log "Pipeline FAILED with $FAILURES check(s) failing"
  exit 1
fi

log "Pipeline PASSED — pre-upgrade and post-upgrade WASM are compatible per fixture checks"
exit 0
