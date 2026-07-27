# Consensus State Monitoring Guide

Stellar's Federated Byzantine Agreement means individual validators can, in
principle, diverge — a stale RPC node, a misconfigured quorum set, or a
bug in a third-party horizon/RPC implementation can all cause a node to
serve contract state that disagrees with the rest of the network. Nothing
in QuorumCredit previously watched for this. This guide covers the new
`scripts/consensus_monitor.py` tool that does.

See also: [Monitoring Setup Guide](monitoring-setup-guide.md) for the
Prometheus/Grafana stack this complements, and
[Monitoring Guide](monitoring-guide.md) for indexer-level alerting.

## What It Does

`consensus_monitor.py` polls a configurable list of Soroban RPC endpoints
(ideally operated by different parties — SDF, community validators, your
own infrastructure) and:

1. Compares the latest ledger sequence each endpoint reports, flagging any
   endpoint that has fallen more than `ledger_tolerance` ledgers behind.
2. Calls the same set of read-only contract views (`get_config`,
   `get_fee_treasury`, `get_slash_treasury` by default — configurable) on
   every endpoint and compares the results byte-for-byte.
3. On any mismatch, retries a bounded number of times with a delay
   (`--reconcile-retries` / `--reconcile-delay`) to distinguish transient
   replication lag from a genuine divergence, before alerting.
4. Sends an alert (webhook, e.g. Slack/PagerDuty-compatible) and exits
   non-zero so it can be wired into a cron job or CI-style scheduler that
   treats a non-zero exit as a page.

## Configuration

Copy `scripts/consensus_monitor.config.example.json` and fill in your
actual validator endpoints, contract ID, and alert webhook:

```bash
cp scripts/consensus_monitor.config.example.json scripts/consensus_monitor.config.json
```

| Field | Description |
|-------|-------------|
| `contract_id` | The deployed QuorumCredit contract address to query. |
| `checks` | List of read-only view functions to compare across validators. |
| `validators` | List of `{name, endpoint}` RPC nodes to poll. Include at least 3 from independent operators for a meaningful comparison. |
| `alert_webhook` | Slack-compatible incoming webhook URL. Omit to disable alert delivery (report is still printed to stdout). |
| `poll_interval_seconds` | Suggested interval for the scheduler running this script; not enforced by the script itself. |

## Running

```bash
./scripts/consensus_monitor.py --config scripts/consensus_monitor.config.json
```

Run with `--dry-run` to see what would be alerted without actually posting
to the webhook — useful when first configuring a new validator set.

Exit codes: `0` = all validators agree, `1` = divergence detected (alert
sent), `2` = configuration or connectivity error.

## Recommended Deployment

Run on a schedule independent of the indexer/RPC nodes being monitored —
e.g. a small cron job or scheduled CI workflow — so that a validator
outage doesn't also take down the thing watching for it:

```cron
* * * * * cd /opt/quorumcredit && ./scripts/consensus_monitor.py --config scripts/consensus_monitor.config.json >> /var/log/quorumcredit/consensus_monitor.log 2>&1
```

## Consensus Health Metrics

Track these over time (e.g. by piping the JSON report into your metrics
pipeline) to build a consensus health dashboard:

- **Divergence rate**: fraction of polls in the last 24h with at least one
  mismatch.
- **Max ledger lag**: worst observed lag between any validator and the
  network tip.
- **Mean time to reconciliation**: how many retry attempts were typically
  needed before a transient mismatch resolved on its own.
- **Per-validator error rate**: connectivity failures broken out by
  validator, to identify a consistently unreliable node.

## Recovery Procedure for Confirmed Divergence

1. **Do not treat a single poll's mismatch as confirmed** — the script's
   built-in reconciliation retries already filter out most replication
   lag. Only act on a report where `healthy: false` persists across
   multiple independent runs of the monitor.
2. **Identify the minority node(s)** — compare against the majority value
   across all healthy validators, not just two.
3. **Take the divergent validator's endpoint out of any load-balanced RPC
   pool** your services (indexer, borrower app, admin tooling) use, so
   reads don't silently pick up stale data.
4. **Investigate the divergent node**: check its ledger close history for
   a missed/late ledger, its quorum set configuration, and its software
   version against the rest of the fleet.
5. **Resync**: restart the node from a recent history archive checkpoint
   rather than trusting incremental catch-up, since the divergence
   indicates something already went wrong with incremental sync.
6. **Re-run the monitor** with `--dry-run` against just the recovered node
   and one known-good node to confirm agreement before returning it to
   the pool.
7. **Post-incident**: record the divergence window, root cause, and time
   to detection/recovery — feed this into the consensus health metrics
   above to track whether detection time is improving.
