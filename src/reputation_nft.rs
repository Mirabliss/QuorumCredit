//! # Reputation NFTs as Achievement Badges (Issue #1251)
//!
//! Issues on-chain NFT-style badges tied to lending milestones.
//!
//! ## Badge Tiers
//!
//! | Badge            | Trigger                                      |
//! |------------------|----------------------------------------------|
//! | `FirstLoan`      | Borrower successfully repays first loan.     |
//! | `TenLoans`       | Borrower successfully repays 10th loan.      |
//! | `Centurion`      | Borrower's on-chain reputation score ≥ 100. |
//! | `TopVoucher`     | Voucher has backed ≥ 5 repaid loans.         |
//! | `TrustPillar`    | Voucher has backed ≥ 20 repaid loans.        |
//!
//! ## Staking for Benefits
//!
//! Badge holders can stake their badge to earn a yield bonus.  Staked badges
//! are locked and cannot be transferred until unstaked.  The yield bonus is
//! applied as an additive BPS increment during repayment calculations.
//!
//! ## Marketplace
//!
//! Unstaked badges can be listed for sale.  A buyer pays the listing price
//! and the badge ownership is transferred.  The marketplace is permissionless:
//! any badge holder may list or delist.

#![allow(unused)]

use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers::require_not_paused;
use crate::types::DataKey;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Yield bonus applied when a `FirstLoan` badge is staked, in basis points (25 = 0.25%).
pub const FIRST_LOAN_BADGE_YIELD_BONUS_BPS: i128 = 25;

/// Yield bonus applied when a `TenLoans` badge is staked, in basis points (50 = 0.5%).
pub const TEN_LOANS_BADGE_YIELD_BONUS_BPS: i128 = 50;

/// Yield bonus applied when a `Centurion` badge is staked, in basis points (100 = 1%).
pub const CENTURION_BADGE_YIELD_BONUS_BPS: i128 = 100;

/// Yield bonus applied when a `TopVoucher` badge is staked, in basis points (30 = 0.3%).
pub const TOP_VOUCHER_BADGE_YIELD_BONUS_BPS: i128 = 30;

/// Yield bonus applied when a `TrustPillar` badge is staked, in basis points (75 = 0.75%).
pub const TRUST_PILLAR_BADGE_YIELD_BONUS_BPS: i128 = 75;

/// Minimum successful repayments by a backing voucher to qualify for `TopVoucher` badge.
pub const TOP_VOUCHER_MIN_BACKED: u32 = 5;

/// Minimum successful repayments by a backing voucher to qualify for `TrustPillar` badge.
pub const TRUST_PILLAR_MIN_BACKED: u32 = 20;

/// Reputation score threshold to qualify for `Centurion` badge.
pub const CENTURION_SCORE_THRESHOLD: u32 = 100;

// ── Data Structures ───────────────────────────────────────────────────────────

/// The set of achievement badge types.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BadgeType {
    /// Awarded to a borrower upon first successful loan repayment.
    FirstLoan,
    /// Awarded to a borrower upon 10th successful loan repayment.
    TenLoans,
    /// Awarded when on-chain reputation score reaches 100.
    Centurion,
    /// Awarded to a voucher who has backed ≥ 5 repaid loans.
    TopVoucher,
    /// Awarded to a voucher who has backed ≥ 20 repaid loans.
    TrustPillar,
}

/// An individual badge record.
#[contracttype]
#[derive(Clone)]
pub struct Badge {
    /// The badge type / tier.
    pub badge_type: BadgeType,
    /// The owner of this badge.
    pub owner: Address,
    /// Ledger timestamp when the badge was minted.
    pub minted_at: u64,
    /// Whether the badge is currently staked for a yield bonus.
    pub staked: bool,
    /// Whether the badge is currently listed on the marketplace.
    pub listed_for_sale: bool,
    /// Listing price in stroops (0 when not listed).
    pub listing_price: i128,
}

