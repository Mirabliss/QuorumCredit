# Synthetic Monitoring Guide

> Issue #1236 — Periodic synthetic health checks for QuorumCredit.

---

## Overview

Synthetic monitoring runs a scripted loan lifecycle (vouch → request_loan → repay) against the live contract every **5 minutes**. Failures are detected before real users are impacted.

The system provides:

- **Three-step lifecycle check** — vouch, request_loan, repay
- **Health status** — `Healthy`, `Degraded`, `Unhealthy`, `Unknown`
- **Success-rate tracking** — rolling rate over the last 288 checks (24 hours)
- **Soroban event emission** — `synthetic/check` events consumed by the indexer
- **Alert integration** — indexer forwards failures to PagerDuty / Slack

---

## Health Status

| Status | Meaning | Action |
|---|---|---|
| `Healthy` | All 3 lifecycle steps passed | None |
| `Degraded` | At least one non-critical step failed | Investigate within 30 min |
| `Unhealthy` | A critical step (loan / repay) failed | Page on-call immediately |
| `Unknown` | No check has been run yet | Start the scheduler |

---

## Architecture

```
External Scheduler (cron / CI)
      │  every 5 minutes
      │  stellar contract invoke --fn run_synthetic_check
      ▼
QuorumCredit Contract (src/synthetic_monitoring.rs)
      │  executes 3 lifecycle steps
      │  stores SyntheticCheckResult in persistent storage
      │  emits synthetic/check event
      ▼
Indexer (tools/indexer/src/indexer.rs)
      │  reads synthetic/check events
      │  pushes to Prometheus / Grafana
      │  alerts PagerDuty / Slack on Degraded or Unhealthy
```

---

## API Reference

### `run_synthetic_check(env, config) -> SyntheticCheckResult`

Execute one synthetic probe run. The `SyntheticProbeConfig` carries dedicated probe addresses that do not interfere with real user state.

```rust
use crate::synthetic_monitoring::{run_synthetic_check, SyntheticProbeConfig};

let config = SyntheticProbeConfig {
    probe_voucher:  voucher_address,
    probe_borrower: borrower_address,
    token:          token_address,
    stake_amount:   1_000_000,  // 0.1 XLM in stroops
    loan_amount:    500_000,    // 0.05 XLM in stroops
};
let result = run_synthetic_check(&env, config);
```

### `get_health_status(env) -> HealthStatus`

Retrieve the current health status without running a new check.

### `get_latest_synthetic_result(env) -> Option<SyntheticCheckResult>`

Read the full result of the most recent check.

### `get_synthetic_stats(env) -> SyntheticStats`

Cumulative pass/fail counts and rolling 24-hour success rate (bps).

---

## Scheduler Setup

### Using `stellar-cli` + cron

Add to your crontab (`crontab -e`):

```cron
# QuorumCredit synthetic monitoring — every 5 minutes
*/5 * * * * /usr/local/bin/stellar contract invoke \
  --id $CONTRACT_ID \
  --fn run_synthetic_check \
  --network testnet \
  --source $PROBE_SECRET_KEY \
  -- \
  --probe_voucher $PROBE_VOUCHER_ADDRESS \
  --probe_borrower $PROBE_BORROWER_ADDRESS \
  --token $TOKEN_CONTRACT \
  --stake_amount 1000000 \
  --loan_amount 500000 \
  >> /var/log/quorum-synthetic.log 2>&1
```

### Using GitHub Actions

```yaml
# .github/workflows/synthetic-monitoring.yml
name: Synthetic Monitoring
on:
  schedule:
    - cron: "*/5 * * * *"   # every 5 minutes
  workflow_dispatch:

jobs:
  synthetic-check:
    runs-on: ubuntu-latest
    steps:
      - name: Run synthetic probe
        env:
          CONTRACT_ID:           ${{ secrets.CONTRACT_ID }}
          PROBE_SECRET_KEY:      ${{ secrets.PROBE_SECRET_KEY }}
          PROBE_VOUCHER_ADDRESS: ${{ secrets.PROBE_VOUCHER_ADDRESS }}
          PROBE_BORROWER_ADDRESS: ${{ secrets.PROBE_BORROWER_ADDRESS }}
          TOKEN_CONTRACT:        ${{ secrets.TOKEN_CONTRACT }}
        run: |
          stellar contract invoke \
            --id "$CONTRACT_ID" \
            --fn run_synthetic_check \
            --network mainnet \
            --source "$PROBE_SECRET_KEY" \
            -- \
            --probe_voucher "$PROBE_VOUCHER_ADDRESS" \
            --probe_borrower "$PROBE_BORROWER_ADDRESS" \
            --token "$TOKEN_CONTRACT" \
            --stake_amount 1000000 \
            --loan_amount 500000
```

---

## Alert Configuration

### Indexer Integration

The indexer reads `synthetic/check` Soroban events and forwards them to Prometheus:

```yaml
# docs/prometheus-config.yml snippet
- job_name: quorum_synthetic
  metrics_path: /metrics
  static_configs:
    - targets: ['localhost:9090']
```

Prometheus metrics exposed:

| Metric | Type | Description |
|---|---|---|
| `qc_synthetic_check_total` | Counter | Total probe runs |
| `qc_synthetic_check_passed_total` | Counter | Passed runs |
| `qc_synthetic_check_failed_total` | Counter | Failed runs |
| `qc_synthetic_health_status` | Gauge | 2=Healthy, 1=Degraded, 0=Unhealthy |
| `qc_synthetic_success_rate_bps` | Gauge | Rolling 24h success rate (bps) |

