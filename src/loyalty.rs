//! # Issue #1245 — Loyalty Program with Tiered Rewards
//!
//! Implements a tiered loyalty program to encourage protocol participation:
//!
//! ## Tiers
//! | Tier   | Repayment count | Interest discount | Fee waiver | Min stake discount | Anniversary bonus |
//! |--------|-----------------|-------------------|------------|--------------------|-------------------|
//! | Bronze | 0–4             | 0 bps             | 0%         | 0%                 | 0 bps             |
//! | Silver | 5–19            | 50 bps (0.5%)     | 25%        | 5%                 | 50 bps            |
//! | Gold   | 20+             | 150 bps (1.5%)    | 100%       | 15%                | 150 bps           |
//!
//! ## How it works
//! - `update_loyalty_tier` is called after each successful loan repayment to advance tiers.
//! - `get_loyalty_tier` returns the current tier for any user.
//! - `get_loyalty_benefits` returns the benefit package for the user's current tier.
//! - `claim_anniversary_bonus` allows users to claim a once-per-year bonus.
//! - Benefits (interest discounts, fee waivers) are applied externally by the loan module.

use crate::errors::ContractError;
use crate::helpers::require_not_paused;
use crate::types::{
    DataKey, LoyaltyBenefits, LoyaltyRecord, LoyaltyTier, LOYALTY_ANNIVERSARY_PERIOD_SECS,
    LOYALTY_GOLD_THRESHOLD, LOYALTY_SILVER_THRESHOLD,
    loyalty_bronze_benefits, loyalty_silver_benefits, loyalty_gold_benefits,
};
use soroban_sdk::{symbol_short, Address, Env};

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Load a user's loyalty record, creating a default Bronze record if absent.
fn load_loyalty_record(env: &Env, user: &Address) -> LoyaltyRecord {
    env.storage()
        .persistent()
        .get(&DataKey::LoyaltyRecord(user.clone()))
        .unwrap_or(LoyaltyRecord {
            user: user.clone(),
            tier: LoyaltyTier::Bronze,
            repayment_count: 0,
            member_since: env.ledger().timestamp(),
            last_tier_upgrade_at: 0,
            last_anniversary_bonus_at: 0,
            total_benefits_earned: 0,
        })
}

/// Determine the tier corresponding to a repayment count.
fn tier_for_count(count: u32) -> LoyaltyTier {
    if count >= LOYALTY_GOLD_THRESHOLD {
        LoyaltyTier::Gold
    } else if count >= LOYALTY_SILVER_THRESHOLD {
        LoyaltyTier::Silver
    } else {
        LoyaltyTier::Bronze
    }
}

/// Return the benefits for a given tier.
pub fn benefits_for_tier(tier: LoyaltyTier) -> LoyaltyBenefits {
    match tier {
        LoyaltyTier::Bronze => loyalty_bronze_benefits(),
        LoyaltyTier::Silver => loyalty_silver_benefits(),
        LoyaltyTier::Gold => loyalty_gold_benefits(),
    }
}

// ── Public functions ──────────────────────────────────────────────────────────

/// Get the current loyalty tier for a user.
///
/// Returns `LoyaltyTier::Bronze` for users with no loan history.
pub fn get_loyalty_tier(env: Env, user: Address) -> LoyaltyTier {
    load_loyalty_record(&env, &user).tier
}

/// Get the full loyalty record for a user.
pub fn get_loyalty_record(env: Env, user: Address) -> LoyaltyRecord {
    load_loyalty_record(&env, &user)
}

/// Get the loyalty benefits package for a user's current tier.
pub fn get_loyalty_benefits(env: Env, user: Address) -> LoyaltyBenefits {
    let record = load_loyalty_record(&env, &user);
    benefits_for_tier(record.tier)
}

/// Update a user's loyalty tier after a successful loan repayment.
///
/// Called internally after `repay()` succeeds. Increments the repayment count
/// and advances the tier if the new count crosses a threshold.
///
/// Emits event `loyalty/upgrade` with `(user, old_tier, new_tier)` when tier advances.
/// Emits event `loyalty/repay` with `(user, repayment_count, tier)` on every call.
///
/// This function is **not** guarded by `require_not_paused` — it is called from
/// within the repay flow that already checked pause state.
pub fn record_repayment_for_loyalty(env: &Env, user: &Address) {
    let mut record = load_loyalty_record(env, user);
    record.repayment_count += 1;

    let new_tier = tier_for_count(record.repayment_count);
    let old_tier = record.tier;

    if new_tier != old_tier {
        record.tier = new_tier;
        record.last_tier_upgrade_at = env.ledger().timestamp();

        env.events().publish(
            (symbol_short!("loyalty"), symbol_short!("upgrade")),
            (user.clone(), old_tier, new_tier),
        );
    }

    env.storage()
        .persistent()
        .set(&DataKey::LoyaltyRecord(user.clone()), &record);

    env.events().publish(
        (symbol_short!("loyalty"), symbol_short!("repay")),
        (user.clone(), record.repayment_count, record.tier),
    );
}

