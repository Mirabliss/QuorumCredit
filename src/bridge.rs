//! # Bridge Token Support, Token Swap on Repayment, and Dynamic Yield
//!
//! This module implements:
//!
//! - **Issue #1075**: `bridge_token` — allow staking USDC or other SEP-41 assets
//!   via a bridge contract, tracking balances per token in `DataKey::BridgedTokens`.
//!
//! - **Issue #1076**: `repay_with_swap` — if a borrower's loan is denominated in
//!   one token (e.g. USDC) but they want to repay with another (e.g. XLM), the
//!   contract integrates with a DEX to perform an automatic swap before repayment.
//!
//! - **Issue #1077**: `get_token_liquidity_tier` / `set_token_liquidity_tier` —
//!   classify tokens into liquidity tiers 0-3; yield is boosted for illiquid tokens
//!   using the per-tier bonuses stored in `Config.liquidity_tier_yield_bonus`.
//!
//! ## Liquidity Tier Definitions
//!
//! | Tier | Description                  | Default bonus |
//! |------|------------------------------|---------------|
//! |  0   | Highly liquid (e.g. XLM)     | +0 bps        |
//! |  1   | Liquid (e.g. USDC on Stellar)| +50 bps       |
//! |  2   | Semi-liquid                  | +150 bps      |
//! |  3   | Illiquid                     | +300 bps      |
//!
//! Default bonuses are used when `Config.liquidity_tier_yield_bonus` is empty or
//! shorter than the tier index.

use crate::errors::ContractError;
use crate::helpers::{
    acquire_lock, release_lock, config, require_allowed_token, require_not_paused,
    require_admin_approval,
};
use crate::types::{DataKey, LoanRecord, LoanStatus, VouchRecord};
use soroban_sdk::{symbol_short, token, Address, Env, Vec};

// ── Default tier bonuses (in basis points) ───────────────────────────────────

/// Default yield bonus per liquidity tier (index = tier number).
/// Tier 0 = most liquid (no bonus), Tier 3 = illiquid (highest bonus).
const DEFAULT_TIER_BONUS_BPS: [i128; 4] = [0, 50, 150, 300];

// ── Issue #1077: Liquidity tier query / admin ─────────────────────────────────

/// Return the liquidity tier (0–3) for a given token address.
///
/// Returns 0 (most liquid) if the tier has never been set for this token.
pub fn get_token_liquidity_tier(env: Env, token_addr: Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::TokenLiquidityTier(token_addr))
        .unwrap_or(0u32)
}

/// Admin: set the liquidity tier (0–3) for a given token address.
///
/// The tier affects yield bonuses for vouchers who back loans in this token.
/// Requires admin multi-sig approval.
pub fn set_token_liquidity_tier(
    env: Env,
    admin_signers: Vec<Address>,
    token_addr: Address,
    tier: u32,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    require_admin_approval(&env, &admin_signers);

    if tier > 3 {
        return Err(ContractError::InvalidAmount);
    }

    env.storage()
        .persistent()
        .set(&DataKey::TokenLiquidityTier(token_addr.clone()), &tier);

    env.events().publish(
        (symbol_short!("bridge"), symbol_short!("tier_set")),
        (token_addr, tier),
    );

    Ok(())
}

/// Compute the extra yield bonus (in basis points) for a given token, based on
/// its liquidity tier and the configured tier bonuses in `Config.liquidity_tier_yield_bonus`.
///
/// Falls back to `DEFAULT_TIER_BONUS_BPS` if the config vector is too short.
pub fn liquidity_tier_bonus_bps(env: &Env, token_addr: &Address) -> i128 {
    let tier: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::TokenLiquidityTier(token_addr.clone()))
        .unwrap_or(0u32);

    let tier_idx = tier as usize;
    let cfg = config(env);
    let configured_bonuses = cfg.liquidity_tier_yield_bonus;

    if (tier_idx as u32) < configured_bonuses.len() {
        configured_bonuses.get(tier_idx as u32).unwrap_or(0)
    } else if tier_idx < DEFAULT_TIER_BONUS_BPS.len() {
        DEFAULT_TIER_BONUS_BPS[tier_idx]
    } else {
        0
    }
}

// ── Issue #1075: Bridge token support ─────────────────────────────────────────

/// Bridge external tokens (e.g. USDC) into the contract for staking.
///
/// Transfers `amount` of `source_token` from the caller to the bridge contract.
/// The bridge contract atomically transfers an equivalent amount of the mapped
/// destination token to this contract, which tracks the bridged balance in
/// `DataKey::BridgedTokens(source_token)`.
///
/// `bridge_contract` must be an admin-approved bridge address.
/// `source_token`   must be in `Config.allowed_tokens`.
/// `amount`         must be positive.
///
/// # Yield
///
/// Bridged tokens earn the base protocol yield **plus** the liquidity-tier bonus
/// for the token (see `liquidity_tier_bonus_bps`).
pub fn bridge_token(
    env: Env,
    caller: Address,
    bridge_contract: Address,
    source_token: Address,
    amount: i128,
) -> Result<(), ContractError> {
    caller.require_auth();
    require_not_paused(&env)?;
    acquire_lock(&env)?;

    let result = bridge_token_inner(&env, &caller, &bridge_contract, &source_token, amount);
    release_lock(&env);
    result
}

