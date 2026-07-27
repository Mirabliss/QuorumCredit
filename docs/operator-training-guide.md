# Operator Training Guide

Training materials for anyone taking on an operational role for
QuorumCredit — running infrastructure, watching dashboards, responding to
incidents, or executing deployments. This guide is meant to be worked
through in order by a new operator, and used as an ongoing reference by
experienced ones. It builds on
[`docs/monitoring-guide.md`](./monitoring-guide.md),
[`docs/pre-deployment-checklist.md`](./pre-deployment-checklist.md),
[`docs/troubleshooting-guide.md`](./troubleshooting-guide.md), and
[`docs/backup-recovery-guide.md`](./backup-recovery-guide.md), pulling them
together into a single operational curriculum rather than duplicating them.

## 1. Operator Roles

QuorumCredit operations are split into distinct roles, mirroring the
on-chain RBAC roles in `src/rbac.rs` (see `docs/RBAC_QUICK_REFERENCE.md`)
so that on-chain permissions and operational responsibilities stay aligned:

### 1.1 On-Call Operator
- **Responsibility**: first responder for alerts (Section 3), triages
  incidents, executes documented runbooks (e.g.
  `docs/troubleshooting-guide.md`), and escalates when a runbook doesn't
  cover the situation.
- **On-chain permissions needed**: read-only in most cases; pause
  authority only if explicitly granted for emergency response.
- **Not responsible for**: deployments or governance actions — those are
  separate roles below, even if the same person sometimes holds multiple
  roles.

### 1.2 Deployment Operator
- **Responsibility**: executes deployments end-to-end using
  `docs/pre-deployment-checklist.md` and `scripts/deploy.sh`.
- **On-chain permissions needed**: whatever admin role is required to
  execute an upgrade or initialization, per `docs/upgrade-guide.md`.
- **Prerequisite**: must have completed the certification track in
  Section 5 up to at least "Deployment Certified" before running a
  mainnet deployment unsupervised.

### 1.3 Governance/Multisig Signer
- **Responsibility**: reviews and signs governance actions (config
  changes, credit-score weight updates, role grants) per
  `docs/adr/0005-multisig-admin-and-governance.md` and
  `docs/governance-queue-guide.md`.
- **Not an operational role in the day-to-day sense** — included here
  because on-call operators need to know who to contact when an incident
  requires a governance action (e.g. pausing via multisig, adjusting a
  misconfigured parameter).

### 1.4 Indexer/Infrastructure Operator
- **Responsibility**: keeps the off-chain indexer (`tools/indexer`),
  backup jobs (`scripts/backup.sh`), and dashboard/server infrastructure
  (`server/`, `dashboard/`) healthy and up to date.
- **On-chain permissions needed**: none — this role operates entirely on
  off-chain infrastructure, though its failures can look like on-chain
  issues to an on-call operator (e.g. stale dashboard data caused by
  indexer lag, not an actual protocol problem).

## 2. Daily Operational Procedures

A recommended daily routine for an on-call operator, to be done at shift
start regardless of whether any alerts have fired overnight:

1. **Dashboard sweep** — open the monitoring dashboards (Section 3) and
   confirm all panels are populating with recent data (a blank/stale panel
   is itself a signal, usually of indexer lag rather than protocol
   failure — see the decision tree in Section 4).
2. **Alert queue review** — check for any alerts that fired and were
   auto-resolved overnight; even auto-resolved alerts should be reviewed
   to catch recurring low-grade issues before they escalate.
3. **Oracle freshness check** — confirm the rate oracle's last update
   timestamp is within its expected interval (see
   `docs/pre-deployment-checklist.md` Section 3 for the same check applied
   at deployment time — the daily check is the ongoing version of it).
4. **Backup job confirmation** — confirm the most recent scheduled backup
   (`scripts/backup.sh`) completed successfully; a silently failing
   backup job is only discovered at the worst possible time (when a
   restore is actually needed) if it isn't checked daily.
5. **Invariant check** — spot-check that no invariant-violation alert is
   pending (`src/invariants.rs`); this should normally be covered by
   automated alerting, but a manual daily check is a cheap second layer.
6. **Withdrawal queue depth check** — confirm the withdrawal queue isn't
   growing unexpectedly (see `WITHDRAWAL_QUEUE_OPTIMIZATION.md`), which
   can indicate a stuck batch-processing job rather than organic demand.
7. **Handoff notes** — log anything notable for the next shift, even if
   nothing fired — "quiet shift, all checks green" is still a useful
   record for spotting trends over time.

## 3. Monitoring Dashboards and Alerts

This section is an operator-facing index into `docs/monitoring-guide.md` —
read that guide fully once, then use this as the quick-reference summary
during a shift.

| Dashboard/Panel | What it shows | Escalate if |
|---|---|---|
| Contract health | Invariant status, pause state, recent transaction volume | Any invariant shows violated, or pause state changed unexpectedly |
| Oracle status | Last update time, current rate value, feed source | Last update older than expected interval, or rate value outside sane bounds |
| Credit score activity | Volume of `update_credit_score` calls, distribution of resulting tiers | Sudden spike or collapse in a specific tier — may indicate a scoring bug or an attack |
| Vouching/Sybil signals | Vouch creation rate, distribution of stake-time weights | Unusual clustering of new low-age, minimum-stake vouches — possible Sybil attempt |
| Withdrawal queue | Queue depth, processing rate | Depth growing faster than processing rate over a sustained period |
| Indexer lag | Delta between latest on-chain event and latest indexed event | Lag exceeding the threshold documented in `docs/event-indexing-guide.md` |
| Backup status | Last successful backup timestamp, backup size trend | Missed scheduled backup, or unexplained size drop (possible partial backup) |

