//! Issue #1169: Conditional vouch release on loan performance milestones.
//!
//! Instead of a voucher's stake being locked in full for the life of the
//! loan, it is released in 25% increments as the loan hits performance
//! milestones (issued, first payment, half repaid, completed). Reaching a
//! milestone in the first half of the loan's term earns a small early-release
//! bonus, funded out of the voucher's own remaining stake so no external
//! funds are required.

use soroban_sdk::{symbol_short, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers::{require_allowed_token, require_not_thawing};
use crate::types::{DataKey, LoanMilestone, LoanRecord, LoanStatus, VouchRecord};
use crate::vouch::invalidate_weighted_stake_cache;

const ALL_MILESTONES: [LoanMilestone; 4] = [
    LoanMilestone::Issued,
    LoanMilestone::FirstPaymentMade,
    LoanMilestone::HalfRepaid,
    LoanMilestone::Completed,
];

/// Early-release bonus, in basis points of the milestone's release amount,
/// paid (out of the voucher's own remaining stake) when the milestone is hit
/// before the halfway point of the loan's [disbursement, deadline] window.
const EARLY_BONUS_BPS: u32 = 500; // 5%

fn milestone_reached(loan: &LoanRecord, milestone: LoanMilestone) -> bool {
    match milestone {
        LoanMilestone::Issued => true,
        LoanMilestone::FirstPaymentMade => loan.amount_repaid > 0,
        LoanMilestone::HalfRepaid => loan.amount > 0 && loan.amount_repaid >= loan.amount / 2,
        LoanMilestone::Completed => loan.repaid || loan.status == LoanStatus::Repaid,
    }
}

fn is_early(env: &Env, loan: &LoanRecord) -> bool {
    if loan.deadline <= loan.disbursement_timestamp {
        return false;
    }
    let now = env.ledger().timestamp();
    let midpoint = loan.disbursement_timestamp + (loan.deadline - loan.disbursement_timestamp) / 2;
    now <= midpoint
}

/// Total amount already released to `voucher` for `loan_id` across all
/// milestones so far.
fn total_released(env: &Env, loan_id: u64, voucher: &Address) -> i128 {
    let mut total: i128 = 0;
    for milestone in ALL_MILESTONES.iter() {
        if let Some(amount) = env.storage().persistent().get::<DataKey, i128>(
            &DataKey::VouchMilestoneRelease(loan_id, voucher.clone(), milestone.index()),
        ) {
            total += amount;
        }
    }
    total
}

/// Release the portion of `voucher`'s stake unlocked by reaching `milestone`
/// on `loan_id`. Releases 25% of the voucher's original stake per milestone,
/// plus an early-completion bonus drawn from their own remaining stake.
pub fn release_vouch_at_milestone(
    env: Env,
    loan_id: u64,
    voucher: Address,
    milestone: LoanMilestone,
) -> Result<i128, ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    let loan: LoanRecord = env
        .storage()
        .persistent()
        .get(&DataKey::Loan(loan_id))
        .ok_or(ContractError::NoActiveLoan)?;

    if !milestone_reached(&loan, milestone) {
        return Err(ContractError::MilestoneNotReached);
    }

    if env.storage().persistent().has(&DataKey::VouchMilestoneRelease(
        loan_id,
        voucher.clone(),
        milestone.index(),
    )) {
        return Err(ContractError::MilestoneAlreadyReleased);
    }

    let borrower = loan.borrower.clone();
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;
    let vouch_rec = vouches.get(idx).unwrap();
    let current_stake = vouch_rec.stake;

    let original_stake = current_stake
        .checked_add(total_released(&env, loan_id, &voucher))
        .ok_or(ContractError::StakeOverflow)?;

    let mut release_amount = (original_stake * milestone.release_bps() as i128) / 10_000;
    if is_early(&env, &loan) {
        let bonus = (release_amount * EARLY_BONUS_BPS as i128) / 10_000;
        release_amount = release_amount
            .checked_add(bonus)
            .ok_or(ContractError::StakeOverflow)?;
    }
    if release_amount > current_stake {
        release_amount = current_stake;
    }
    if release_amount <= 0 {
        return Ok(0);
    }

    let token_client = require_allowed_token(&env, &vouch_rec.token)?;
    let mut vouches_mut = vouches;
    let new_stake = current_stake - release_amount;
    if new_stake == 0 {
        vouches_mut.remove(idx);
    } else {
        let mut updated = vouch_rec.clone();
        updated.stake = new_stake;
        vouches_mut.set(idx, updated);
    }
    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);
    invalidate_weighted_stake_cache(&env, &borrower, &vouch_rec.token);

    token_client.transfer(&env.current_contract_address(), &voucher, &release_amount);

    env.storage().persistent().set(
        &DataKey::VouchMilestoneRelease(loan_id, voucher.clone(), milestone.index()),
        &release_amount,
    );
    if !env
        .storage()
        .persistent()
        .has(&DataKey::MilestoneAchieved(loan_id, milestone.index()))
    {
        env.storage().persistent().set(
            &DataKey::MilestoneAchieved(loan_id, milestone.index()),
            &env.ledger().timestamp(),
        );
    }

    env.events().publish(
        (symbol_short!("milestone"), symbol_short!("released")),
        (loan_id, voucher, release_amount),
    );

    Ok(release_amount)
}

pub fn get_milestone_achieved(env: Env, loan_id: u64, milestone: LoanMilestone) -> Option<u64> {
    env.storage()
        .persistent()
        .get(&DataKey::MilestoneAchieved(loan_id, milestone.index()))
}

pub fn get_milestone_release(
    env: Env,
    loan_id: u64,
    voucher: Address,
    milestone: LoanMilestone,
) -> Option<i128> {
    env.storage().persistent().get(&DataKey::VouchMilestoneRelease(
        loan_id,
        voucher,
        milestone.index(),
    ))
}