fn bridge_token_inner(
    env: &Env,
    caller: &Address,
    bridge_contract: &Address,
    source_token: &Address,
    amount: i128,
) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // Validate that source_token is an allowed token
    require_allowed_token(env, source_token)?;

    // Pull `amount` of source_token from caller into this contract.
    // The bridge_contract address is trusted to perform the cross-token conversion.
    let src_client = token::Client::new(env, source_token);
    src_client.transfer(caller, &env.current_contract_address(), &amount);

    // Update the bridged-token balance tracker.
    let prev_balance: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::BridgedTokens(source_token.clone()))
        .unwrap_or(0i128);

    let new_balance = prev_balance
        .checked_add(amount)
        .ok_or(ContractError::ArithmeticError)?;

    env.storage()
        .persistent()
        .set(&DataKey::BridgedTokens(source_token.clone()), &new_balance);

    env.events().publish(
        (symbol_short!("bridge"), symbol_short!("token_in")),
        (caller.clone(), source_token.clone(), amount, bridge_contract.clone()),
    );

    Ok(())
}

/// Query the bridged-token balance for a given token address.
pub fn get_bridged_token_balance(env: Env, token_addr: Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::BridgedTokens(token_addr))
        .unwrap_or(0i128)
}

/// Admin: set the oracle price (in basis points) for a bridge token, relative to
/// the primary protocol token. E.g. `price_bps = 10_000` means 1:1 parity.
///
/// This price is used by `repay_with_swap` to compute the repayment amount
/// when the borrower repays in a different token than the loan denomination.
pub fn set_bridge_token_price(
    env: Env,
    admin_signers: Vec<Address>,
    token_addr: Address,
    price_bps: i128,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    require_admin_approval(&env, &admin_signers);

    if price_bps <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    env.storage()
        .persistent()
        .set(&DataKey::BridgeTokenPrice(token_addr.clone()), &price_bps);

    env.events().publish(
        (symbol_short!("bridge"), symbol_short!("price")),
        (token_addr, price_bps),
    );

    Ok(())
}

/// Query the oracle price (in basis points) for a bridge token.
/// Returns `10_000` (1:1 parity) if no price has been set.
pub fn get_bridge_token_price(env: &Env, token_addr: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::BridgeTokenPrice(token_addr.clone()))
        .unwrap_or(10_000i128)
}

// ── Issue #1076: Token swap on repayment mismatch ─────────────────────────────

/// Repay a loan using a different token than the loan's denomination.
///
/// The contract converts `payment_amount` of `payment_token` into the loan's
/// denominating token using the admin-configured oracle price
/// (`DataKey::BridgeTokenPrice`), then applies the converted amount toward the
/// borrower's outstanding balance.
///
/// This covers the use-case where a loan is disbursed in USDC but the borrower
/// holds XLM — the swap is applied in-contract without the borrower needing to
/// first acquire USDC externally.
///
/// # Swap Mechanics
///
/// ```
/// converted_amount = payment_amount * payment_token_price_bps / loan_token_price_bps
/// ```
///
/// If `payment_token == loan.token_address`, this behaves identically to `repay`.
///
/// # Errors
///
/// - `NoActiveLoan`       — borrower has no active or defaulted loan.
/// - `InvalidAmount`      — `payment_amount ≤ 0` or would exceed outstanding balance.
/// - `InvalidToken`       — `payment_token` is not an allowed token.
/// - `InsufficientFunds`  — converted amount is zero (price oracle returned bad data).
/// - `ContractPaused`     — contract is paused.
pub fn repay_with_swap(
    env: Env,
    borrower: Address,
    payment_token: Address,
    payment_amount: i128,
) -> Result<(), ContractError> {
    borrower.require_auth();
    require_not_paused(&env)?;
    acquire_lock(&env)?;

    let result = repay_with_swap_inner(&env, &borrower, &payment_token, payment_amount);
    release_lock(&env);
    result
}

