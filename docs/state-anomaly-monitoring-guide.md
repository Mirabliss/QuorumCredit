# Contract State Anomaly Monitoring Guide

`scripts/consensus_monitor.py` (see
[Consensus Monitoring Guide](consensus-monitoring-guide.md)) checks that
validators agree with each other. This guide covers
`scripts/state_anomaly_monitor.py`, which instead tracks contract state
*over time* against a single trusted source, to catch things like a
sudden spike in defaults, an exploit draining vouches, or storage quietly
approaching its limit — the kind of drift that consensus agreement alone
won't reveal, because all validators can agree on a bad state.

## What It Tracks

| Metric | Anomaly Condition (default threshold) |
|--------|-----------------------------------------|
| Defaulted loans | More than 5 new defaults between polls |
| Total loan count | More than 1000 new loans between polls (spam/exploit signal) |
| Average interest rate | Outside 0–5000 bps |
| Storage entries vs. limit | At or above 80% of `storage_limit` |
| Average gas per transaction | More than 25% increase since last poll |
| Active vouches | More than 30% drop since last poll (mass-withdrawal signal) |

All thresholds are configurable per-deployment in
`scripts/state_anomaly_monitor.config.json` under `thresholds`.

## Configuration

```bash
cp scripts/state_anomaly_monitor.config.example.json scripts/state_anomaly_monitor.config.json
```

| Field | Description |
|-------|-------------|
| `indexer_summary_url` | JSON summary endpoint on your QuorumCredit indexer exposing the metrics above (extend the indexer from [Monitoring Setup Guide](monitoring-setup-guide.md) if it doesn't already). |
| `history_path` | Where snapshot history is persisted between runs (JSON array, trimmed to `max_history_entries`). |
| `default_storage_limit` | Fallback storage limit if the indexer doesn't report one. |
| `alert_webhook` | Slack-compatible webhook for anomaly alerts. |
| `thresholds` | Per-metric anomaly thresholds — see table above. |

## Running

```bash
# Normal poll: fetch state, compare to history, alert on anomalies, append to history
./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json

# Dry run: see what would alert without posting to the webhook
./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json --dry-run

# Daily report only: print a human-readable summary without touching history/alerts
./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json --report-only
```

Exit codes: `0` = no anomalies, `1` = anomaly detected (alert sent),
`2` = configuration or connectivity error.

## Recommended Deployment

Poll hourly for anomaly detection, and generate the human-readable daily
report once every 24h:

```cron
0 * * * * cd /opt/quorumcredit && ./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json >> /var/log/quorumcredit/state_anomaly.log 2>&1
0 6 * * * cd /opt/quorumcredit && ./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json --report-only > /var/log/quorumcredit/daily_report_$(date +\%F).md
```

At an hourly poll interval, `max_history_entries: 720` retains roughly 30
days of history, which is enough context for the daily report's 24-hour
window and for eyeballing week-over-week trends.

## Responding to an Anomaly Alert

1. **Defaults spike**: cross-check against
   [Common Support Issues](faq.md#common-support-issues) — a spike right
   after a deadline-heavy cohort of loans matures is expected seasonality,
   not necessarily an incident. Compare against the same period in prior
   weeks before escalating.
2. **Loan count spike**: check whether `min_loan_amount` is still enforced
   correctly (see [Contract Invariants](contract-invariants.md)) — a spike
   combined with unusually small loan sizes suggests spam or an attempted
   griefing attack.
3. **Storage approaching limit**: plan an archival pass (see
   `src/archive.rs` for the contract's existing archival mechanism) before
   the limit is reached, not after — running out of storage mid-operation
   can strand in-flight loans.
4. **Gas cost trend up**: correlate with recent contract upgrades — a gas
   regression introduced by an upgrade should show up here quickly. Check
   [Gas Benchmarking](../GAS_BENCHMARKING_IMPLEMENTATION_SUMMARY.md) results for
   the same time window.
5. **Vouch drop**: check whether this coincides with a slashing event or
   a broader market/community event outside the protocol's control before
   treating it as an on-chain issue.

## Extending the Indexer Summary Endpoint

If your indexer doesn't yet expose `total_loans`, `average_interest_rate_bps`,
etc. as a single JSON summary, add a lightweight handler that aggregates
the existing Prometheus metrics documented in
[Monitoring Setup Guide — Available Metrics](monitoring-setup-guide.md#available-metrics)
into the shape this script expects; the script never talks to Prometheus
directly so any endpoint returning the right JSON keys works.
