# Incident Response Playbook

This playbook documents how QuorumCredit operators respond to production
incidents. It exists so that on-call responders can act quickly and
consistently instead of improvising during an outage, and so that
post-incident reviews have a checklist to verify against. Pair this with
[troubleshooting-guide.md](./troubleshooting-guide.md) (symptom-level fixes)
and [monitoring-guide.md](./monitoring-guide.md) (what the dashboards mean).

## Table of Contents

- [Incident Severity Levels](#incident-severity-levels)
- [Incident Types](#incident-types)
  - [1. Smart Contract Issues](#1-smart-contract-issues)
  - [2. API / Server Outages](#2-api--server-outages)
  - [3. Data Loss](#3-data-loss)
- [Communication Templates](#communication-templates)
- [Recovery Verification Steps](#recovery-verification-steps)
- [Post-Incident Review Checklist](#post-incident-review-checklist)

---

## Incident Severity Levels

| Level | Definition | Example | Response time |
|-------|------------|---------|----------------|
| SEV-1 | Funds at risk or contract halted | Exploit draining vouch pool; contract panics on every call | Immediate, all-hands |
| SEV-2 | Core functionality degraded, no fund loss | API server down, indexer stalled | < 30 min |
| SEV-3 | Partial degradation, workaround exists | Webhook deliveries failing, one RPC endpoint down | < 4 hours |
| SEV-4 | Cosmetic / low impact | Stale dashboard metric, doc bug | Next business day |

Declare the incident in the `#incidents` channel with `/incident declare <SEV-N> <one-line summary>` as soon as SEV-1/2 is suspected — don't wait for confirmation.

---

## 1. Smart Contract Issues

Covers: exploit in progress, panicking contract calls, invariant violations, stuck funds, unauthorized admin/governance action.

### Detection
- Prometheus alert on invariant checks failing (see `prometheus-alerts.yml`)
- Anomalous transaction volume or repayment/slash ratio in Grafana
- Community report of unexpected balance changes

### Immediate actions
1. **Triage authority**: Page the on-call contract engineer and one of the multisig admins (see `docs/adr/0005-multisig-admin-and-governance.md`).
2. **Assess exploitability**: Reproduce against testnet with the suspected transaction sequence before touching mainnet state.
3. **Pause if available**: If the deployed contract version has an emergency-pause/governance-timelock path, initiate it per `docs/governance-queue-guide.md`. Do not attempt an ad hoc admin call outside the documented governance flow — unreviewed emergency transactions are themselves a common cause of secondary incidents.
4. **Freeze off-chain surfaces**: Disable webhook dispatch and the public API's mutating endpoints (`/vouch`, loan issuance) if the exploit is being actively exercised, to limit blast radius while a fix is prepared.
5. **Preserve evidence**: Snapshot the indexer database and export the relevant event range before any remediation transaction is submitted — this is required for the post-incident review and any disclosure.

### Remediation
- Prepare a fix or governance action, get a second engineer to review the diff/transaction against `docs/security-audit-checklist.md`.
- Route through governance/timelock unless the severity justifies an emergency multisig action (requires 2 of the documented signers, logged in `#incidents`).
- Re-run the full invariant test suite (`src/invariants.rs`, `src/property_based_invariants_test.rs`) against the patched contract before redeploying.

---

## 2. API / Server Outages

Covers: broadcast server (`server/`) unresponsive, elevated error rates, indexer (`services/indexer`) falling behind chain head, webhook delivery collapse.

### Detection
- `/health` endpoint failing or timing out
- `qc_*` Prometheus counters flatlining
- Indexer lag alert (event timestamp vs. wall clock)

### Immediate actions
1. Check process status and recent logs on the affected host(s):
   ```bash
   systemctl status quorum-credit-server
   journalctl -u quorum-credit-server -n 200 --no-pager
   ```
2. Check the indexer's read position against chain head — a stalled indexer looks like an API outage to clients even when the server process is healthy:
   ```bash
   curl -s localhost:PORT/health | jq
   ```
3. If the server is up but degraded, check `/metrics` for saturation (event loop lag, open handle count) before restarting — a restart without root cause loses debugging state.
4. If webhook delivery is the failure mode specifically, check `GET /api/webhooks/{id}/stats` for the affected subscription's `successRateBps`; a collapse there points at the subscriber's endpoint, not this service, and should not trigger a server-side rollback.
5. Roll back to the last known-good deploy if the outage started immediately after a release.

### Escalation
- If root cause is unclear after 15 minutes at SEV-2, escalate to SEV-1 and pull in a second engineer.
- If the RPC provider (Soroban RPC endpoint) is the underlying cause, fail over to the documented backup endpoint (see `production-deployment-guide.md`).

---

## 3. Data Loss

Covers: indexer database corruption/loss, lost webhook registrations, lost off-chain metadata (expense records, recurring-payment schedules) that has no on-chain equivalent.

### Detection
- Indexer queries returning empty/inconsistent results for known-good ranges
- Borrower/operator reports of missing expense records or recurring-payment schedules
- Backup job failure alert

### Immediate actions
1. **Stop writes** to the affected store to prevent further divergence, if the loss is ongoing (e.g. disk failure in progress).
2. **Classify recoverability**:
   - On-chain state (loans, vouches, balances): always recoverable by re-indexing from genesis or the last checkpoint — not permanent data loss. See `backup-recovery-guide.md`.
   - Off-chain-only state (expense ledger, recurring-payment schedules, webhook registrations — all in-memory stores in `server/src/*Store.ts`): **not recoverable without a backup**, since it has no on-chain source of truth. This is the case that needs a real backup/restore path in production, not just re-indexing.
3. **Restore from backup** per `backup-recovery-guide.md`, verifying backup integrity (checksum, row counts) before cutting traffic back over.
4. **Re-index** if the loss is confined to the indexer's sqlite database:
   ```bash
   cd services/indexer && npm run reindex -- --from-genesis
   ```

### If no backup exists
- Document the gap explicitly in the incident record — do not silently patch over it.
- Off-chain records that borrowers submitted (e.g. declared expense purpose) may be re-collectable by asking affected borrowers to re-submit; on-chain-derived data is not.

---

## Communication Templates

### Initial notice (post within 15 min of SEV-1/2 declaration)

> **[INCIDENT] SEV-{N}: {one-line summary}**
> Status: Investigating
> Impact: {who/what is affected, e.g. "webhook deliveries delayed, no fund impact"}
> Started: {timestamp, UTC}
> Next update: within {30/60} minutes

### Update (every 30–60 min until resolved)

> **[UPDATE] SEV-{N}: {one-line summary}**
> Status: {Investigating | Identified | Monitoring | Resolved}
> What we know: {root cause if identified, else current hypothesis}
> What we're doing: {current action}
> Next update: {time}

### Resolution notice

> **[RESOLVED] SEV-{N}: {one-line summary}**
> Root cause: {brief}
> Resolution: {what fixed it}
> Follow-up: post-incident review scheduled for {date}; tracking issue {link}

### External/user-facing notice (SEV-1/2 with user impact)

> We're aware of an issue affecting {feature}. {No funds are at risk. / We have paused X as a precaution.} We'll post an update by {time}. Track status at {status page/channel}.

---

## Recovery Verification Steps

Before declaring an incident resolved, verify **all** of the following that apply:

- [ ] `/health` returns `200 ok` from all instances behind the load balancer
- [ ] Indexer lag is back under the normal threshold (see `monitoring-guide.md` for the alert threshold)
- [ ] Error rate (`qc_*_failed_total` counters) has returned to baseline, not just stopped increasing
- [ ] For contract incidents: the full invariant suite passes against the live contract state (read-only checks, no test transactions on mainnet)
- [ ] For data-loss incidents: restored record counts match the last verified backup, and a spot-check of 5+ records matches known-good values
- [ ] For webhook incidents: `GET /api/webhooks/{id}/stats` shows `successRateBps` recovering for previously-affected subscriptions
- [ ] No new alerts have fired in the 15 minutes following the fix
- [ ] A second engineer has independently confirmed the above (no self-certification for SEV-1/2)

---

## Post-Incident Review Checklist

Hold within 2 business days of resolution for SEV-1/2 incidents.

- [ ] Timeline reconstructed (detection → declaration → mitigation → resolution), with timestamps
- [ ] Root cause identified and documented (not just the immediate trigger — the underlying gap)
- [ ] Blast radius confirmed: what was actually affected vs. what was initially suspected
- [ ] Contributing factors identified: monitoring gaps, missing runbook steps, delayed escalation, etc.
- [ ] Action items filed as tracked issues with owners and due dates (not just discussed)
- [ ] This playbook updated if the incident revealed a missing or incorrect step
- [ ] Communication reviewed: were updates timely and accurate, both internal and external?
- [ ] For contract incidents: audit/review process re-checked — did existing checks in `docs/security-audit-checklist.md` cover this class of issue, and if not, is it added?
- [ ] Summary shared with the wider team, blameless in tone, focused on systemic fixes

---

## See Also

- [troubleshooting-guide.md](./troubleshooting-guide.md) — symptom-level fixes for common operational issues
- [monitoring-guide.md](./monitoring-guide.md) / [monitoring-setup-guide.md](./monitoring-setup-guide.md) — dashboards and alert definitions
- [backup-recovery-guide.md](./backup-recovery-guide.md) — backup cadence and restore procedure
- [security-audit-checklist.md](./security-audit-checklist.md) — pre-deploy review checklist referenced during contract-incident remediation
- [governance-queue-guide.md](./governance-queue-guide.md) — the governance/timelock path for emergency contract actions