/// Aggregate rarity and distribution statistics for a badge type.
#[contracttype]
#[derive(Clone)]
pub struct BadgeStats {
    /// How many badges of this type have ever been minted.
    pub total_minted: u64,
    /// How many are currently staked.
    pub total_staked: u64,
    /// How many are currently listed on the marketplace.
    pub total_listed: u64,
}

// ── DataKey extensions (added to types.rs) ────────────────────────────────────
//
//   ReputationBadge(Address, BadgeType)  — Badge record
//   BadgeStats(BadgeType)                — BadgeStats per type
//   VoucherBackedCount(Address)          — u32: repaid loans backed by voucher

// ── Minting ───────────────────────────────────────────────────────────────────

/// Mint a badge for `owner` of type `badge_type`.
///
/// Idempotent: if the owner already holds this badge, the call succeeds
/// without changing state.
///
/// This is an internal helper called by the loan repayment path.
pub fn mint_badge(env: &Env, owner: &Address, badge_type: BadgeType) {
    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    if env.storage().persistent().has(&key) {
        return; // Already minted — idempotent.
    }

    let badge = Badge {
        badge_type: badge_type.clone(),
        owner: owner.clone(),
        minted_at: env.ledger().timestamp(),
        staked: false,
        listed_for_sale: false,
        listing_price: 0,
    };

    env.storage().persistent().set(&key, &badge);

    // Update stats.
    let mut stats: BadgeStats = env
        .storage()
        .persistent()
        .get(&DataKey::BadgeStats(badge_type.clone()))
        .unwrap_or(BadgeStats {
            total_minted: 0,
            total_staked: 0,
            total_listed: 0,
        });
    stats.total_minted += 1;
    env.storage()
        .persistent()
        .set(&DataKey::BadgeStats(badge_type.clone()), &stats);

    env.events().publish(
        (symbol_short!("badge"), symbol_short!("mint")),
        (owner.clone(), badge_type),
    );
}

/// Evaluate and mint any newly earned badges for `address` after a repayment.
///
/// Checks:
/// - Borrower repayment count → `FirstLoan`, `TenLoans`.
/// - On-chain reputation score → `Centurion`.
/// - Voucher backed count → `TopVoucher`, `TrustPillar`.
pub fn evaluate_and_mint_badges(env: &Env, address: &Address) {
    // Borrower milestone badges.
    let repayment_count: u32 = env
        .storage()
        .persistent()
        .get::<DataKey, u32>(&DataKey::RepaymentCount(address.clone()))
        .unwrap_or(0);

    if repayment_count >= 1 {
        mint_badge(env, address, BadgeType::FirstLoan);
    }
    if repayment_count >= 10 {
        mint_badge(env, address, BadgeType::TenLoans);
    }

    // Centurion badge: reputation score ≥ 100.
    let rep_score: u32 = env
        .storage()
        .persistent()
        .get::<DataKey, u32>(&DataKey::ReputationScore(address.clone()))
        .unwrap_or(0);
    if rep_score >= CENTURION_SCORE_THRESHOLD {
        mint_badge(env, address, BadgeType::Centurion);
    }

    // Voucher milestone badges.
    let backed_count: u32 = env
        .storage()
        .persistent()
        .get::<DataKey, u32>(&DataKey::VoucherBackedCount(address.clone()))
        .unwrap_or(0);
    if backed_count >= TOP_VOUCHER_MIN_BACKED {
        mint_badge(env, address, BadgeType::TopVoucher);
    }
    if backed_count >= TRUST_PILLAR_MIN_BACKED {
        mint_badge(env, address, BadgeType::TrustPillar);
    }
}

// ── Staking ───────────────────────────────────────────────────────────────────

