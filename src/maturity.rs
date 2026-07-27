/// Issue #1177: Vouch Maturity-Based Interest Adjustment
///
/// This module implements tenure-based interest bonus system for vouchers.
/// Vouchers earn additional interest (0.1% per 6 months, capped at 1%) based on
/// continuous participation. Special loyalty bonus (0.5% additional) for 2+ years.

use soroban_sdk::{Address, Env};
use crate::types::{
    VouchMaturityRecord, DataKey, ContractError,
    MATURITY_BONUS_INCREMENT_BPS, MATURITY_BONUS_PERIOD_SECS, MATURITY_BONUS_MAX_BPS,
    LOYALTY_BONUS_THRESHOLD_SECS, LOYALTY_BONUS_BPS, BPS_DENOMINATOR,
};

/// Initialize or update maturity record for a new vouch.
///
/// Called when a vouch is first created to establish the maturity tracking.
pub fn initialize_vouch_maturity(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
) -> Result<(), ContractError> {
    let current_timestamp = env.ledger().timestamp();

    let maturity_record = VouchMaturityRecord {
        voucher: voucher.clone(),
        borrower: borrower.clone(),
        token: token.clone(),
        vouch_created_at: current_timestamp,
        last_maturity_update: current_timestamp,
        maturity_bonus_bps: 0,
        loyalty_bonus_eligible: false,
    };

    let key = DataKey::VouchMaturity(borrower.clone(), voucher.clone(), token.clone());
    env.storage()
        .persistent()
        .set::<DataKey, VouchMaturityRecord>(&key, &maturity_record);

    Ok(())
}

/// Calculate and update the maturity bonus for a vouch.
///
/// Returns the current maturity bonus in basis points.
pub fn update_maturity_bonus(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
) -> Result<i128, ContractError> {
    let key = DataKey::VouchMaturity(borrower.clone(), voucher.clone(), token.clone());

    let mut maturity = env
        .storage()
        .persistent()
        .get::<DataKey, VouchMaturityRecord>(&key)
        .ok_or(ContractError::InvalidInput)?;

    let current_timestamp = env.ledger().timestamp();
    let time_elapsed = current_timestamp
        .checked_sub(maturity.vouch_created_at)
        .ok_or(ContractError::ArithmeticError)?;

    // Calculate maturity bonus: 0.1% per 6 months, capped at 1%
    let periods_elapsed = time_elapsed / MATURITY_BONUS_PERIOD_SECS;
    let mut new_bonus_bps = (periods_elapsed as i128)
        .checked_mul(MATURITY_BONUS_INCREMENT_BPS)
        .ok_or(ContractError::ArithmeticError)?;

    // Cap at maximum bonus
    if new_bonus_bps > MATURITY_BONUS_MAX_BPS {
        new_bonus_bps = MATURITY_BONUS_MAX_BPS;
    }

    // Check for loyalty bonus eligibility (2+ years)
    if time_elapsed >= LOYALTY_BONUS_THRESHOLD_SECS {
        maturity.loyalty_bonus_eligible = true;
    }

    maturity.maturity_bonus_bps = new_bonus_bps;
    maturity.last_maturity_update = current_timestamp;

    env.storage()
        .persistent()
        .set::<DataKey, VouchMaturityRecord>(&key, &maturity);

    Ok(new_bonus_bps)
}

/// Get the total interest bonus for a vouch (maturity + loyalty).
///
/// Returns the combined bonus in basis points.
pub fn get_total_interest_bonus(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
) -> Result<i128, ContractError> {
    // First update the maturity bonus
    let maturity_bonus = update_maturity_bonus(env, voucher, borrower, token)?;

    let key = DataKey::VouchMaturity(borrower.clone(), voucher.clone(), token.clone());
    let maturity = env
        .storage()
        .persistent()
        .get::<DataKey, VouchMaturityRecord>(&key)
        .ok_or(ContractError::InvalidInput)?;

    let mut total_bonus = maturity_bonus;

    // Add loyalty bonus if eligible
    if maturity.loyalty_bonus_eligible {
        total_bonus = total_bonus
            .checked_add(LOYALTY_BONUS_BPS)
            .ok_or(ContractError::ArithmeticError)?;
    }

    Ok(total_bonus)
}

/// Apply maturity-based interest adjustment to a yield amount.
///
/// Takes the base yield and applies the maturity/loyalty multiplier.
/// Returns the adjusted yield in stroops.
pub fn apply_maturity_interest_adjustment(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
    base_yield: i128,
) -> Result<i128, ContractError> {
    let bonus_bps = get_total_interest_bonus(env, voucher, borrower, token)?;

    if bonus_bps == 0 {
        return Ok(base_yield);
    }

    // Calculate bonus: base_yield * (bonus_bps / BPS_DENOMINATOR)
    let bonus_amount = base_yield
        .checked_mul(bonus_bps)
        .ok_or(ContractError::ArithmeticError)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::ArithmeticError)?;

    let adjusted_yield = base_yield
        .checked_add(bonus_amount)
        .ok_or(ContractError::ArithmeticError)?;

    Ok(adjusted_yield)
}

/// Get the maturity record for a vouch.
pub fn get_vouch_maturity(
    env: Env,
    voucher: Address,
    borrower: Address,
    token: Address,
) -> Result<VouchMaturityRecord, ContractError> {
    let key = DataKey::VouchMaturity(borrower, voucher, token);
    env.storage()
        .persistent()
        .get::<DataKey, VouchMaturityRecord>(&key)
        .ok_or(ContractError::InvalidInput)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_vouch_maturity() {
        // Tests will be added in the test suite
    }

    #[test]
    fn test_update_maturity_bonus() {
        // Tests will be added in the test suite
    }

    #[test]
    fn test_get_total_interest_bonus() {
        // Tests will be added in the test suite
    }
}
