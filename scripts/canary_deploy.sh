#!/bin/bash
# canary_deploy.sh — Progressive deployment with canary releases (Issue #1231).
#
# Replaces a big-bang deploy with a staged traffic shift: deploy a canary instance,
# send it a small slice of traffic, watch its error rate and latency for a window,
# and only widen the slice (or roll back) based on what's observed — automatically.
#
# This controls TRAFFIC WEIGHT DECISIONS and the promote/rollback logic; it does not
# itself implement a load balancer. Applying a weight is delegated to a pluggable
# adapter script (--lb-adapter) — a single, obvious seam, since this repo has no load
# balancer / ingress config to integrate against. Without an adapter, weights are
# printed as instructions and the monitoring/decision logic still runs (useful for
# dry-running the thresholds in CI without real infra).
#
# Usage:
#   ./scripts/canary_deploy.sh --canary-url <url> [options]
#
# Required:
#   --canary-url <url>       Base URL of the canary instance (must expose /metrics, /health)
#
# Options:
#   --stable-url <url>       Base URL of a stable instance, for relative comparison
#                            (canary error rate vs stable error rate) in addition to
#                            the absolute thresholds below. Optional but recommended.
#   --steps <list>           Space or comma separated traffic percentages to progress
#                            through, in order (default: "5 25 50 100")
#   --window-seconds <n>     Observation window at each step before deciding, in
#                            seconds (default: 60)
#   --max-error-rate <f>     Absolute error-rate ceiling (5xx / total requests) over
#                            the window, as a fraction (default: 0.05 = 5%)
#   --max-latency-ms <n>     Absolute average-latency ceiling in ms over the window
#                            (default: 500)
#   --max-error-rate-multiplier <f>
#                            When --stable-url is set: canary's error rate must not
#                            exceed stable's by more than this multiplier (default: 2)
#   --lb-adapter <path>      Executable invoked as `<path> set-weight <percent>` to
#                            actually shift traffic. Without this, weights are printed
#                            as operator instructions instead of applied.
#   --deploy-cmd <cmd>       Shell command to deploy/start the canary instance before
#                            the first traffic-shift step (optional — assumes the
#                            canary is already running if omitted).
#   --history <path>         JSONL log of every step's decision
#                            (default: deploy/canary-history.jsonl)
#
# Exit code is non-zero if the rollout was rolled back at any step.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CANARY_URL=""
STABLE_URL=""
STEPS="5 25 50 100"
WINDOW_SECONDS=60
MAX_ERROR_RATE=0.05
MAX_LATENCY_MS=500
MAX_ERROR_RATE_MULTIPLIER=2
LB_ADAPTER=""
DEPLOY_CMD=""
HISTORY="$PROJECT_ROOT/deploy/canary-history.jsonl"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --canary-url) CANARY_URL="${2:?}"; shift 2 ;;
        --stable-url) STABLE_URL="${2:?}"; shift 2 ;;
        --steps) STEPS="${2:?}"; shift 2 ;;
        --window-seconds) WINDOW_SECONDS="${2:?}"; shift 2 ;;
        --max-error-rate) MAX_ERROR_RATE="${2:?}"; shift 2 ;;
        --max-latency-ms) MAX_LATENCY_MS="${2:?}"; shift 2 ;;
        --max-error-rate-multiplier) MAX_ERROR_RATE_MULTIPLIER="${2:?}"; shift 2 ;;
        --lb-adapter) LB_ADAPTER="${2:?}"; shift 2 ;;
        --deploy-cmd) DEPLOY_CMD="${2:?}"; shift 2 ;;
        --history) HISTORY="${2:?}"; shift 2 ;;
        *) echo "Error: Unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [ -z "$CANARY_URL" ]; then
    echo "Error: --canary-url is required." >&2
    exit 1
fi

for cmd in curl jq awk; do
    command -v "$cmd" &>/dev/null || { echo "Error: '$cmd' not found." >&2; exit 1; }
done

mkdir -p "$(dirname "$HISTORY")"
STEP_LIST=$(echo "$STEPS" | tr ',' ' ')

log_decision() {
    local step="$1" decision="$2" error_rate="$3" latency_ms="$4" reason="${5:-}"
    jq -n \
        --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --argjson step "$step" \
        --arg decision "$decision" \
        --arg error_rate "$error_rate" \
        --arg latency_ms "$latency_ms" \
        --arg reason "$reason" \
        --arg canary_url "$CANARY_URL" \
        '{timestamp:$timestamp, step_percent:$step, decision:$decision, error_rate:($error_rate|tonumber), latency_ms:($latency_ms|tonumber), reason:$reason, canary_url:$canary_url}' \
        >> "$HISTORY"
}

apply_weight() {
    local percent="$1"
    if [ -n "$LB_ADAPTER" ]; then
        echo "  Applying weight via adapter: $LB_ADAPTER set-weight $percent"
        "$LB_ADAPTER" set-weight "$percent"
    else
        echo "  [MANUAL] No --lb-adapter configured — shift ${percent}% of traffic to canary ($CANARY_URL) now."
    fi
}

rollback() {
    local reason="$1"
    echo "" >&2
    echo "ROLLBACK: $reason" >&2
    apply_weight 0
    echo "  Rolled back to 0% canary traffic." >&2
}

# Prometheus counter/gauge line value, or 0 if absent (metrics registry is
# dependency-free hand-rolled exposition — see server/src/http/metricsRegistry.ts).
metric_value() {
    local text="$1" name="$2"
    echo "$text" | awk -v n="$name" '$1 == n { print $2; found=1 } END { if (!found) print 0 }'
}

