//! Issue #1168: Loan repayment automation via recurring transfers.
//!
//! The borrower opts into a repayment schedule (amount, frequency, start
//! date). Because Soroban authorization is per-transaction, true "pull"
//! automation relies on the borrower granting this contract a standard
//! SEP-41 token allowance (`token.approve(borrower, <this contract>, ...)`)
//! covering the scheduled amount; an off-chain keeper (see
//! `server/src/recurring`) then calls `execute_recurring_payment` once each
//! period is due. The contract call itself is atomic (it either fully
//! succeeds or the transaction reverts), so retry-with-backoff and borrower
//! notification on repeated failure are handled by that off-chain layer,
//! which reports failed attempts back via `record_recurring_payment_failure`.

use soroban_sdk::{symbol_short, Address, Env};

use crate::errors::ContractError;
use crate::helpers::{get_active_loan_record, require_allowed_token, require_not_thawing};
use crate::types::DataKey;
pub use crate::types::RecurringPaymentConfig;

/// Set up (or replace, after termination) a recurring repayment schedule for
/// the caller's active loan.
pub fn setup_recurring_payment(
    env: Env,
    borrower: Address,
    token: Address,
    amount: i128,
    frequency_secs: u64,
    start_date: u64,
) -> Result<(), ContractError> {
    borrower.require_auth();
    require_not_thawing(&env)?;

    if amount <= 0 || frequency_secs == 0 {
        return Err(ContractError::InvalidAmount);
    }
    require_allowed_token(&env, &token)?;
    // Ensures there is a loan to automate repayment against.
    get_active_loan_record(&env, &borrower)?;

    if let Some(existing) = env
        .storage()
        .persistent()
        .get::<DataKey, RecurringPaymentConfig>(&DataKey::RecurringPayment(borrower.clone()))
    {
        if existing.active {
            return Err(ContractError::RecurringPaymentExists);
        }
    }

    let config = RecurringPaymentConfig {
        borrower: borrower.clone(),
        token,
        amount,
        frequency_secs,
        start_date,
        next_payment_due: start_date,
        active: true,
        success_count: 0,
        failure_count: 0,
        retry_count: 0,
    };
    env.storage()
        .persistent()
        .set(&DataKey::RecurringPayment(borrower.clone()), &config);

    env.events().publish(
        (symbol_short!("recurpay"), symbol_short!("setup")),
        (borrower, amount, frequency_secs),
    );

    Ok(())
}

/// Execute a due recurring payment, pulling `amount` from the borrower via
/// their standing token allowance and applying it as a loan repayment.
/// Permissionless by design so an off-chain keeper can trigger it; the
/// borrower already authorized the pull when they approved the allowance.
pub fn execute_recurring_payment(env: Env, borrower: Address) -> Result<i128, ContractError> {
    require_not_thawing(&env)?;

    let mut config: RecurringPaymentConfig = env
        .storage()
        .persistent()
        .get(&DataKey::RecurringPayment(borrower.clone()))
        .ok_or(ContractError::RecurringPaymentNotFound)?;

    if !config.active {
        return Err(ContractError::RecurringPaymentInactive);
    }
    if env.ledger().timestamp() < config.next_payment_due {
        return Err(ContractError::RecurringPaymentNotDue);
    }

    let mut loan = get_active_loan_record(&env, &borrower)?;
    let token_client = require_allowed_token(&env, &config.token)?;

    // Reverts (and rolls back this whole call) if the borrower hasn't
    // approved a sufficient allowance or lacks the balance; the off-chain
    // keeper treats a reverted submission as a failed attempt and reports it
    // via `record_recurring_payment_failure`.
    token_client.transfer_from(
        &env.current_contract_address(),
        &borrower,
        &env.current_contract_address(),
        &config.amount,
    );

    loan.amount_repaid = loan
        .amount_repaid
        .checked_add(config.amount)
        .ok_or(ContractError::ArithmeticError)?;
    env.storage().persistent().set(&DataKey::Loan(loan.id), &loan);

    config.next_payment_due = config
        .next_payment_due
        .checked_add(config.frequency_secs)
        .ok_or(ContractError::ArithmeticError)?;
    config.success_count = config.success_count.saturating_add(1);
    config.retry_count = 0;
    env.storage()
        .persistent()
        .set(&DataKey::RecurringPayment(borrower.clone()), &config);

    env.events().publish(
        (symbol_short!("recurpay"), symbol_short!("executed")),
        (borrower, config.amount),
    );

    Ok(config.amount)
}

/// Record that an off-chain execution attempt failed (transfer reverted),
/// so retry counts and success-rate tracking reflect reality. Called by the
/// automation keeper, not the borrower.
pub fn record_recurring_payment_failure(env: Env, borrower: Address) -> Result<u32, ContractError> {
    let mut config: RecurringPaymentConfig = env
        .storage()
        .persistent()
        .get(&DataKey::RecurringPayment(borrower.clone()))
        .ok_or(ContractError::RecurringPaymentNotFound)?;

    config.retry_count = config.retry_count.saturating_add(1);
    config.failure_count = config.failure_count.saturating_add(1);
    env.storage()
        .persistent()
        .set(&DataKey::RecurringPayment(borrower.clone()), &config);

    env.events().publish(
        (symbol_short!("recurpay"), symbol_short!("failed")),
        (borrower, config.retry_count),
    );

    Ok(config.retry_count)
}

/// Early termination of a recurring payment schedule by the borrower.
pub fn terminate_recurring_payment(env: Env, borrower: Address) -> Result<(), ContractError> {
    borrower.require_auth();

    let mut config: RecurringPaymentConfig = env
        .storage()
        .persistent()
        .get(&DataKey::RecurringPayment(borrower.clone()))
        .ok_or(ContractError::RecurringPaymentNotFound)?;

    config.active = false;
    env.storage()
        .persistent()
        .set(&DataKey::RecurringPayment(borrower.clone()), &config);

    env.events().publish(
        (symbol_short!("recurpay"), symbol_short!("terminate")),
        (borrower,),
    );

    Ok(())
}

pub fn get_recurring_payment(env: Env, borrower: Address) -> Option<RecurringPaymentConfig> {
    env.storage()
        .persistent()
        .get(&DataKey::RecurringPayment(borrower))
}

/// Success rate in basis points (10_000 = 100%) across all execution attempts.
pub fn recurring_payment_success_rate(env: Env, borrower: Address) -> u32 {
    let config: Option<RecurringPaymentConfig> = env
        .storage()
        .persistent()
        .get(&DataKey::RecurringPayment(borrower));
    match config {
        Some(c) => {
            let attempts = c.success_count + c.failure_count;
            if attempts == 0 {
                0
            } else {
                (c.success_count * 10_000) / attempts
            }
        }
        None => 0,
    }
}