/// Stake a badge to activate its yield bonus.
///
/// # Errors
/// - `InvalidAmount`    — badge not found.
/// - `InvalidStateTransition` — badge is listed for sale; delist first.
/// - `ContractPaused`   — contract is paused.
pub fn stake_badge(
    env: &Env,
    owner: Address,
    badge_type: BadgeType,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    owner.require_auth();

    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    let mut badge: Badge = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    if badge.listed_for_sale {
        return Err(ContractError::InvalidStateTransition);
    }
    if badge.staked {
        return Ok(()); // Already staked — idempotent.
    }

    badge.staked = true;
    env.storage().persistent().set(&key, &badge);

    // Update stats.
    let stats_key = DataKey::BadgeStats(badge_type.clone());
    let mut stats: BadgeStats = env
        .storage()
        .persistent()
        .get(&stats_key)
        .unwrap_or(BadgeStats { total_minted: 0, total_staked: 0, total_listed: 0 });
    stats.total_staked += 1;
    env.storage().persistent().set(&stats_key, &stats);

    env.events().publish(
        (symbol_short!("badge"), symbol_short!("stake")),
        (owner, badge_type),
    );

    Ok(())
}

/// Unstake a badge, deactivating its yield bonus.
///
/// # Errors
/// - `InvalidAmount`    — badge not found.
/// - `ContractPaused`   — contract is paused.
pub fn unstake_badge(
    env: &Env,
    owner: Address,
    badge_type: BadgeType,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    owner.require_auth();

    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    let mut badge: Badge = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    if !badge.staked {
        return Ok(()); // Already unstaked — idempotent.
    }

    badge.staked = false;
    env.storage().persistent().set(&key, &badge);

    let stats_key = DataKey::BadgeStats(badge_type.clone());
    if let Some(mut stats) = env
        .storage()
        .persistent()
        .get::<DataKey, BadgeStats>(&stats_key)
    {
        stats.total_staked = stats.total_staked.saturating_sub(1);
        env.storage().persistent().set(&stats_key, &stats);
    }

    env.events().publish(
        (symbol_short!("badge"), symbol_short!("unstake")),
        (owner, badge_type),
    );

    Ok(())
}

/// Return the yield bonus BPS for a given badge type if staked, else 0.
pub fn get_staked_yield_bonus(env: &Env, owner: &Address, badge_type: &BadgeType) -> i128 {
    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    let badge: Option<Badge> = env.storage().persistent().get(&key);
    match badge {
        Some(b) if b.staked => match badge_type {
            BadgeType::FirstLoan => FIRST_LOAN_BADGE_YIELD_BONUS_BPS,
            BadgeType::TenLoans => TEN_LOANS_BADGE_YIELD_BONUS_BPS,
            BadgeType::Centurion => CENTURION_BADGE_YIELD_BONUS_BPS,
            BadgeType::TopVoucher => TOP_VOUCHER_BADGE_YIELD_BONUS_BPS,
            BadgeType::TrustPillar => TRUST_PILLAR_BADGE_YIELD_BONUS_BPS,
        },
        _ => 0,
    }
}

/// Return the total staked yield bonus for all badges held by `owner`.
pub fn total_staked_yield_bonus(env: &Env, owner: &Address) -> i128 {
    let badge_types = [
        BadgeType::FirstLoan,
        BadgeType::TenLoans,
        BadgeType::Centurion,
        BadgeType::TopVoucher,
        BadgeType::TrustPillar,
    ];
    badge_types
        .iter()
        .map(|bt| get_staked_yield_bonus(env, owner, bt))
        .sum()
}

// ── Marketplace ───────────────────────────────────────────────────────────────

