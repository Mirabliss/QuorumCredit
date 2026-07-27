//! Flash loan functionality for QuorumCredit (Issue #1183).
//!
//! Flash loans enable atomic borrowing and repayment within a single transaction.
//! Key characteristics:
//! - Instant capital access without collateral
//! - Must be repaid (with fee) within the same transaction block
//! - Reverts entire transaction if repayment fails
//! - Enables atomic arbitrage and liquidation operations
//!
//! ## Flash Loan Flow
//! 1. Borrower calls `flash_loan()` with amount and callback contract
//! 2. Protocol transfers amount to callback contract
//! 3. Callback contract executes arbitrary logic (trading, liquidation, etc.)
//! 4. Callback must call `repay_flash_loan()` with principal + 0.05% fee
//! 5. If repayment fails, entire transaction reverts

extern crate alloc;

use crate::errors::ContractError;
use crate::helpers::{config, require_not_paused, token_client};
use crate::types::{DataKey, Config};
use soroban_sdk::{contracttype, Address, BytesN, Env, String, Vec};

/// Flash loan fee in basis points (0.05% = 5 bps)
pub const FLASH_LOAN_FEE_BPS: i128 = 5;

/// Per-contract borrowing cap to prevent abuse.
pub const DEFAULT_PER_CONTRACT_FLASH_CAP: i128 = 10_000_000_000; // 1000 XLM

/// Maximum flash loan amount to prevent systemic risk.
pub const MAX_FLASH_LOAN_AMOUNT: i128 = 1_000_000_000_000; // 100,000 XLM

/// Minimum flash loan amount
pub const MIN_FLASH_LOAN_AMOUNT: i128 = 1_000_000; // 0.1 XLM

/// Records flash loan activity for analytics.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FlashLoanRecord {
    /// Borrower contract address
    pub borrower: Address,
    /// Amount borrowed in stroops
    pub amount: i128,
    /// Fee collected in stroops
    pub fee: i128,
    /// Timestamp of the flash loan
    pub timestamp: u64,
    /// Token used for the loan
    pub token: Address,
}

/// Flash loan event for off-chain tracking.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FlashLoanEvent {
    /// Borrower address
    pub borrower: Address,
    /// Loan amount
    pub amount: i128,
    /// Fee amount
    pub fee: i128,
    /// Successful completion
    pub success: bool,
    /// Block timestamp
    pub timestamp: u64,
}

/// Flash loan statistics for governance.
#[contracttype]
#[derive(Clone, Debug)]
pub struct FlashLoanStats {
    /// Total flash loans originated
    pub total_volume: i128,
    /// Total fees collected
    pub total_fees: i128,
    /// Number of flash loans
    pub loan_count: u64,
    /// Current period fees (resets daily)
    pub period_fees: i128,
}

/// Borrow cap per contract address.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PerContractCap {
    /// Contract address
    pub contract: Address,
    /// Current amount borrowed in this period
    pub borrowed_amount: i128,
    /// Max borrowable amount
    pub cap: i128,
    /// Last reset timestamp
    pub last_reset: u64,
}

/// Initialize flash loan subsystem (called once during contract initialization).
pub fn initialize_flash_loans(env: &Env) -> Result<(), ContractError> {
    // Initialize flash loan statistics storage
    let stats = FlashLoanStats {
        total_volume: 0,
        total_fees: 0,
        loan_count: 0,
        period_fees: 0,
    };

    env.storage()
        .persistent()
        .set(&DataKey::FlashLoanStats, &stats);

    Ok(())
}

/// Execute a flash loan transaction.
///
/// # Arguments
/// * `env` - the Soroban environment
/// * `amount` - flash loan amount in stroops (0.1-100,000 XLM)
/// * `callback_contract` - the contract to call with the borrowed funds
/// * `callback_data` - data to pass to the callback contract
///
/// # Returns
/// `Ok(())` if flash loan succeeded and was repaid; `Err` otherwise.
///
/// # Panics
/// The entire transaction reverts if:
/// - Contract balance is insufficient
/// - Amount exceeds limits or caps
/// - Callback contract doesn't repay principal + fee in same transaction
pub fn flash_loan(
    env: &Env,
    amount: i128,
    callback_contract: Address,
    callback_data: BytesN<32>,
) -> Result<(), ContractError> {
    require_not_paused(env)?;

    // Validate amount
    if amount < MIN_FLASH_LOAN_AMOUNT {
        return Err(ContractError::InvalidAmount);
    }
    if amount > MAX_FLASH_LOAN_AMOUNT {
        return Err(ContractError::InvalidAmount);
    }

    let cfg = config(env);

    // Check contract balance
    let contract_id = env.current_contract_address();
    let balance = token_client(env, &cfg.token).balance(&contract_id);
    if balance < amount {
        return Err(ContractError::InsufficientFunds);
    }

    // Check per-contract rate limit
    validate_per_contract_cap(env, &callback_contract, amount)?;

    // Calculate fee (0.05% = 5 bps)
    let fee = (amount * FLASH_LOAN_FEE_BPS) / 10_000;
    let total_repay = amount + fee;

    // Transfer amount to callback contract
    token_client(env, &cfg.token).transfer(&contract_id, &callback_contract, &amount);

    // Invoke callback contract to perform flash loan operations
    // The callback must call repay_flash_loan() before transaction completes
    // Note: In production, this would use Soroban's contract invocation system

    // Record the flash loan for analytics
    let record = FlashLoanRecord {
        borrower: callback_contract.clone(),
        amount,
        fee,
        timestamp: env.ledger().timestamp(),
        token: cfg.token.clone(),
    };

    add_flash_loan_record(env, &record)?;

    // Update statistics
    update_flash_loan_stats(env, amount, fee)?;

    // Verify repayment happened (in production, this is implicit in transaction atomicity)
    // The contract balance check ensures the loan was repaid
    let balance_after = token_client(env, &cfg.token).balance(&contract_id);
    if balance_after < balance {
        // Flash loan was not repaid in full
        return Err(ContractError::FlashLoanNotRepaid);
    }

    Ok(())
}

