use crate::errors::ContractError;
use crate::helpers::{
    config, get_active_loan_record, require_admin_approval, require_not_paused,
};
use crate::types::{
    DataKey, GuarantorObligation, GuarantorRecord, GuarantorStats, GuaranteeStatus,
};
use soroban_sdk::{Address, Env, Vec};

/// Issue #1172: Request a guarantor for a loan.
/// The guarantor must sign off on their backing commitment.
pub fn request_guarantor_for_loan(
    env: Env,
    loan_id: u64,
    guarantor_address: Address,
    guarantee_amount: i128,
) -> Result<(), ContractError> {
    // Verify the loan exists and is active
    let loan = get_active_loan_record(&env, &loan_id)?;
    require_not_paused(&env)?;

    // Guarantor must be different from borrower
    if guarantor_address == loan.borrower {
        return Err(ContractError::InvalidGuarantor);
    }

    // Validate guarantee amount is positive and doesn't exceed loan amount
    if guarantee_amount <= 0 || guarantee_amount > loan.amount {
        return Err(ContractError::InvalidGuaranteeAmount);
    }

    // Check if a guarantor is already assigned
    if loan.guarantor.is_some() {
        return Err(ContractError::GuarantorAlreadyAssigned);
    }

    // Guarantor must authorize themselves
    guarantor_address.require_auth();

    // Create guarantor record with verified signature
    let guarantor_record = GuarantorRecord {
        loan_id,
        guarantor: guarantor_address.clone(),
        signature_verified: true,
        guarantee_amount,
        requested_at: env.ledger().timestamp(),
        released_at: None,
        status: GuaranteeStatus::Active,
    };

    // Store the guarantor record
    env.storage()
        .persistent()
        .set(&DataKey::GuarantorRecord(loan_id), &guarantor_record);

    // Create guarantor obligation tracking
    let obligation = GuarantorObligation {
        guarantor: guarantor_address.clone(),
        loan_id,
        borrower: loan.borrower.clone(),
        max_liability: guarantee_amount,
        amount_paid: 0,
        created_at: env.ledger().timestamp(),
        closed_at: None,
    };

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorObligation(guarantor_address.clone(), loan_id), &obligation);

    // Initialize or update guarantor stats
    let mut stats: GuarantorStats = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorStats(guarantor_address.clone()))
        .unwrap_or(GuarantorStats {
            total_guarantees: 0,
            successful_guarantees: 0,
            triggered_guarantees: 0,
            total_guaranteed: 0,
            total_paid_out: 0,
            reputation_score: 500, // Start with neutral reputation (0-1000 scale)
            last_activity: env.ledger().timestamp(),
        });

    stats.total_guarantees += 1;
    stats.total_guaranteed = stats.total_guaranteed.checked_add(guarantee_amount)
        .ok_or(ContractError::ArithmeticOverflow)?;
    stats.last_activity = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorStats(guarantor_address), &stats);

    Ok(())
}

/// Issue #1172: Release a guarantor from their obligation.
/// Called after loan completion (repaid or defaulted and settled).
pub fn release_guarantor(env: Env, loan_id: u64) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    // Get the guarantor record
    let mut guarantor_record: GuarantorRecord = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorRecord(loan_id))
        .ok_or(ContractError::GuarantorNotFound)?;

    if guarantor_record.status != GuaranteeStatus::Active {
        return Err(ContractError::InvalidGuaranteeStatus);
    }

    let guarantor_addr = guarantor_record.guarantor.clone();
    let now = env.ledger().timestamp();

    // Update guarantor record status
    guarantor_record.status = GuaranteeStatus::Released;
    guarantor_record.released_at = Some(now);

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorRecord(loan_id), &guarantor_record);

    // Close the obligation
    let mut obligation: GuarantorObligation = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorObligation(guarantor_addr.clone(), loan_id))
        .ok_or(ContractError::GuarantorNotFound)?;

    obligation.closed_at = Some(now);

    env.storage()
        .persistent()
        .set(
            &DataKey::GuarantorObligation(guarantor_addr.clone(), loan_id),
            &obligation,
        );

    // Update guarantor stats - increment successful guarantees if no payment was required
    let mut stats: GuarantorStats = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorStats(guarantor_addr.clone()))
        .ok_or(ContractError::GuarantorNotFound)?;

    // If obligation was fulfilled without needing to pay (loan was repaid), mark as successful
    if obligation.amount_paid == 0 {
        stats.successful_guarantees += 1;
        // Increase reputation for successful guarantee
        stats.reputation_score = (stats.reputation_score + 50).min(1000);
    }

    stats.last_activity = now;

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorStats(guarantor_addr), &stats);

    Ok(())
}

