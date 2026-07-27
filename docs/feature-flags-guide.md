# Feature Flags Guide

> Issue #1233 — Runtime feature-flag and gradual-rollout system for QuorumCredit.

---

## Overview

QuorumCredit features are compiled into the WASM binary, but the **Feature Flags** system lets an admin enable, disable, or gradually roll out any feature at runtime — without redeploying a new contract.

Each flag has:

| Property | Type | Description |
|---|---|---|
| `name` | `String` | Unique identifier (≤ 9 chars) |
| `enabled` | `bool` | Global on/off switch |
| `rollout_pct` | `u32` (0–100) | Percentage of callers that see the flag as active |
| `last_updated_ledger` | `u32` | Ledger when the flag was last modified |

---

## Well-Known Flags

| Name | Constant | Description |
|---|---|---|
| `dyn_rate` | `FLAG_DYNAMIC_RATE` | Credit-score-based dynamic interest rate |
| `vote_deleg` | `FLAG_VOTE_DELEGATION` | Governance vote delegation |
| `synth_mon` | `FLAG_SYNTHETIC_MONITORING` | Synthetic monitoring probe |
| `flash_loan` | `FLAG_FLASH_LOAN` | Flash loan feature |
| `slash_v2` | `FLAG_SLASH_V2` | New slash logic canary |

---

## API Reference

### `is_feature_enabled(env, name, caller) -> bool`

Check if a feature is active for a specific caller.

Returns `true` when:
1. The flag exists.
2. `flag.enabled == true`.
3. The caller's deterministic bucket (hash of address mod 100) < `rollout_pct`.

```rust
use crate::feature_flags::is_feature_enabled;

let active = is_feature_enabled(&env, String::from_str(&env, "dyn_rate"), caller);
```

### `set_feature_flag(env, admin, name, enabled, rollout_pct) -> Result<(), ContractError>`

Create or update a feature flag. Requires admin auth.

```rust
use crate::feature_flags::set_feature_flag;

// Enable dynamic rate for 10% of callers.
set_feature_flag(&env, admin, String::from_str(&env, "dyn_rate"), true, 10)?;
```

### `get_feature_flag(env, name) -> Option<FeatureFlag>`

Read a flag's current state.

### `list_feature_flags(env) -> Vec<FeatureFlagSummary>`

List all flags stored in the contract.

### `rollout_step(env, admin, name, delta) -> Result<u32, ContractError>`

Increment a flag's rollout percentage by `delta`. Clamps at 100.

### `kill_flag(env, admin, name) -> Result<(), ContractError>`

Emergency kill-switch: immediately disables a flag for all callers (sets `enabled = false`, `rollout_pct = 0`).

---

## Rollout Procedures

### Creating a New Flag

1. Define a constant in `src/feature_flags.rs`:
   ```rust
   pub const FLAG_MY_FEATURE: &str = "my_feat";
   ```

2. Register the flag at contract initialization (or via admin call):
   ```rust
   set_feature_flag(&env, admin, String::from_str(&env, FLAG_MY_FEATURE), false, 0)?;
   ```

3. Guard the new code path in the contract:
   ```rust
   if is_feature_enabled(&env, String::from_str(&env, FLAG_MY_FEATURE), caller.clone()) {
       // new code path
   } else {
       // legacy code path
   }
   ```

### Gradual Rollout (Canary)

Start at 0% and increment in steps until 100%:

```bash
# Step 1 — 10% of callers
stellar contract invoke --id $CONTRACT_ID --fn set_feature_flag -- \
  --admin $ADMIN_ADDRESS --name slash_v2 --enabled true --rollout_pct 10

# Step 2 — observe metrics; if healthy, advance to 25%
stellar contract invoke --id $CONTRACT_ID --fn rollout_step -- \
  --admin $ADMIN_ADDRESS --name slash_v2 --delta 15

# Step 3 — continue to 50%, 75%, 100% …
```

Recommended rollout schedule:

| Step | Percentage | Wait time |
|---|---|---|
| 1 | 10% | 1 hour |
| 2 | 25% | 2 hours |
| 3 | 50% | 4 hours |
| 4 | 75% | 8 hours |
| 5 | 100% | — |

After each step, verify:
- Error rate < 0.1% in Grafana
- Synthetic checks remain `Healthy`
- No new `ContractError` events in the indexer

### Emergency Rollback

If any step reveals a regression:

```bash
# Immediately disable the feature for all callers.
stellar contract invoke --id $CONTRACT_ID --fn kill_flag -- \
  --admin $ADMIN_ADDRESS --name slash_v2
```

This is instant and does not require a new WASM deployment.

---

## Rollout Percentage Logic

The rollout decision is deterministic per-address:

```
bucket = hash(caller_address) % 100
active  = bucket < rollout_pct
```

The hash is a XOR-fold FNV-1a variant applied to the raw Stellar address bytes. This ensures:

- The same caller always sees the same result for a given `rollout_pct`.
- Incrementing `rollout_pct` only **adds** callers; it does not shuffle the existing set.
- There is no off-chain state required — the contract is self-contained.

---

## Storage

Flags are stored in **persistent Soroban storage** under `FeatureFlagKey::Flag(name)`. The index of all flag names is stored under `FeatureFlagKey::Index`.

Persistent storage survives ledger boundaries and is not affected by contract upgrades unless the admin explicitly migrates data.

---

## Monitoring Integration

Feature flag changes emit no dedicated Soroban event in the current implementation. If you need an audit trail, extend `set_feature_flag` to emit a `flags/change` event:

```rust
env.events().publish(
    (symbol_short!("flags"), symbol_short!("change")),
    (name.clone(), enabled, rollout_pct),
);
```

---

## Security Considerations

- Only admins can create or modify flags (`admin.require_auth()` is enforced).
- Flags default to **fail-closed**: if a flag does not exist in storage, `is_feature_enabled` returns `false`.
- `rollout_pct = 101` or higher is rejected with `ContractError::InvalidAmount`.
- The hash-based bucket is not a cryptographic guarantee — it is a stable sharding function. Do not use it for security-sensitive decisions.

---

## Example: Dynamic Interest Rate Flag

```rust
// In src/loan.rs, inside request_loan():
let name = String::from_str(&env, crate::feature_flags::FLAG_DYNAMIC_RATE);
let use_dynamic_rate = crate::feature_flags::is_feature_enabled(&env, name, borrower.clone());

let effective_rate = if use_dynamic_rate {
    crate::credit_score::dynamic_rate_for_borrower(&env, &borrower)
} else {
    config.yield_bps
};
```