/// Claim the anniversary loyalty bonus.
///
/// The anniversary bonus is available once per year from the user's `member_since` date.
/// It applies a yield bonus (in basis points) to the user's next repayment reward.
/// The bonus amount (in stroops) is recorded in `total_benefits_earned`.
///
/// # Arguments
/// * `user`          — The user claiming the bonus.
/// * `loan_principal`— The principal of the current/most recent loan (used to compute bonus amount).
///
/// Returns the anniversary bonus amount in basis points for external application.
///
/// Emits event: `loyalty/anniv` with `(user, tier, bonus_bps)`.
pub fn claim_anniversary_bonus(
    env: Env,
    user: Address,
    loan_principal: i128,
) -> Result<u32, ContractError> {
    require_not_paused(&env)?;
    user.require_auth();

    let mut record = load_loyalty_record(&env, &user);
    let benefits = benefits_for_tier(record.tier);

    if benefits.anniversary_bonus_bps == 0 {
        // Bronze tier has no anniversary bonus
        return Ok(0);
    }

    let now = env.ledger().timestamp();

    // Check if one full year has elapsed since the last claim (or member_since).
    let last_claim = if record.last_anniversary_bonus_at > 0 {
        record.last_anniversary_bonus_at
    } else {
        record.member_since
    };

    if now < last_claim + LOYALTY_ANNIVERSARY_PERIOD_SECS {
        // Not yet eligible — return 0 without error so callers can check
        return Ok(0);
    }

    // Record the claim
    record.last_anniversary_bonus_at = now;

    // Accumulate benefits earned (approximate stroops value of the bonus)
    if loan_principal > 0 {
        let bonus_stroops =
            loan_principal * benefits.anniversary_bonus_bps as i128 / 10_000;
        record.total_benefits_earned += bonus_stroops;
    }

    env.storage()
        .persistent()
        .set(&DataKey::LoyaltyRecord(user.clone()), &record);

    env.events().publish(
        (symbol_short!("loyalty"), symbol_short!("anniv")),
        (user, record.tier, benefits.anniversary_bonus_bps),
    );

    Ok(benefits.anniversary_bonus_bps)
}

/// Apply the loyalty interest discount to a given base rate.
///
/// Returns the adjusted rate in basis points, floored at 0.
///
/// # Example
/// Base rate = 500 bps, Silver discount = 50 bps → effective rate = 450 bps.
pub fn apply_loyalty_interest_discount(env: &Env, user: &Address, base_rate_bps: i128) -> i128 {
    let record = load_loyalty_record(env, user);
    let benefits = benefits_for_tier(record.tier);
    (base_rate_bps - benefits.interest_rate_discount_bps).max(0)
}

/// Apply the loyalty fee waiver to a given fee amount.
///
/// Returns the effective fee in stroops after the waiver.
///
/// # Example
/// Fee = 1000 stroops, Gold waiver = 100% (10000 bps) → effective fee = 0.
pub fn apply_loyalty_fee_waiver(env: &Env, user: &Address, fee_stroops: i128) -> i128 {
    let record = load_loyalty_record(env, user);
    let benefits = benefits_for_tier(record.tier);
    let waiver = fee_stroops * benefits.fee_waiver_bps as i128 / 10_000;
    (fee_stroops - waiver).max(0)
}

/// Apply the loyalty minimum-stake discount to a given minimum stake.
///
/// Returns the effective minimum stake in stroops.
pub fn apply_loyalty_min_stake_discount(env: &Env, user: &Address, min_stake: i128) -> i128 {
    let record = load_loyalty_record(env, user);
    let benefits = benefits_for_tier(record.tier);
    let discount = min_stake * benefits.min_stake_discount_bps as i128 / 10_000;
    (min_stake - discount).max(1) // Never reduce below 1 stroop
}