### Grafana Alert Rules

```yaml
# In your Grafana alert group:
- alert: QuorumCreditSyntheticUnhealthy
  expr: qc_synthetic_health_status == 0
  for: 5m
  labels:
    severity: critical
  annotations:
    summary: "QuorumCredit contract is Unhealthy"
    description: "Synthetic loan lifecycle check failing. Check Soroban events."

- alert: QuorumCreditSyntheticDegraded
  expr: qc_synthetic_health_status == 1
  for: 15m
  labels:
    severity: warning
  annotations:
    summary: "QuorumCredit contract is Degraded"
    description: "Some synthetic steps failing. Investigate within 30 minutes."

- alert: QuorumCreditSyntheticSuccessRateLow
  expr: qc_synthetic_success_rate_bps < 9500
  for: 30m
  labels:
    severity: warning
  annotations:
    summary: "QuorumCredit synthetic success rate below 95%"
```

### PagerDuty / Slack Webhook

Configure the indexer to POST to your alerting endpoint when `status != "Healthy"`:

```typescript
// In tools/indexer/src/indexer.rs (off-chain handler)
if (event.action === "check" && event.category === "synthetic") {
  const { status } = event.value as { status: string };
  if (status === "Unhealthy" || status === "Degraded") {
    await fetch(process.env.ALERT_WEBHOOK_URL!, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        text: `🚨 QuorumCredit synthetic check: *${status}*`,
        details: event.value,
      }),
    });
  }
}
```

---

## Synthetic Probe Addresses

Use **dedicated testnet/mainnet accounts** for probe addresses — never your own keys.

| Account | Purpose |
|---|---|
| `PROBE_VOUCHER` | Stakes for the synthetic borrower |
| `PROBE_BORROWER` | Receives the synthetic loan and repays |

Pre-fund these accounts with enough XLM to cover stake + loan + transaction fees:

```bash
# Fund probe accounts on testnet
stellar keys generate probe-voucher --network testnet
stellar keys generate probe-borrower --network testnet
stellar account fund probe-voucher --network testnet
stellar account fund probe-borrower --network testnet
```

---

## Runbook

### `Healthy` → `Degraded`

1. Check the `step_results` in the latest `SyntheticCheckResult`.
2. Identify which step failed (`vouch`, `request_loan`, or `repay`).
3. Review the most recent Soroban events for the corresponding operation.
4. If the vouch step failed, check the probe voucher's balance.
5. If intermittent, wait for the next cycle. If persistent (>3 consecutive), escalate.

### `Degraded` → `Unhealthy`

1. Page the on-call engineer immediately.
2. Check if the contract is paused: `stellar contract invoke --fn get_config`.
3. Check the Stellar testnet/mainnet status page.
4. If the contract was recently upgraded, consider rolling back.
5. Run `stellar contract invoke --fn get_loan --borrower $PROBE_BORROWER` to inspect state.

### False Positives

Probes may fail due to:

- **Network congestion** — retry in the next cycle.
- **Ledger resets** (testnet only) — re-initialize probe accounts.
- **Probe account balance** — refund the probe voucher/borrower accounts.

To suppress alerts during planned maintenance:

```bash
# Disable the synthetic_monitoring flag to skip probe calls.
stellar contract invoke --id $CONTRACT_ID --fn kill_flag -- \
  --admin $ADMIN_ADDRESS --name synth_mon
```

Re-enable after maintenance:

```bash
stellar contract invoke --id $CONTRACT_ID --fn set_feature_flag -- \
  --admin $ADMIN_ADDRESS --name synth_mon --enabled true --rollout_pct 100
```

---

## Success-Rate Tracking

`get_synthetic_stats` returns:

```rust
SyntheticStats {
    total_runs: 288,
    total_passed: 285,
    total_degraded: 2,
    total_unhealthy: 1,
    rolling_success_rate_bps: 9896,  // 98.96 %
    last_run_ledger: 1234567,
}
```

The rolling rate is calculated over all historical runs (not just the last 288) in the current implementation. A future upgrade may limit it to a sliding window using a ring buffer.

Thresholds:

| Rate | Status |
|---|---|
| ≥ 9 500 bps (95 %) | Healthy |
| 8 000 – 9 499 bps | Degraded |
| < 8 000 bps | Unhealthy |

---

## Data Retention

Synthetic check results are stored in **persistent Soroban storage**:

| Key | Value | Description |
|---|---|---|
| `SyntheticKey::LatestResult` | `SyntheticCheckResult` | Most recent run |
| `SyntheticKey::Stats` | `SyntheticStats` | Cumulative counters |

Historical results beyond the latest are not retained on-chain (storage cost). The full history is available via the indexer's events database (`tools/indexer`).

---

## Related Documentation

- [Feature Flags Guide](feature-flags-guide.md) — use `FLAG_SYNTHETIC_MONITORING` to enable/disable the probe.
- [Monitoring Setup Guide](monitoring-setup-guide.md) — Prometheus, Grafana, and alerting configuration.
- [Monitoring Guide](monitoring-guide.md) — general protocol observability.
- [Deployment Guide](deployment-guide.md) — how to set up probe accounts on testnet/mainnet.
