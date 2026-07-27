/// Large Loan Multi-Signature Approval.
///
/// Today any single admin can approve a loan of any size. This module adds a
/// queuing system for loans above a configurable threshold (default: 50,000
/// USDC-equivalent, expressed in stroop-style base units — see
/// `DEFAULT_LARGE_LOAN_THRESHOLD`) that requires 2-of-3 admin signatures
/// before the loan may be disbursed, with a 48-hour expiration window on the
/// proposal so a stalled approval can't linger indefinitely.
use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers;

/// Number of distinct admin signatures required to approve a large loan.
pub const REQUIRED_LARGE_LOAN_SIGNATURES: u32 = 2;

/// How long a large-loan approval proposal remains valid before it must be
/// re-proposed, in seconds (48 hours).
pub const LARGE_LOAN_APPROVAL_EXPIRY_SECS: u64 = 48 * 60 * 60;

/// Default large-loan threshold, in the contract's base token unit (stroops,
/// 7 decimals — consistent with the rest of the contract's stroop
/// convention). 50,000 USDC-equivalent at 7 decimals.
pub const DEFAULT_LARGE_LOAN_THRESHOLD: i128 = 50_000 * 10_000_000;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum LargeLoanDataKey {
    Threshold,
    ApprovalCounter,
    Approval(u64),
    /// borrower -> most recent approval id proposed for them, so callers can
    /// look up a pending approval without tracking the id themselves.
    LatestApprovalForBorrower(Address),
}

/// A pending (or resolved) large-loan multi-signature approval proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LargeLoanApproval {
    pub approval_id: u64,
    pub loan_id: u64,
    pub borrower: Address,
    pub amount: i128,
    pub signers: Vec<Address>,
    pub proposed_at: u64,
    pub expires_at: u64,
    pub executed: bool,
}

/// Admin-governed override of the large-loan threshold. Requires the
/// contract's standard admin approval threshold, same as other config
/// changes.
pub fn set_large_loan_threshold(
    env: Env,
    admin_signers: Vec<Address>,
    threshold: i128,
) -> Result<(), ContractError> {
    helpers::require_admin_approval(&env, &admin_signers);
    if threshold <= 0 {
        return Err(ContractError::InvalidAmount);
    }
    env.storage()
        .instance()
        .set(&LargeLoanDataKey::Threshold, &threshold);
    Ok(())
}

/// Current large-loan threshold, defaulting to `DEFAULT_LARGE_LOAN_THRESHOLD`
/// if never overridden by governance.
pub fn get_large_loan_threshold(env: Env) -> i128 {
    env.storage()
        .instance()
        .get(&LargeLoanDataKey::Threshold)
        .unwrap_or(DEFAULT_LARGE_LOAN_THRESHOLD)
}