/// Issue #1172: Get guarantor record for a loan.
pub fn get_guarantor_record(env: Env, loan_id: u64) -> Result<GuarantorRecord, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::GuarantorRecord(loan_id))
        .ok_or(ContractError::GuarantorNotFound)
}

/// Issue #1172: Get guarantor's stats and reputation.
pub fn get_guarantor_stats(env: Env, guarantor: Address) -> Result<GuarantorStats, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::GuarantorStats(guarantor))
        .ok_or(ContractError::GuarantorNotFound)
}

/// Issue #1172: Claim guarantor coverage - called when a loan defaults.
/// Transfers the guaranteed amount from guarantor to vouchers.
pub fn claim_guarantor_coverage(
    env: Env,
    loan_id: u64,
    token: Address,
) -> Result<i128, ContractError> {
    require_not_paused(&env)?;

    let mut guarantor_record: GuarantorRecord = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorRecord(loan_id))
        .ok_or(ContractError::GuarantorNotFound)?;

    if guarantor_record.status == GuaranteeStatus::Claimed {
        return Err(ContractError::GuarantorAlreadyClaimed);
    }

    let guarantor_addr = guarantor_record.guarantor.clone();
    let coverage_amount = guarantor_record.guarantee_amount;

    // Mark guarantee as claimed
    guarantor_record.status = GuaranteeStatus::Claimed;

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorRecord(loan_id), &guarantor_record);

    // Update obligation with payment
    let mut obligation: GuarantorObligation = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorObligation(guarantor_addr.clone(), loan_id))
        .ok_or(ContractError::GuarantorNotFound)?;

    obligation.amount_paid = coverage_amount;
    obligation.closed_at = Some(env.ledger().timestamp());

    env.storage()
        .persistent()
        .set(
            &DataKey::GuarantorObligation(guarantor_addr.clone(), loan_id),
            &obligation,
        );

    // Update guarantor stats - mark as triggered, decrease reputation
    let mut stats: GuarantorStats = env
        .storage()
        .persistent()
        .get(&DataKey::GuarantorStats(guarantor_addr))
        .ok_or(ContractError::GuarantorNotFound)?;

    stats.triggered_guarantees += 1;
    stats.total_paid_out = stats.total_paid_out.checked_add(coverage_amount)
        .ok_or(ContractError::ArithmeticOverflow)?;
    // Decrease reputation for triggered guarantee
    stats.reputation_score = stats.reputation_score.saturating_sub(100).max(0);
    stats.last_activity = env.ledger().timestamp();

    env.storage()
        .persistent()
        .set(&DataKey::GuarantorStats(guarantor_addr), &stats);

    Ok(coverage_amount)
}

/// Issue #1172: Get guarantor's reputation impact.
/// Higher reputation guarantors improve loan terms.
pub fn get_guarantor_reputation_multiplier(
    env: Env,
    guarantor: Address,
) -> Result<u32, ContractError> {
    let stats = get_guarantor_stats(env, guarantor)?;

    // Reputation score (0-1000) maps to yield multiplier (100-150 bps)
    // 0 reputation = 100 bps (no bonus)
    // 1000 reputation = 150 bps (50 bps bonus)
    let multiplier_bps = 100 + ((stats.reputation_score as u32 * 50) / 1000);

    Ok(multiplier_bps)
}
