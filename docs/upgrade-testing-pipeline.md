# Automated Upgrade Testing Pipeline

Manual contract upgrades are error-prone: it's easy to forget to check
storage layout compatibility, or to skip verifying that a "small" change
didn't silently break a function signature. This pipeline automates those
checks and runs them in CI on every PR that touches `src/**` or
`Cargo.toml`, and again on every push to `main`.

See also: [Upgrade Guide](upgrade-guide.md) for the manual runbook this
pipeline is meant to reduce the risk of, and
[Monitoring Setup Guide](monitoring-setup-guide.md) for watching a live
upgrade in progress.

## Components

| Component | Purpose |
|-----------|---------|
| `.github/workflows/upgrade-test.yml` | CI entrypoint. Builds pre-upgrade WASM from `main` and post-upgrade WASM from the PR branch, then runs the pipeline script. |
| `scripts/upgrade_pipeline_test.sh` | Orchestrates the five pipeline steps and produces a rollback plan on failure. |
| `tests/fixtures/upgrade/pre_upgrade_state.json` | Representative pre-upgrade contract state (loans, vouches, credit scores, treasury balances). |
| `tests/fixtures/upgrade/expected_post_migration.json` | Invariants and spot-checks that must hold after migration. |
| `tests/fixtures/upgrade/backward_compat_functions.json` | List of exported functions that must remain present in the post-upgrade WASM. |
| `tests/fixtures/upgrade/storage_layout.json` | Baseline storage key/type layout that must not change in a breaking way. |

## Pipeline Steps

### 1. Load pre-upgrade state fixtures

The pipeline loads `pre_upgrade_state.json`, a representative snapshot of
contract state (active/repaid/defaulted loans, vouches, credit scores,
treasury balances) that stands in for real testnet/mainnet state without
requiring a live network connection in CI.

### 2. State migration validation

`expected_post_migration.json` encodes invariants that must hold after the
upgrade is applied to the pre-upgrade fixture: totals must balance (e.g.
sum of loan principals, sum of vouch stakes), status enums must map
consistently, and any recalculation of credit scores or treasury balances
must be explicitly documented in the upgrade PR rather than happening
silently.

### 3. Backward compatibility of functions

`backward_compat_functions.json` lists every function external callers
(SDKs, indexers, the borrower app) depend on. The pipeline checks the
post-upgrade WASM's export table still contains each one — a function
being renamed or removed without a deprecation path is treated as a
pipeline failure, not a warning.

### 4. Storage layout verification

`storage_layout.json` is the baseline of instance and persistent storage
keys and their types. Changing an existing key's type, or removing a key
outright, is a breaking migration and must ship with an explicit migration
function rather than a bare `upgrade()` call. The pipeline flags this so
it gets caught in review instead of on mainnet.

### 5. Automatic rollback on failure

If any check fails, the CI job's rollback step runs
`upgrade_pipeline_test.sh --rollback`, which writes
`artifacts/rollback_plan.json` — the hash of the last-known-good WASM and
the exact sequence of admin calls needed to restore it. The workflow then
fails the PR/build, so a broken upgrade never reaches the point of being
merged, let alone deployed.

## Running Locally

```bash
# Build both WASM artifacts first (see docs/upgrade-guide.md for build flags)
cargo build --release --target wasm32-unknown-unknown

./scripts/upgrade_pipeline_test.sh \
  --pre-wasm target/wasm32-unknown-unknown/release/quorum_credit_prev.wasm \
  --post-wasm target/wasm32-unknown-unknown/release/quorum_credit.wasm \
  --fixtures tests/fixtures/upgrade
```

## Updating Fixtures

When an upgrade intentionally changes storage layout, function signatures,
or scoring behavior, update the relevant fixture file **in the same PR**
as the code change, with a note in the PR description explaining what
changed and why it's safe. This keeps the fixtures honest — they should
always reflect the current intended contract, not just the original one.