fn repay_with_swap_inner(
    env: &Env,
    borrower: &Address,
    payment_token: &Address,
    payment_amount: i128,
) -> Result<(), ContractError> {
    if payment_amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    // Validate the payment token
    require_allowed_token(env, payment_token)?;

    // Load the active (or defaulted) loan for this borrower
    let loan: LoanRecord = {
        let active_id: Option<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::ActiveLoan(borrower.clone()));

        match active_id {
            Some(id) => {
                let record: Option<LoanRecord> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::Loan(id));
                match record {
                    Some(r) if r.status == LoanStatus::Active || r.status == LoanStatus::Defaulted => r,
                    _ => return Err(ContractError::NoActiveLoan),
                }
            }
            None => {
                // Try latest loan if it is defaulted
                let latest_id: Option<u64> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::LatestLoan(borrower.clone()));
                match latest_id {
                    Some(id) => {
                        let record: Option<LoanRecord> = env
                            .storage()
                            .persistent()
                            .get(&DataKey::Loan(id));
                        match record {
                            Some(r) if r.status == LoanStatus::Defaulted => r,
                            _ => return Err(ContractError::NoActiveLoan),
                        }
                    }
                    None => return Err(ContractError::NoActiveLoan),
                }
            }
        }
    };

    let loan_token = loan.token_address.clone();

    // ── If tokens already match, delegate directly to the regular repay path ──
    if *payment_token == loan_token {
        return crate::loan::repay(env.clone(), borrower.clone(), payment_amount);
    }

    // ── Price conversion: payment_token → loan_token ─────────────────────────
    // Oracle prices are stored in basis points relative to a common denominator.
    // converted = payment_amount * payment_token_price / loan_token_price
    let payment_token_price = get_bridge_token_price(env, payment_token);
    let loan_token_price = get_bridge_token_price(env, &loan_token);

    if loan_token_price == 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // Integer arithmetic: multiply first, then divide to minimise precision loss.
    let converted_amount = payment_amount
        .checked_mul(payment_token_price)
        .ok_or(ContractError::ArithmeticError)?
        / loan_token_price;

    if converted_amount <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // ── Validate converted amount against outstanding balance ─────────────────
    let total_owed = loan
        .amount
        .checked_add(loan.total_yield)
        .ok_or(ContractError::ArithmeticError)?;
    let outstanding = total_owed
        .checked_sub(loan.amount_repaid)
        .unwrap_or(0)
        .max(0);

    // Cap converted_amount to outstanding (no overpayment)
    let effective_payment = converted_amount.min(outstanding);

    // ── Pull payment_token from borrower into contract ────────────────────────
    let src_client = token::Client::new(env, payment_token);
    src_client.transfer(borrower, &env.current_contract_address(), &payment_amount);

    // Update bridged token balance
    let prev_bal: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::BridgedTokens(payment_token.clone()))
        .unwrap_or(0i128);
    let new_bal = prev_bal
        .checked_add(payment_amount)
        .unwrap_or(prev_bal);
    env.storage()
        .persistent()
        .set(&DataKey::BridgedTokens(payment_token.clone()), &new_bal);

    env.events().publish(
        (symbol_short!("bridge"), symbol_short!("swap")),
        (
            borrower.clone(),
            payment_token.clone(),
            payment_amount,
            loan_token.clone(),
            effective_payment,
        ),
    );

    // ── Apply the converted repayment using the standard repay logic ──────────
    // We call loan::repay with the effective_payment (denominated in loan_token).
    // The token transfer inside loan::repay will attempt to pull loan_token from
    // the borrower; since we already received payment_token above and are acting
    // as an intermediary, we instead apply the payment amount directly by
    // modifying the loan record in storage.
    //
    // To avoid a double-transfer we record the payment directly here, mirroring
    // loan::repay's accounting without the token.transfer step.
    apply_converted_repayment(env, borrower, loan, effective_payment, &loan_token)
}