sample_metrics() {
    local url="$1"
    curl -sf "$url/metrics" 2>/dev/null || echo ""
}

# Prints "error_rate avg_latency_ms" for the delta between two metrics snapshots.
compute_window_stats() {
    local before="$1" after="$2"
    local req_before req_after err_before err_after dur_before dur_after cnt_before cnt_after
    req_before=$(metric_value "$before" "qc_http_requests_total")
    req_after=$(metric_value "$after" "qc_http_requests_total")
    err_before=$(metric_value "$before" "qc_http_request_errors_total")
    err_after=$(metric_value "$after" "qc_http_request_errors_total")
    dur_before=$(metric_value "$before" "qc_http_request_duration_ms_sum")
    dur_after=$(metric_value "$after" "qc_http_request_duration_ms_sum")
    cnt_before=$(metric_value "$before" "qc_http_request_duration_ms_count")
    cnt_after=$(metric_value "$after" "qc_http_request_duration_ms_count")

    awk -v rb="$req_before" -v ra="$req_after" -v eb="$err_before" -v ea="$err_after" \
        -v db="$dur_before" -v da="$dur_after" -v cb="$cnt_before" -v ca="$cnt_after" \
        'BEGIN {
            req = ra - rb; err = ea - eb; dur = da - db; cnt = ca - cb;
            error_rate = (req > 0) ? err / req : 0;
            latency_ms = (cnt > 0) ? dur / cnt : 0;
            printf "%.6f %.2f", error_rate, latency_ms;
        }'
}

# ── Optional: deploy the canary instance ──────────────────────────────────────

if [ -n "$DEPLOY_CMD" ]; then
    echo "Deploying canary instance: $DEPLOY_CMD"
    if ! eval "$DEPLOY_CMD"; then
        echo "Error: --deploy-cmd failed; aborting before any traffic is shifted." >&2
        exit 1
    fi
fi

echo "Canary rollout: $CANARY_URL"
[ -n "$STABLE_URL" ] && echo "Stable baseline: $STABLE_URL"
echo "Steps: $STEP_LIST"
echo "Window: ${WINDOW_SECONDS}s | max error rate: $MAX_ERROR_RATE | max latency: ${MAX_LATENCY_MS}ms"
echo ""

# ── Step through traffic percentages ──────────────────────────────────────────

for percent in $STEP_LIST; do
    echo "═══ Step: ${percent}% canary traffic ═══"
    apply_weight "$percent"

    echo "  Sampling baseline metrics..."
    CANARY_BEFORE=$(sample_metrics "$CANARY_URL")
    STABLE_BEFORE=""
    [ -n "$STABLE_URL" ] && STABLE_BEFORE=$(sample_metrics "$STABLE_URL")

    echo "  Observing for ${WINDOW_SECONDS}s..."
    sleep "$WINDOW_SECONDS"

    CANARY_AFTER=$(sample_metrics "$CANARY_URL")
    STABLE_AFTER=""
    [ -n "$STABLE_URL" ] && STABLE_AFTER=$(sample_metrics "$STABLE_URL")

    if [ -z "$CANARY_AFTER" ]; then
        rollback "canary instance became unreachable during the observation window"
        log_decision "$percent" "rollback" "1.0" "0" "canary unreachable"
        exit 1
    fi

    read -r ERROR_RATE LATENCY_MS <<< "$(compute_window_stats "$CANARY_BEFORE" "$CANARY_AFTER")"
    echo "  Canary: error_rate=$ERROR_RATE avg_latency_ms=${LATENCY_MS}ms"

    FAIL_REASON=""
    if awk -v e="$ERROR_RATE" -v m="$MAX_ERROR_RATE" 'BEGIN{exit !(e > m)}'; then
        FAIL_REASON="error rate $ERROR_RATE exceeds absolute ceiling $MAX_ERROR_RATE"
    elif awk -v l="$LATENCY_MS" -v m="$MAX_LATENCY_MS" 'BEGIN{exit !(l > m)}'; then
        FAIL_REASON="avg latency ${LATENCY_MS}ms exceeds ceiling ${MAX_LATENCY_MS}ms"
    elif [ -n "$STABLE_URL" ] && [ -n "$STABLE_AFTER" ]; then
        read -r STABLE_ERROR_RATE STABLE_LATENCY_MS <<< "$(compute_window_stats "$STABLE_BEFORE" "$STABLE_AFTER")"
        echo "  Stable: error_rate=$STABLE_ERROR_RATE avg_latency_ms=${STABLE_LATENCY_MS}ms"
        if awk -v ce="$ERROR_RATE" -v se="$STABLE_ERROR_RATE" -v mult="$MAX_ERROR_RATE_MULTIPLIER" \
            'BEGIN{exit !(se > 0 && ce > se * mult)}'; then
            FAIL_REASON="canary error rate $ERROR_RATE is more than ${MAX_ERROR_RATE_MULTIPLIER}x stable's $STABLE_ERROR_RATE"
        fi
    fi

    if [ -n "$FAIL_REASON" ]; then
        rollback "$FAIL_REASON"
        log_decision "$percent" "rollback" "$ERROR_RATE" "$LATENCY_MS" "$FAIL_REASON"
        exit 1
    fi

    echo "  [OK] within thresholds — proceeding."
    log_decision "$percent" "promote" "$ERROR_RATE" "$LATENCY_MS"
    echo ""
done

echo "Canary rollout complete: 100% traffic on the new version."
echo "History: $HISTORY"