/// List a badge for sale on the marketplace.
///
/// # Errors
/// - `InvalidAmount`          — badge not found or `price` is zero.
/// - `InvalidStateTransition` — badge is currently staked.
/// - `ContractPaused`         — contract is paused.
pub fn list_badge_for_sale(
    env: &Env,
    owner: Address,
    badge_type: BadgeType,
    price: i128,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    owner.require_auth();

    if price <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    let mut badge: Badge = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    if badge.staked {
        return Err(ContractError::InvalidStateTransition);
    }

    badge.listed_for_sale = true;
    badge.listing_price = price;
    env.storage().persistent().set(&key, &badge);

    let stats_key = DataKey::BadgeStats(badge_type.clone());
    let mut stats: BadgeStats = env
        .storage()
        .persistent()
        .get(&stats_key)
        .unwrap_or(BadgeStats { total_minted: 0, total_staked: 0, total_listed: 0 });
    stats.total_listed += 1;
    env.storage().persistent().set(&stats_key, &stats);

    env.events().publish(
        (symbol_short!("badge"), symbol_short!("list")),
        (owner, badge_type, price),
    );

    Ok(())
}

/// Delist a badge from the marketplace.
///
/// # Errors
/// - `InvalidAmount` — badge not found.
pub fn delist_badge(
    env: &Env,
    owner: Address,
    badge_type: BadgeType,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    owner.require_auth();

    let key = DataKey::ReputationBadge(owner.clone(), badge_type.clone());
    let mut badge: Badge = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    if !badge.listed_for_sale {
        return Ok(());
    }

    badge.listed_for_sale = false;
    badge.listing_price = 0;
    env.storage().persistent().set(&key, &badge);

    let stats_key = DataKey::BadgeStats(badge_type.clone());
    if let Some(mut stats) = env
        .storage()
        .persistent()
        .get::<DataKey, BadgeStats>(&stats_key)
    {
        stats.total_listed = stats.total_listed.saturating_sub(1);
        env.storage().persistent().set(&stats_key, &stats);
    }

    Ok(())
}