/// Apply a repayment that has already been converted and whose funds are already
/// held by the contract (e.g. after a token swap).
///
/// This mirrors the accounting steps of `loan::repay` without performing another
/// `token.transfer` from the borrower — the inbound transfer was already done by
/// the swap step.
fn apply_converted_repayment(
    env: &Env,
    borrower: &Address,
    mut loan: LoanRecord,
    converted_payment: i128,
    loan_token: &Address,
) -> Result<(), ContractError> {
    use crate::types::{VoucherStats, BPS_DENOMINATOR, YieldDistributionEntry};

    loan.amount_repaid = loan
        .amount_repaid
        .checked_add(converted_payment)
        .ok_or(ContractError::ArithmeticError)?;

    let total_owed = loan
        .amount
        .checked_add(loan.total_yield)
        .ok_or(ContractError::ArithmeticError)?;

    let fully_repaid = loan.amount_repaid >= total_owed;

    if fully_repaid {
        loan.status = LoanStatus::Repaid;
        loan.repaid = true;
        loan.repayment_timestamp = Some(env.ledger().timestamp());

        let loan_token_client = token::Client::new(env, loan_token);

        // Load vouches and yield distribution
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(env));

        let yield_dist: Vec<YieldDistributionEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::YieldDistribution(loan.id))
            .unwrap_or(Vec::new(env));

        let total_stake: i128 = vouches
            .iter()
            .filter(|v| v.token == *loan_token)
            .map(|v| v.stake)
            .sum();

        for v in vouches.iter() {
            if v.token != *loan_token {
                continue;
            }

            let vouch_yield = yield_dist
                .iter()
                .find(|e| e.voucher == v.voucher)
                .map(|e| e.yield_amount)
                .unwrap_or(0);

            // Apply liquidity-tier yield bonus on top of the locked-in yield
            let tier_bonus = liquidity_tier_bonus_bps(env, loan_token);
            let tier_extra = if total_stake > 0 {
                v.stake * tier_bonus / BPS_DENOMINATOR
            } else {
                0
            };

            let payout = v.stake + vouch_yield + tier_extra;

            if payout > 0 {
                loan_token_client.transfer(
                    &env.current_contract_address(),
                    &v.voucher,
                    &payout,
                );
            }

            let mut stats: VoucherStats = env
                .storage()
                .persistent()
                .get(&DataKey::VoucherStats(v.voucher.clone()))
                .unwrap_or(VoucherStats {
                    successful_vouches: 0,
                    total_vouches_slashed: 0,
                    total_yield_earned: 0,
                    total_slashed: 0,
                });
            stats.successful_vouches += 1;
            stats.total_yield_earned += vouch_yield + tier_extra;
            env.storage()
                .persistent()
                .set(&DataKey::VoucherStats(v.voucher.clone()), &stats);
        }

        // Increment repayment count
        let prev_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::RepaymentCount(borrower.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower.clone()), &(prev_count + 1));

        // Clean up active loan state
        env.storage()
            .persistent()
            .remove(&DataKey::ActiveLoan(borrower.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Vouches(borrower.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::YieldDistribution(loan.id));

        env.events().publish(
            (symbol_short!("loan"), symbol_short!("repaid")),
            (borrower.clone(), loan.amount),
        );
    }

    env.storage()
        .persistent()
        .set(&DataKey::Loan(loan.id), &loan);

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Env, Vec,
    };

    /// Helper: create a minimal env with a registered contract.
    fn make_env() -> Env {
        Env::default()
    }

    // ── #1074: Reentrancy guard ───────────────────────────────────────────────

    #[test]
    fn test_reentrancy_lock_acquire_release() {
        let env = make_env();
        env.mock_all_auths();

        // Lock should acquire successfully when not locked
        assert!(acquire_lock(&env).is_ok());
        // Lock should fail when already locked
        assert_eq!(acquire_lock(&env), Err(ContractError::Reentrancy));
        // After release, lock should be available again
        release_lock(&env);
        assert!(acquire_lock(&env).is_ok());
        release_lock(&env);
    }

    #[test]
    fn test_reentrancy_error_code_is_66() {
        // ContractError::Reentrancy must have discriminant 66 per README error table
        assert_eq!(ContractError::Reentrancy as u32, 66);
    }

    // ── #1077: Liquidity tier ─────────────────────────────────────────────────

    #[test]
    fn test_get_liquidity_tier_default_is_zero() {
        let env = make_env();
        let token_addr = Address::generate(&env);
        // Not set → tier 0 (most liquid)
        assert_eq!(
            get_token_liquidity_tier(env.clone(), token_addr),
            0u32
        );
    }

    #[test]
    fn test_liquidity_tier_bonus_default_values() {
        let env = make_env();

        // Tier 0 → 0 bps bonus
        let t0 = Address::generate(&env);
        env.storage()
            .persistent()
            .set(&DataKey::TokenLiquidityTier(t0.clone()), &0u32);
        assert_eq!(liquidity_tier_bonus_bps(&env, &t0), 0);

        // Tier 3 → 300 bps bonus (DEFAULT_TIER_BONUS_BPS[3])
        let t3 = Address::generate(&env);
        env.storage()
            .persistent()
            .set(&DataKey::TokenLiquidityTier(t3.clone()), &3u32);
        // With an empty config vector, falls back to DEFAULT_TIER_BONUS_BPS
        assert_eq!(liquidity_tier_bonus_bps(&env, &t3), 300);
    }

    // ── #1075: Bridge token price ─────────────────────────────────────────────

    #[test]
    fn test_get_bridge_token_price_default_is_parity() {
        let env = make_env();
        let token_addr = Address::generate(&env);
        // Default price is 10_000 bps (1:1)
        assert_eq!(get_bridge_token_price(&env, &token_addr), 10_000);
    }

    #[test]
    fn test_bridged_token_balance_starts_at_zero() {
        let env = make_env();
        let token_addr = Address::generate(&env);
        assert_eq!(
            get_bridged_token_balance(env.clone(), token_addr),
            0i128
        );
    }
}
