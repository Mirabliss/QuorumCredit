# Pre-Deployment Checklist

A standardized checklist to run through before every mainnet (or
significant testnet) deployment of QuorumCredit. This is meant to be used
as an actual checklist during a deployment window, not just read once.
It complements — and does not replace — the narrative guides in
[`deployment-guide.md`](./deployment-guide.md) and
[`production-deployment-guide.md`](./production-deployment-guide.md), and
the [`security-audit-checklist.md`](./security-audit-checklist.md) for
pre-audit hardening. Use `scripts/deploy.sh` for the actual deployment
mechanics referenced below.

## 1. Pre-Deployment Verification Steps

- [ ] Target network confirmed (testnet vs. mainnet) and matches the
      intended release — double-check `NETWORK`/RPC endpoint env vars
      before running `scripts/deploy.sh`.
- [ ] All changes since the last deployment are merged to the release
      branch and the commit hash to be deployed is recorded in the
      deployment ticket/log.
- [ ] Full test suite passes on the exact commit being deployed (unit
      tests in `src/*_test.rs`, integration tests in `tests/`).
- [ ] Mutation testing results reviewed for any recently-changed modules
      (see `docs/mutation-testing.md`) — no unexplained mutant survival in
      security-critical paths (`invariants.rs`, `rbac.rs`, `credit_score.rs`,
      `collateral_pool.rs`).
- [ ] Gas/resource budget check: recent changes reviewed against
      `docs/gas-budgets.md` to confirm no transaction path has grown past
      its Soroban resource budget.
- [ ] Security audit checklist (`docs/security-audit-checklist.md`) has
      been completed for any new or modified contract logic since the
      last deployment.
- [ ] Multisig/admin key holders for this deployment are confirmed
      available and have reviewed the change set (see
      `docs/adr/0005-multisig-admin-and-governance.md`).
- [ ] Environment configuration (contract addresses, oracle addresses,
      admin/governance addresses) reviewed line-by-line against the
      target network's known-good values — a wrong address here is one of
      the most common and costly deployment mistakes.
- [ ] Deployment scripts (`scripts/deploy.sh`, `scripts/initialize.sh`)
      have been dry-run against a fork or testnet copy of current mainnet
      state if this is a mainnet upgrade rather than a fresh deploy.
- [ ] Rollback plan (Section 4 below) reviewed and understood by whoever
      is executing the deployment, before starting.

## 2. Contract State Validation Checks

- [ ] All protocol invariants (`src/invariants.rs`,
      `docs/contract-invariants.md`) pass against current on-chain state
      immediately before deployment, to confirm the starting state is
      sound.
- [ ] If this deployment includes a storage migration or redesign (see
      `src/storage_redesign_test.rs`), the migration has been dry-run
      against a snapshot of production state and the resulting storage
      layout has been diffed against expectations.
- [ ] Credit score configuration weights sum to `10000` bps
      (`set_credit_score_config`'s own invariant) if credit-score config
      is part of this deployment.
- [ ] Paused-state handling verified: confirm the contract's pause switch
      works as expected in a pre-deployment dry run (see
      `src/paused_state_test.rs`), in case an emergency pause is needed
      immediately after deployment.
- [ ] RBAC roles (`src/rbac.rs`) for this deployment reviewed — no role
      is over-privileged relative to what this release actually requires,
      and role assignments match `docs/RBAC_QUICK_REFERENCE.md` (root
      `RBAC_*` docs) expectations.
- [ ] Collateral pool accounting (`src/collateral_pool.rs`) reconciled:
      total staked/locked amounts tracked by the contract match the
      indexer's independently-computed totals before deployment proceeds.
- [ ] No open/pending withdrawal-queue entries are left in an ambiguous
      state that the upcoming deployment's logic changes could
      mishandle (see `WITHDRAWAL_QUEUE_OPTIMIZATION.md`).

## 3. Oracle Health Verification

- [ ] Oracle price/rate feed is live and returning fresh data (checked
      within the last expected update interval, not stale) immediately
      before deployment.
- [ ] Oracle address configured in the deployment matches the audited,
      approved oracle contract address for the target network — verify
      against the deployment record from the last successful deployment,
      not from memory.
- [ ] Dynamic rate oracle behavior sanity-checked against a known-good
      reference rate (see `src/dynamic_rate_oracle_test.rs`) to catch a
      misconfigured or stale oracle before it affects live interest
      calculations.
- [ ] Oracle failure/fallback behavior confirmed: verify the contract's
      behavior if the oracle becomes unavailable post-deployment (does it
      pause rate-sensitive operations, or fall back to a last-known-good
      value?) and that this behavior is what's intended for this release.
- [ ] Oracle monitoring alerts (see Section 5 and
      `docs/monitoring-guide.md`) are active and correctly pointed at the
      oracle instance this deployment will use.

## 4. Rollback Procedures

- [ ] Confirm whether this deployment is upgradeable in-place (via the
      contract's upgrade mechanism, see `docs/upgrade-guide.md`) or
      requires a fresh contract address. Rollback mechanics differ
      significantly between the two.
- [ ] For in-place upgrades: confirm the previous WASM hash/version is
      recorded and available to redeploy immediately if a critical issue
      is found post-deployment.
- [ ] For state-affecting migrations: confirm a state backup was taken
      immediately before deployment (see `docs/backup-recovery-guide.md`
      and `scripts/backup.sh`) so `scripts/restore.sh` can recover
      pre-deployment state if the migration must be reverted.
- [ ] Rollback authorization confirmed: identify in advance which
      multisig signers/roles are required to execute a rollback, and
      confirm they are reachable during the deployment window.
- [ ] Rollback decision criteria agreed in advance (e.g. "any invariant
      violation observed within the first N blocks triggers automatic
      rollback consideration") so the decision isn't made ad hoc under
      pressure during an incident.
- [ ] Communication plan for a rollback event is ready (who gets
      notified, what's posted publicly if the deployment is user-facing).

## 5. Post-Deployment Monitoring Setup

- [ ] Monitoring dashboards (see `docs/monitoring-guide.md`) confirmed to
      be tracking the newly-deployed contract address, not a stale
      address from a previous deployment.
- [ ] Alert thresholds reviewed and, if this deployment changes expected
      transaction volume or gas usage patterns, adjusted so alerts don't
      immediately false-positive (or fail to fire) against the new
      baseline.
- [ ] Invariant-violation alerting confirmed active — any invariant
      breach post-deployment should page an operator, not go unnoticed
      until the next manual check.
- [ ] Oracle health alerting (staleness, unavailability) confirmed active
      per Section 3.
- [ ] A defined observation window is established post-deployment (e.g.
      first 24 hours) during which an operator actively watches
      dashboards rather than relying solely on passive alerting, given
      that a new deployment is the highest-risk period for surfacing an
      undetected issue.
- [ ] Post-deployment sign-off recorded (who deployed, when, commit hash,
      confirmation that all above sections were completed) for the
      deployment log/audit trail.

## Quick Reference: Order of Operations

1. Complete Section 1 (pre-deployment verification) entirely before
   touching any deployment script.
2. Take a state backup (Section 4) immediately before executing the
   deployment.
3. Execute the deployment via `scripts/deploy.sh` / `scripts/initialize.sh`.
4. Immediately run Section 2 (contract state validation) against the
   freshly-deployed contract.
5. Run Section 3 (oracle health) checks against the live deployment.
6. Confirm Section 5 (monitoring) is active before considering the
   deployment complete.
7. If any check in Sections 2–3 fails, execute Section 4's rollback
   procedure rather than attempting a live fix under time pressure.