/// Purchase a badge from the marketplace (off-chain token transfer assumed).
///
/// This transfers badge ownership on-chain.  Token payment is handled by the
/// caller (client-side) before invoking this function.
///
/// # Errors
/// - `InvalidAmount`          — badge not found or not listed.
/// - `ContractPaused`         — contract is paused.
pub fn purchase_badge(
    env: &Env,
    buyer: Address,
    seller: Address,
    badge_type: BadgeType,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    buyer.require_auth();

    let key = DataKey::ReputationBadge(seller.clone(), badge_type.clone());
    let mut badge: Badge = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    if !badge.listed_for_sale {
        return Err(ContractError::InvalidAmount);
    }

    // Remove badge from seller.
    env.storage().persistent().remove(&key);

    // Mint to buyer with same metadata but reset marketplace flags.
    let new_badge = Badge {
        badge_type: badge_type.clone(),
        owner: buyer.clone(),
        minted_at: badge.minted_at,
        staked: false,
        listed_for_sale: false,
        listing_price: 0,
    };

    env.storage()
        .persistent()
        .set(&DataKey::ReputationBadge(buyer.clone(), badge_type.clone()), &new_badge);

    let stats_key = DataKey::BadgeStats(badge_type.clone());
    if let Some(mut stats) = env
        .storage()
        .persistent()
        .get::<DataKey, BadgeStats>(&stats_key)
    {
        stats.total_listed = stats.total_listed.saturating_sub(1);
        env.storage().persistent().set(&stats_key, &stats);
    }

    env.events().publish(
        (symbol_short!("badge"), symbol_short!("sold")),
        (seller, buyer, badge_type, badge.listing_price),
    );

    Ok(())
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// Return the badge record for `owner` and `badge_type`, if it exists.
pub fn get_badge(env: &Env, owner: &Address, badge_type: BadgeType) -> Option<Badge> {
    env.storage()
        .persistent()
        .get(&DataKey::ReputationBadge(owner.clone(), badge_type))
}

/// Return distribution stats for a badge type.
pub fn get_badge_stats(env: &Env, badge_type: BadgeType) -> BadgeStats {
    env.storage()
        .persistent()
        .get(&DataKey::BadgeStats(badge_type))
        .unwrap_or(BadgeStats {
            total_minted: 0,
            total_staked: 0,
            total_listed: 0,
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let owner = Address::generate(&env);
        (env, owner)
    }

    #[test]
    fn test_mint_badge_idempotent() {
        let (env, owner) = setup();
        mint_badge(&env, &owner, BadgeType::FirstLoan);
        mint_badge(&env, &owner, BadgeType::FirstLoan); // second call is no-op
        let stats = get_badge_stats(&env, BadgeType::FirstLoan);
        assert_eq!(stats.total_minted, 1);
    }

    #[test]
    fn test_stake_and_unstake() {
        let (env, owner) = setup();
        mint_badge(&env, &owner, BadgeType::TenLoans);
        stake_badge(&env, owner.clone(), BadgeType::TenLoans).unwrap();

        let bonus = get_staked_yield_bonus(&env, &owner, &BadgeType::TenLoans);
        assert_eq!(bonus, TEN_LOANS_BADGE_YIELD_BONUS_BPS);

        unstake_badge(&env, owner.clone(), BadgeType::TenLoans).unwrap();
        let bonus_after = get_staked_yield_bonus(&env, &owner, &BadgeType::TenLoans);
        assert_eq!(bonus_after, 0);
    }

    #[test]
    fn test_cannot_stake_listed_badge() {
        let (env, owner) = setup();
        mint_badge(&env, &owner, BadgeType::Centurion);
        list_badge_for_sale(&env, owner.clone(), BadgeType::Centurion, 1_000_000).unwrap();
        let result = stake_badge(&env, owner, BadgeType::Centurion);
        assert_eq!(result, Err(ContractError::InvalidStateTransition));
    }

    #[test]
    fn test_list_and_purchase_badge() {
        let (env, seller) = setup();
        let buyer = Address::generate(&env);

        mint_badge(&env, &seller, BadgeType::TopVoucher);
        list_badge_for_sale(&env, seller.clone(), BadgeType::TopVoucher, 500_000).unwrap();
        purchase_badge(&env, buyer.clone(), seller.clone(), BadgeType::TopVoucher).unwrap();

        // Seller no longer has the badge.
        assert!(get_badge(&env, &seller, BadgeType::TopVoucher).is_none());
        // Buyer now has it.
        let b = get_badge(&env, &buyer, BadgeType::TopVoucher).unwrap();
        assert_eq!(b.owner, buyer);
        assert!(!b.listed_for_sale);
    }

    #[test]
    fn test_total_staked_yield_bonus() {
        let (env, owner) = setup();
        mint_badge(&env, &owner, BadgeType::FirstLoan);
        mint_badge(&env, &owner, BadgeType::TenLoans);
        stake_badge(&env, owner.clone(), BadgeType::FirstLoan).unwrap();
        stake_badge(&env, owner.clone(), BadgeType::TenLoans).unwrap();

        let total = total_staked_yield_bonus(&env, &owner);
        assert_eq!(
            total,
            FIRST_LOAN_BADGE_YIELD_BONUS_BPS + TEN_LOANS_BADGE_YIELD_BONUS_BPS
        );
    }

    #[test]
    fn test_evaluate_and_mint_first_loan_badge() {
        let (env, borrower) = setup();
        // Set repayment count to 1.
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower.clone()), &1u32);
        evaluate_and_mint_badges(&env, &borrower);
        assert!(get_badge(&env, &borrower, BadgeType::FirstLoan).is_some());
        assert!(get_badge(&env, &borrower, BadgeType::TenLoans).is_none());
    }

    #[test]
    fn test_evaluate_and_mint_ten_loans_badge() {
        let (env, borrower) = setup();
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower.clone()), &10u32);
        evaluate_and_mint_badges(&env, &borrower);
        assert!(get_badge(&env, &borrower, BadgeType::FirstLoan).is_some());
        assert!(get_badge(&env, &borrower, BadgeType::TenLoans).is_some());
    }
}