/// Queue a large loan for multi-signature approval. Any registered admin can
/// initiate the proposal; the proposer's signature counts as the first of
/// the required `REQUIRED_LARGE_LOAN_SIGNATURES`. Fails if `amount` does not
/// exceed the current large-loan threshold — small loans don't need this
/// flow.
///
/// Task: `propose_large_loan_approval(env, loan_id)`.
pub fn propose_large_loan_approval(
    env: Env,
    proposer: Address,
    loan_id: u64,
    borrower: Address,
    amount: i128,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    if !helpers::is_admin(&env, &proposer) {
        return Err(ContractError::UnauthorizedCaller);
    }

    let threshold = get_large_loan_threshold(env.clone());
    if amount <= threshold {
        return Err(ContractError::BelowLargeLoanThreshold);
    }

    let approval_id: u64 = env
        .storage()
        .persistent()
        .get(&LargeLoanDataKey::ApprovalCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .ok_or(ContractError::ArithmeticError)?;
    env.storage()
        .persistent()
        .set(&LargeLoanDataKey::ApprovalCounter, &approval_id);

    let now = env.ledger().timestamp();
    let mut signers = Vec::new(&env);
    signers.push_back(proposer.clone());

    let approval = LargeLoanApproval {
        approval_id,
        loan_id,
        borrower: borrower.clone(),
        amount,
        signers,
        proposed_at: now,
        expires_at: now + LARGE_LOAN_APPROVAL_EXPIRY_SECS,
        executed: false,
    };
    env.storage()
        .persistent()
        .set(&LargeLoanDataKey::Approval(approval_id), &approval);
    env.storage().persistent().set(
        &LargeLoanDataKey::LatestApprovalForBorrower(borrower.clone()),
        &approval_id,
    );

    env.events().publish(
        (symbol_short!("llapp"), symbol_short!("proposed")),
        (approval_id, loan_id, borrower, amount),
    );

    Ok(approval_id)
}

/// Add a second (or third) admin signature to a pending large-loan approval.
/// Once `REQUIRED_LARGE_LOAN_SIGNATURES` distinct admin signatures are
/// collected within the 48-hour window, the proposal is marked executed and
/// the loan may proceed to disbursement by the normal loan flow. Returns
/// `true` if this signature caused the approval to become executed.
pub fn sign_large_loan_approval(
    env: Env,
    signer: Address,
    approval_id: u64,
) -> Result<bool, ContractError> {
    signer.require_auth();
    if !helpers::is_admin(&env, &signer) {
        return Err(ContractError::UnauthorizedCaller);
    }

    let mut approval: LargeLoanApproval = env
        .storage()
        .persistent()
        .get(&LargeLoanDataKey::Approval(approval_id))
        .ok_or(ContractError::LargeLoanApprovalNotFound)?;

    if approval.executed {
        return Err(ContractError::LargeLoanApprovalAlreadyExecuted);
    }

    let now = env.ledger().timestamp();
    if now > approval.expires_at {
        return Err(ContractError::LargeLoanApprovalExpired);
    }

    if approval.signers.iter().any(|s| s == signer) {
        return Err(ContractError::DuplicateApprovalSigner);
    }

    approval.signers.push_back(signer.clone());

    let newly_executed = approval.signers.len() >= REQUIRED_LARGE_LOAN_SIGNATURES;
    approval.executed = newly_executed;

    env.storage()
        .persistent()
        .set(&LargeLoanDataKey::Approval(approval_id), &approval);

    if newly_executed {
        env.events().publish(
            (symbol_short!("llapp"), symbol_short!("approved")),
            (approval_id, approval.loan_id, approval.signers.len()),
        );
    } else {
        env.events().publish(
            (symbol_short!("llapp"), symbol_short!("signed")),
            (approval_id, signer),
        );
    }

    Ok(newly_executed)
}

/// Whether a given large-loan approval has collected enough non-expired
/// signatures to be considered approved.
pub fn is_large_loan_approved(env: Env, approval_id: u64) -> bool {
    match env
        .storage()
        .persistent()
        .get::<LargeLoanDataKey, LargeLoanApproval>(&LargeLoanDataKey::Approval(approval_id))
    {
        Some(approval) => {
            approval.executed && env.ledger().timestamp() <= approval.expires_at
        }
        None => false,
    }
}

/// Read a large-loan approval record, including its current signer list and
/// timing metadata (proposed_at / expires_at) for off-chain monitoring.
pub fn get_large_loan_approval(env: Env, approval_id: u64) -> Option<LargeLoanApproval> {
    env.storage()
        .persistent()
        .get(&LargeLoanDataKey::Approval(approval_id))
}

/// Look up the most recently proposed large-loan approval for a borrower.
pub fn get_latest_large_loan_approval_for_borrower(
    env: Env,
    borrower: Address,
) -> Option<LargeLoanApproval> {
    let approval_id: u64 = env
        .storage()
        .persistent()
        .get(&LargeLoanDataKey::LatestApprovalForBorrower(borrower))?;
    env.storage()
        .persistent()
        .get(&LargeLoanDataKey::Approval(approval_id))
}