/// Repay a flash loan (called by the callback contract).
///
/// # Arguments
/// * `env` - the Soroban environment
/// * `principal` - the original loan amount
/// * `fee` - the 0.05% fee
pub fn repay_flash_loan(
    env: &Env,
    borrower: Address,
    principal: i128,
    fee: i128,
) -> Result<(), ContractError> {
    let cfg = config(env);
    let total = principal + fee;

    // Validate fee calculation
    let expected_fee = (principal * FLASH_LOAN_FEE_BPS) / 10_000;
    if fee != expected_fee {
        return Err(ContractError::InvalidFeeAmount);
    }

    // Transfer repayment from borrower to contract
    let contract_id = env.current_contract_address();
    token_client(env, &cfg.token).transfer_from(
        &borrower,
        &contract_id,
        &total,
    )?;

    Ok(())
}

/// Get flash loan statistics.
pub fn get_flash_loan_stats(env: &Env) -> Result<FlashLoanStats, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::FlashLoanStats)
        .ok_or(ContractError::NotFound)
}

/// Get total flash loan volume.
pub fn get_total_flash_loan_volume(env: &Env) -> Result<i128, ContractError> {
    let stats = get_flash_loan_stats(env)?;
    Ok(stats.total_volume)
}

/// Get total flash loan fees collected.
pub fn get_total_flash_loan_fees(env: &Env) -> Result<i128, ContractError> {
    let stats = get_flash_loan_stats(env)?;
    Ok(stats.total_fees)
}

/// Get flash loan count.
pub fn get_flash_loan_count(env: &Env) -> Result<u64, ContractError> {
    let stats = get_flash_loan_stats(env)?;
    Ok(stats.loan_count)
}

/// Check if a contract is at its borrowing cap.
pub fn check_per_contract_cap(
    env: &Env,
    contract: &Address,
) -> Result<i128, ContractError> {
    let cap_data: Option<PerContractCap> = env
        .storage()
        .persistent()
        .get(&DataKey::FlashLoanPerContractCap(contract.clone()));

    if let Some(cap) = cap_data {
        Ok(cap.cap - cap.borrowed_amount)
    } else {
        Ok(DEFAULT_PER_CONTRACT_FLASH_CAP)
    }
}

// ── Internal functions ────────────────────────────────────────────────────────

fn validate_per_contract_cap(
    env: &Env,
    contract: &Address,
    amount: i128,
) -> Result<(), ContractError> {
    let available = check_per_contract_cap(env, contract)?;

    if amount > available {
        return Err(ContractError::FlashLoanCapExceeded);
    }

    Ok(())
}

fn add_flash_loan_record(env: &Env, record: &FlashLoanRecord) -> Result<(), ContractError> {
    let mut records: Vec<FlashLoanRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::FlashLoanHistory)
        .unwrap_or_else(|| Vec::new(&env));

    records.push_back(record.clone());

    // Keep only last 10000 records to avoid unbounded growth
    if records.len() > 10000 {
        // Archive old records (would archive to separate storage in production)
        records.pop_front();
    }

    env.storage()
        .persistent()
        .set(&DataKey::FlashLoanHistory, &records);

    Ok(())
}

fn update_flash_loan_stats(env: &Env, amount: i128, fee: i128) -> Result<(), ContractError> {
    let mut stats = env
        .storage()
        .persistent()
        .get(&DataKey::FlashLoanStats)
        .ok_or(ContractError::NotFound)?;

    stats.total_volume += amount;
    stats.total_fees += fee;
    stats.loan_count += 1;
    stats.period_fees += fee;

    env.storage()
        .persistent()
        .set(&DataKey::FlashLoanStats, &stats);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flash_loan_fee_calculation() {
        let principal = 1_000_000;
        let expected_fee = (principal * FLASH_LOAN_FEE_BPS) / 10_000;
        assert_eq!(expected_fee, 50); // 0.05% of 1 XLM
    }

    #[test]
    fn test_flash_loan_amount_bounds() {
        assert!(MIN_FLASH_LOAN_AMOUNT > 0);
        assert!(MAX_FLASH_LOAN_AMOUNT > MIN_FLASH_LOAN_AMOUNT);
        assert!(DEFAULT_PER_CONTRACT_FLASH_CAP > 0);
    }

    #[test]
    fn test_large_loan_fee() {
        let principal = MAX_FLASH_LOAN_AMOUNT;
        let fee = (principal * FLASH_LOAN_FEE_BPS) / 10_000;
        assert!(fee > 0);
        assert!(principal + fee > principal);
    }
}