Alert severity conventions used across these dashboards:
- **Critical** — pages the on-call operator immediately, requires
  acknowledgment within the response-time SLA defined in
  `docs/monitoring-guide.md`.
- **Warning** — reviewed at the next daily sweep unless it recurs multiple
  times in a shift, in which case treat it as critical.
- **Informational** — logged only, reviewed during weekly operational
  review rather than per-shift.

## 4. Troubleshooting Decision Trees

### 4.1 "A dashboard panel looks wrong or stale"

```
Is the underlying on-chain data itself stale (check via direct RPC query,
bypassing the dashboard)?
├─ Yes → this is a protocol/oracle issue, not a dashboard issue.
│        Go to 4.2 or 4.3 depending on which data is stale.
└─ No  → the on-chain data is fine, so this is an indexer/dashboard issue.
         Check indexer lag panel.
         ├─ Indexer lag is high → known indexer backlog; check indexer
         │   process health (tools/indexer), restart if hung, escalate to
         │   Indexer/Infrastructure Operator if restart doesn't clear it.
         └─ Indexer lag is normal → dashboard/frontend bug; file an issue,
             not an operational incident.
```

### 4.2 "Oracle appears stale or returning suspicious values"

```
Is the oracle contract itself reachable (direct query succeeds)?
├─ No  → oracle infrastructure outage. Escalate to whoever owns the
│        oracle service. Consider whether rate-sensitive operations
│        should be paused until resolved (requires governance/multisig).
└─ Yes → oracle is reachable but data looks wrong.
         Is the value within a plausible range vs. recent history?
         ├─ No  → possible oracle manipulation or misconfiguration.
         │        Escalate immediately as a security-relevant incident,
         │        do not assume it will self-correct.
         └─ Yes → value is plausible but timestamp is stale.
                  Check oracle's own update-job health; likely a delayed
                  update rather than an attack. Escalate to oracle owner
                  with lower urgency than the manipulation case above.
```

### 4.3 "Invariant-violation alert fired"

```
Can the violation be reproduced against current on-chain state via a
read-only query (not just the alert's cached snapshot)?
├─ No  → likely a transient/monitoring false-positive. Log it, watch for
│        recurrence, do not take contract action based on a single
│        unreproducible alert.
└─ Yes → genuine invariant violation.
         Is it isolated to a single borrower/loan record, or systemic
         (affects protocol-wide accounting)?
         ├─ Isolated  → contain: flag the specific record, assess whether
         │              it's exploitable before deciding on a fix path.
         │              Usually does not require a full pause.
         └─ Systemic  → treat as a critical incident: consider emergency
                        pause (requires appropriate role/multisig),
                        escalate to governance signers and, if externally
                        exploitable, follow the security disclosure
                        process in SECURITY.md.
```

### 4.4 "Withdrawal queue depth growing abnormally"

```
Is the batch-processing job for the queue actually running (check recent
processing transactions)?
├─ No  → stuck/failed job. Check the process/cron responsible for
│        triggering batch processing, restart if needed.
└─ Yes → job is running but not keeping up.
         Is incoming withdrawal volume abnormally high (organic demand
         spike) or is per-batch processing throughput abnormally low
         (regression)?
         ├─ Demand spike   → monitor; consider increasing batch frequency
         │                   if within safe resource limits.
         └─ Throughput drop → likely a regression from a recent change;
                              escalate to engineering, reference
                              WITHDRAWAL_QUEUE_OPTIMIZATION.md for the
                              expected throughput baseline to compare against.
```

## 5. Certification Program Outline

A three-level certification path. Each level should be re-certified
annually, or immediately after any major protocol upgrade that changes the
relevant systems.

### Level 1 — Monitoring Certified
**Prerequisite for**: On-Call Operator role (unsupervised).
- Complete a guided walkthrough of every dashboard in Section 3.
- Demonstrate correct triage using the decision trees in Section 4 against
  at least three simulated incident scenarios.
- Pass a written/verbal review of alert severity conventions and
  escalation paths.

### Level 2 — Deployment Certified
**Prerequisite for**: Deployment Operator role (unsupervised on mainnet).
- Requires Level 1 certification first.
- Execute a full deployment against a testnet environment end-to-end
  using `docs/pre-deployment-checklist.md`, under supervision.
- Execute a simulated rollback (Section 4 of the pre-deployment checklist)
  against a testnet environment, under supervision.
- Demonstrate familiarity with `docs/upgrade-guide.md` and
  `docs/backup-recovery-guide.md`.

### Level 3 — Incident Commander Certified
**Prerequisite for**: leading response to a systemic/critical incident
(e.g. the systemic branch of the invariant-violation decision tree, or any
scenario requiring an emergency pause).
- Requires Level 1 and Level 2 certification first.
- Demonstrate understanding of the multisig/governance process well enough
  to coordinate an emergency pause or rollback with signers under time
  pressure.
- Participate in at least one full incident-response tabletop exercise
  covering a systemic invariant violation or an oracle manipulation
  scenario.
- Familiarity with `SECURITY.md`'s disclosure process for
  externally-exploitable issues discovered during an incident.

### Renewal
- Level 1: annual refresher covering any dashboard/alert changes since
  last certification.
- Level 2: annual refresher plus a fresh supervised testnet deployment if
  the deployment tooling (`scripts/deploy.sh`, `scripts/initialize.sh`)
  has changed materially.
- Level 3: annual tabletop exercise repeated with an updated scenario.
