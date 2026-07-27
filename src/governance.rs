use crate::errors::ContractError;
use crate::helpers::{
    config, get_active_loan_record, get_latest_loan_record, has_active_loan,
    is_zero_address, require_admin_approval, require_governance_participant, require_not_paused,
};
use crate::types::{
    AdminRemovalProposal, AppealStatus, DataKey, LoanStatus, PendingSlashRecord,
    SlashAppealRecord, SlashEscrowAppealRecord, SlashEscrow,
    SlashingReportRecord, SlashRecord, SlashThresholdProposal, SlashVoteRecord,
    TimelockAction, TimelockProposal,
    VouchRecord, BPS_DENOMINATOR, APPEAL_OVERRIDE_QUORUM_BPS, MONTHLY_PERIOD_SECS,
    SLASH_APPEAL_PERIOD, VoteSlashResult, DEFAULT_SLASH_VOTE_QUORUM_BPS,
};
use soroban_sdk::{panic_with_error, symbol_short, Address, Env, String, Vec};

/// Issue #1069: Resolve the effective voter for a voucher, following delegation chain.
/// Returns the final delegate if one exists, otherwise the original voucher.
pub fn resolve_vote_delegation(env: &Env, voucher: &Address) -> Address {
    let mut current = voucher.clone();
    let mut visited: Vec<Address> = Vec::new(env);
    visited.push_back(current.clone());
    
    // Follow delegation chain (max 10 hops to prevent infinite loops)
    for _ in 0..10 {
        if let Some(delegate) = env
            .storage()
            .persistent()
            .get(&DataKey::VoteDelegation(current.clone()))
        {
            // Check for circular delegation
            if visited.iter().any(|a| a == delegate) {
                break;
            }
            current = delegate;
            visited.push_back(current.clone());
        } else {
            break;
        }
    }
    
    current
}

/// Issue #1069: Get the effective stake for a voucher, considering delegation.
/// If the voucher has delegated their vote, returns 0 (delegate votes with their own stake).
/// Otherwise returns the voucher's actual stake.
pub fn get_effective_vote_stake(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
) -> Result<i128, ContractError> {
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));
    
    let vouch_rec = vouches
        .iter()
        .find(|v| v.voucher == *voucher)
        .ok_or(ContractError::VoucherNotFound)?;
    
    // Check if this voucher has delegated their vote
    let effective_voter = resolve_vote_delegation(env, voucher);
    
    // If the effective voter is different from this voucher, this voucher's stake
    // is counted under the delegate's vote, not their own
    if effective_voter != *voucher {
        Ok(0)
    } else {
        Ok(vouch_rec.stake)
    }
}

/// Cast a governance vote on whether `borrower` should be slashed.
///
/// - Only active vouchers (those with a stake in `Vouches(borrower)`) may vote.
/// - Votes are weighted by the voucher's current stake.
/// - When `approve_stake * BPS_DENOMINATOR / total_stake >= quorum_bps`, slash is auto-executed.
/// Issue #1069: Delegate vote to another address for governance votes.
/// The delegate can vote on behalf of the voucher with the voucher's stake.
pub fn delegate_vote(
    env: Env,
    voucher: Address,
    delegate: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_paused(&env)?;

    if voucher == delegate {
        return Err(ContractError::InvalidStateTransition);
    }

    // Check for circular delegation
    let mut visited: Vec<Address> = Vec::new(&env);
    visited.push_back(voucher.clone());
    
    let mut current = delegate.clone();
    for _ in 0..10 {
        if current == voucher {
            return Err(ContractError::CircularDelegation);
        }
        
        if let Some(next_delegate) = env
            .storage()
            .persistent()
            .get(&DataKey::VoteDelegation(current.clone()))
        {
            if visited.iter().any(|a| a == next_delegate) {
                break;
            }
            visited.push_back(next_delegate.clone());
            current = next_delegate;
        } else {
            break;
        }
    }

    // Store the delegation
    env.storage()
        .persistent()
        .set(&DataKey::VoteDelegation(voucher.clone()), &delegate);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("delegated")),
        (voucher, delegate),
    );

    Ok(())
}

/// Issue #1069: Revoke a previously delegated vote.
pub fn revoke_vote_delegation(
    env: Env,
    voucher: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_paused(&env)?;

    // Check if delegation exists
    if env
        .storage()
        .persistent()
        .get::<DataKey, Address>(&DataKey::VoteDelegation(voucher.clone()))
        .is_none()
    {
        return Err(ContractError::DelegationNotFound);
    }

    // Remove the delegation
    env.storage()
        .persistent()
        .remove(&DataKey::VoteDelegation(voucher.clone()));

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("revoked")),
        (voucher,),
    );

    Ok(())
}

/// Issue #1069: Get the delegate for a voucher, if any.
pub fn get_vote_delegate(env: Env, voucher: Address) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::VoteDelegation(voucher))
}

pub fn vote_slash(
    env: Env,
    voter: Address,
    borrower: Address,
    approve: bool,
) -> Result<VoteSlashResult, ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;

    // Check for an already-executed vote first (before the active-loan guard).
    let existing_vote: Option<SlashVoteRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::SlashVote(borrower.clone()));
    if let Some(ref v) = existing_vote {
        if v.executed {
            return Err(ContractError::SlashAlreadyExecuted);
        }
    }

    let cfg = config(&env);
    let now = env.ledger().timestamp();

    // Enforce slash cooldown (time since last *executed* slash).
    if cfg.slash_cooldown_seconds > 0 {
        if let Some(last) = env
            .storage()
            .persistent()
            .get(&DataKey::LastSlashedAt(borrower.clone()))
        {
            if now.saturating_sub(last) < cfg.slash_cooldown_seconds {
                return Err(ContractError::SlashCooldownActive);
            }
        }
    }

    // If the borrower's latest loan is already repaid, panic with a clear message.
    if let Some(latest) = get_latest_loan_record(&env, &borrower) {
        assert!(
            latest.status != LoanStatus::Repaid,
            "loan already repaid"
        );
    }

    // Borrower must have an active loan to be slashable.
    let loan = get_active_loan_record(&env, &borrower)?;
    if loan.status != crate::types::LoanStatus::Active {
        return Err(ContractError::NoActiveLoan);
    }

    // Fetch vouches and find all stakes the voter is eligible to cast (own stake + delegated stake).
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(&env));

    // Issue #1069: Resolve delegation - if this voucher has delegated, the delegate votes with this stake
    let effective_voter = resolve_vote_delegation(&env, &voter);
    let actual_voter = if effective_voter != voter {
        // This voucher's stake is counted under the delegate's vote
        // The delegate should call vote_slash separately
        return Ok(VoteSlashResult::DelegateWillVote); // Silently succeed - delegate will vote
    } else {
        voter.clone()
    };

    let voucher_stake = vouches
        .iter()
        .find(|v| v.voucher == actual_voter)
        .map(|v| v.stake)
        .ok_or(ContractError::VoucherNotFound)?;

    // Issue #1069: Calculate total stake including delegated votes
    // For each delegate, sum up their own stake plus all delegated stakes
    let mut total_stake: i128 = 0;
    let mut effective_stake: i128 = voucher_stake;
    
    for v in vouches.iter() {
        let voter = resolve_vote_delegation(&env, &v.voucher);
        if voter == v.voucher {
            // This voucher votes for themselves
            total_stake += v.stake;
        } else if voter == actual_voter {
            // This voucher's stake is delegated to the actual voter
            effective_stake += v.stake;
            total_stake += v.stake;
        } else {
            // Delegated to someone else, still counts in total
            total_stake += v.stake;
        }
    }

    // Load or initialise the vote record.
    let mut vote: SlashVoteRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashVote(borrower.clone()))
        .unwrap_or(SlashVoteRecord {
            approve_stake: 0,
            reject_stake: 0,
            voters: Vec::new(&env),
            executed: false,
        });

    if vote.executed {
        if has_active_loan(&env, &borrower) {
            // This is a fresh loan — reset the vote record for the new proposal.
            vote = SlashVoteRecord {
                approve_stake: 0,
                reject_stake: 0,
                voters: Vec::new(&env),
                executed: false,
            };
        } else {
            panic_with_error!(&env, ContractError::SlashAlreadyExecuted);
        }
    }

    // Prevent double-voting by the effective voter.
    if vote.voters.iter().any(|v| resolve_vote_delegation(&env, &v) == actual_voter) {
        return Err(ContractError::AlreadyVoted);
    }

    if approve {
        vote.approve_stake = vote.approve_stake.checked_add(effective_stake).ok_or(ContractError::ArithmeticError)?;
    } else {
        vote.reject_stake = vote.reject_stake.checked_add(effective_stake).ok_or(ContractError::ArithmeticError)?;
    }
    vote.voters.push_back(actual_voter.clone());

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("voted")),
        (actual_voter.clone(), borrower.clone(), approve, effective_stake),
    );

    // Check quorum using the effective total stake.
    let quorum_bps: u32 = env
        .storage()
        .instance()
        .get(&DataKey::SlashVoteQuorum)
        .unwrap_or(DEFAULT_SLASH_VOTE_QUORUM_BPS);

    // Use ceiling division to prevent rounding down
    let quorum_reached = total_stake > 0
        && (vote.approve_stake.checked_mul(BPS_DENOMINATOR).ok_or(ContractError::ArithmeticError)?
            .checked_add(total_stake).ok_or(ContractError::ArithmeticError)?
            .checked_sub(1).ok_or(ContractError::ArithmeticError)?) / total_stake
            >= quorum_bps as i128;

    if quorum_reached {
        vote.executed = true;
        env.storage()
            .persistent()
            .set(&DataKey::SlashVote(borrower.clone()), &vote);

        // Instead of immediately executing, create a pending slash record
        let cfg = config(&env);
        let now = env.ledger().timestamp();
        let executable_at = now + cfg.slash_delay_seconds;

        let pending_slash = PendingSlashRecord {
            borrower: borrower.clone(),
            approved_at: now,
            executable_at,
            executed: false,
        };

        env.storage()
            .persistent()
            .set(&DataKey::PendingSlashExecution(borrower.clone()), &pending_slash);

        env.events().publish(
            (symbol_short!("gov"), symbol_short!("slsh_pend")),
            (borrower.clone(), now, executable_at),
        );
    } else {
        env.storage()
            .persistent()
            .set(&DataKey::SlashVote(borrower.clone()), &vote);
    }

    Ok(VoteSlashResult::VoteCounted)
}

/// Returns the current slash vote record for a borrower, if any.
pub fn get_slash_vote(env: Env, borrower: Address) -> Option<SlashVoteRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::SlashVote(borrower))
}

/// Set the quorum threshold (in basis points) required to auto-execute a slash.
/// Requires admin approval â€” called from admin module.
pub fn set_slash_vote_quorum(env: &Env, quorum_bps: u32) {
    if quorum_bps > 10_000 {
        panic_with_error!(env, ContractError::InvalidBps);
    }
    env.storage()
        .instance()
        .set(&DataKey::SlashVoteQuorum, &quorum_bps);
}

pub fn get_slash_vote_quorum(env: Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::SlashVoteQuorum)
        .unwrap_or(DEFAULT_SLASH_VOTE_QUORUM_BPS)
}

/// Execute a slash vote if quorum has been met.
/// Anyone can call this function to execute a slash once quorum is reached.
/// Requires approximately 2/3 (6667 bps) of total vouched stake to have approved.
pub fn execute_slash_vote(env: Env, borrower: Address) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let vote: crate::types::SlashVoteRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashVote(borrower.clone()))
        .ok_or(ContractError::SlashVoteNotFound)?;

    if vote.executed {
        return Err(ContractError::SlashAlreadyExecuted);
    }

    // Get total stake for the borrower
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(&env));
    let total_stake: i128 = vouches.iter().map(|v| v.stake).sum();

    // Retrieve quorum threshold (default ≈ 66.67%)
    let quorum_bps: u32 = env
        .storage()
        .instance()
        .get(&DataKey::SlashVoteQuorum)
        .unwrap_or(DEFAULT_SLASH_VOTE_QUORUM_BPS);

    // Calculate required quorum stake using ceiling division
    let quorum_stake = (total_stake
        .checked_mul(quorum_bps as i128)
        .unwrap_or(i128::MAX)
        .checked_add(BPS_DENOMINATOR - 1)
        .unwrap_or(i128::MAX))
        / BPS_DENOMINATOR;

    // Check if approval stake meets quorum
    if vote.approve_stake < quorum_stake {
        return Err(ContractError::QuorumNotMet);
    }

    // Mark as executed and execute the slash
    let mut updated_vote = vote;
    updated_vote.executed = true;
    env.storage()
        .persistent()
        .set(&DataKey::SlashVote(borrower.clone()), &updated_vote);

    execute_slash(&env, &borrower)?;

    Ok(())
}

/// Execute a pending slash after the delay period has passed.
/// Anyone can call this function to execute a pending slash once the delay has elapsed.
pub fn execute_pending_slash(env: Env, borrower: Address) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let pending_slash: PendingSlashRecord = env
        .storage()
        .persistent()
        .get(&DataKey::PendingSlashExecution(borrower.clone()))
        .ok_or(ContractError::SlashVoteNotFound)?;

    if pending_slash.executed {
        return Err(ContractError::SlashAlreadyExecuted);
    }

    // Check if delay period has passed
    let now = env.ledger().timestamp();
    if now < pending_slash.executable_at {
        return Err(ContractError::DelayNotElapsed);
    }

    // Mark as executed and execute the slash
    let mut updated_pending = pending_slash;
    updated_pending.executed = true;
    env.storage()
        .persistent()
        .set(&DataKey::PendingSlashExecution(borrower.clone()), &updated_pending);

    execute_slash(&env, &borrower)?;

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("slsh_exec")),
        (borrower.clone(), now),
    );

    Ok(())
}

// â”€â”€ Internal â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn next_slash_id(env: &Env) -> u64 {
    let id = env
        .storage()
        .instance()
        .get(&DataKey::SlashRecordCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("slash ID overflow");
    env.storage()
        .instance()
        .set(&DataKey::SlashRecordCounter, &id);
    id
}

pub fn get_slash_record(env: Env, slash_id: u64) -> Option<SlashRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::SlashRecord(slash_id))
}

pub fn get_slash_record_for_borrower(env: Env, borrower: Address) -> Option<SlashRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::SlashAudit(borrower))
}

pub fn reverse_slash(
    env: Env,
    admin_signers: Vec<Address>,
    slash_id: u64,
    reason: soroban_sdk::String,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    require_admin_approval(&env, &admin_signers);

    let mut record: SlashRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashRecord(slash_id))
        .ok_or(ContractError::SlashRecordNotFound)?;

    if record.reversed {
        return Err(ContractError::SlashAlreadyReversed);
    }

    let loan = get_latest_loan_record(&env, &record.borrower).ok_or(ContractError::NoActiveLoan)?;
    let token = soroban_sdk::token::Client::new(&env, &loan.token_address);
    let contract = env.current_contract_address();

    token.transfer(&contract, &record.borrower, &record.total_slashed);

    record.reversed = true;
    record.reversal_reason = Some(reason.clone());

    env.storage()
        .persistent()
        .set(&DataKey::SlashRecord(slash_id), &record);
    env.storage()
        .persistent()
        .set(&DataKey::SlashAudit(record.borrower.clone()), &record);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("rev_slsh")),
        (slash_id, record.borrower.clone(), reason),
    );

    Ok(())
}

fn execute_slash(env: &Env, borrower: &Address) -> Result<(), ContractError> {
    let cfg = config(env);
    let now = env.ledger().timestamp();

    if cfg.slash_cooldown_seconds > 0 {
        if let Some(last) = env
            .storage()
            .persistent()
            .get(&DataKey::LastSlashedAt(borrower.clone()))
        {
            if now.saturating_sub(last) < cfg.slash_cooldown_seconds {
                return Err(ContractError::SlashCooldownActive);
            }
        }
    }

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));

    let mut loan = get_active_loan_record(env, borrower)?;
    if loan.status == LoanStatus::Defaulted {
        panic_with_error!(env, ContractError::SlashAlreadyExecuted);
    }
    let loan_token = soroban_sdk::token::Client::new(env, &loan.token_address);

    // Calculate total stake backing this borrower (used for loan-size scaling)
    let _total_stake: i128 = vouches.iter().map(|v| v.stake).sum();

    // Calculate effective slash basis points based on default count (graduated penalties).
    // Note: the dynamic-health calculation is intentionally not used here —
    // the graduated default_count logic is the authoritative rate for execute_slash.
    let default_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::DefaultCount(borrower.clone()))
        .unwrap_or(0);
    let base_slash_bps = cfg.slash_bps;
    let mut effective_slash_bps = base_slash_bps + (default_count as i128) * 500;
    if effective_slash_bps > 10_000 {
        effective_slash_bps = 10_000;
    }

    let mut total_slashed: i128 = 0;
    let mut remaining_vouches: Vec<VouchRecord> = Vec::new(env);

    for v in vouches.iter() {
        if v.token != loan.token_address {
            // Return non-matching-token vouches in full to their owner.
            if !is_zero_address(env, &v.token) {
                let other_token = soroban_sdk::token::Client::new(env, &v.token);
                other_token.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            }
            remaining_vouches.push_back(v);
            continue;
        }
        let slash_amount = v.stake.checked_mul(effective_slash_bps).ok_or(ContractError::ArithmeticError)? / BPS_DENOMINATOR;
        let remaining = v.stake.checked_sub(slash_amount).ok_or(ContractError::ArithmeticError)?;
        total_slashed = total_slashed.checked_add(slash_amount).ok_or(ContractError::ArithmeticError)?;

        if remaining > 0 {
            loan_token.transfer(&env.current_contract_address(), &v.voucher, &remaining);
        }

        let mut stats: crate::types::VoucherStats = env
            .storage()
            .persistent()
            .get(&DataKey::VoucherStats(v.voucher.clone()))
            .unwrap_or(crate::types::VoucherStats {
                successful_vouches: 0,
                total_vouches_slashed: 0,
                total_yield_earned: 0,
                total_slashed: 0,
            });
        stats.total_vouches_slashed += 1;
        stats.total_slashed += slash_amount;
        env.storage()
            .persistent()
            .set(&DataKey::VoucherStats(v.voucher.clone()), &stats);
    }

    env.storage()
        .persistent()
        .set(&DataKey::LastSlashedAt(borrower.clone()), &now);

    // Store slashed funds in escrow instead of immediately burning
    let release_timestamp = now + crate::types::SLASH_ESCROW_PERIOD;
    env.storage()
        .persistent()
        .set(&DataKey::SlashEscrow(borrower.clone()), &(total_slashed, release_timestamp));

    loan.status = LoanStatus::Defaulted;
    env.storage()
        .persistent()
        .set(&DataKey::Loan(loan.id), &loan);
    env.storage()
        .persistent()
        .set(&DataKey::LatestLoan(borrower.clone()), &loan.id);
    env.storage()
        .persistent()
        .remove(&DataKey::ActiveLoan(borrower.clone()));

    let count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::DefaultCount(borrower.clone()))
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::DefaultCount(borrower.clone()), &(count + 1));

    if remaining_vouches.is_empty() {
        env.storage()
            .persistent()
            .remove(&DataKey::Vouches(borrower.clone()));
    } else {
        env.storage()
            .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &remaining_vouches);
    }

    let slash_id = next_slash_id(env);
    let record = SlashRecord {
        slash_id,
        borrower: borrower.clone(),
        loan_id: loan.id,
        loan_amount: loan.amount,
        total_slashed,
        slash_timestamp: now,
        recovery_amount: 0,
        reversal_reason: None,
        reversed: false,
        effective_slash_bps,
    };
    env.storage()
        .persistent()
        .set(&DataKey::SlashRecord(slash_id), &record);
    env.storage()
        .persistent()
        .set(&DataKey::SlashAudit(borrower.clone()), &record);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("slashed")),
        (borrower.clone(), total_slashed, slash_id, effective_slash_bps),
    );

    // Process withdrawal queue after slash is executed (Issue #10)
    crate::vouch::process_withdrawal_queue(env, borrower);

    Ok(())
}

/// â”€â”€ Issue 109: Slash Proposal Confirmation Window â”€â”€
///
/// Implements a two-step slash with timelock pattern:
/// 1. propose_slash: Admin creates a proposal, sets execution time (eta)
/// 2. execute_slash_proposal: After delay, anyone can execute

/// Propose a slash action with a delay before execution.
/// This implements the "confirmation window" for the slash action.

pub fn propose_slash(
    env: Env,
    proposer: Address,
    borrower: Address,
    delay_secs: u64,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;

    // Verify borrower has an active loan
    let _loan = get_active_loan_record(&env, &borrower)?;

    // Get or initialize timelock counter
    let proposal_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TimelockCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal ID overflow");

    let eta = env.ledger().timestamp() + delay_secs;

    let proposal = TimelockProposal {
        id: proposal_id,
        action: TimelockAction::Slash(borrower.clone()),
        proposer: proposer.clone(),
        eta,
        executed: false,
        cancelled: false,
    };

    env.storage()
        .instance()
        .set(&DataKey::Timelock(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::TimelockCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("proposed")),
        (proposal_id, proposer, borrower, eta),
    );

    Ok(proposal_id)
}

/// Execute a previously proposed slash action after the delay has passed.
pub fn execute_slash_proposal(env: Env, proposal_id: u64) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    // Get the proposal
    let mut proposal: TimelockProposal = env
        .storage()
        .instance()
        .get(&DataKey::Timelock(proposal_id))
        .ok_or(ContractError::NoActiveLoan)?; // Use existing error as placeholder

    // Check proposal state
    if proposal.executed {
        return Err(ContractError::SlashAlreadyExecuted);
    }
    if proposal.cancelled {
        return Err(ContractError::NoActiveLoan); // Use existing error as placeholder
    }

    // Check delay has passed
    if env.ledger().timestamp() < proposal.eta {
        return Err(ContractError::NoActiveLoan); // Use existing error as placeholder
    }

    // Check expiry (72 hours from eta)
    const TIMELOCK_EXPIRY: u64 = 72 * 60 * 60;
    if env.ledger().timestamp() > proposal.eta + TIMELOCK_EXPIRY {
        return Err(ContractError::NoActiveLoan); // Use existing error as placeholder
    }

    // Extract borrower from the Slash action
    if let TimelockAction::Slash(borrower) = &proposal.action {
        // Mark as executed before calling execute_slash to prevent reentrancy
        proposal.executed = true;
        env.storage()
            .instance()
            .set(&DataKey::Timelock(proposal_id), &proposal);

        // Execute the slash
        execute_slash(&env, borrower)?;

        env.events().publish(
            (symbol_short!("gov"), symbol_short!("executed")),
            (proposal_id, borrower.clone()),
        );

        Ok(())
    } else {
        Err(ContractError::NoActiveLoan) // Only Slash actions supported in this release
    }
}

/// Cancel a pending slash proposal (only by proposer or admin).
pub fn cancel_slash_proposal(
    env: Env,
    caller: Address,
    proposal_id: u64,
) -> Result<(), ContractError> {
    caller.require_auth();

    let mut proposal: TimelockProposal = env
        .storage()
        .instance()
        .get(&DataKey::Timelock(proposal_id))
        .ok_or(ContractError::NoActiveLoan)?;

    // Only proposer can cancel
    if caller != proposal.proposer {
        panic_with_error!(&env, ContractError::UnauthorizedCaller);
    }

    if proposal.executed || proposal.cancelled {
        return Err(ContractError::SlashAlreadyExecuted);
    }

    proposal.cancelled = true;
    env.storage()
        .instance()
        .set(&DataKey::Timelock(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("cancelled")),
        (proposal_id, caller),
    );

    Ok(())
}

/// Get a timelock proposal by ID.
pub fn get_timelock_proposal(env: Env, proposal_id: u64) -> Option<TimelockProposal> {
    env.storage()
        .instance()
        .get(&DataKey::Timelock(proposal_id))
}

/// Issue #552: Appeal a slash decision with evidence. Only the slashed voucher can appeal.
pub fn appeal_slash_with_evidence(
    env: Env,
    voucher: Address,
    borrower: Address,
    evidence_hash: soroban_sdk::BytesN<32>,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_paused(&env)?;

    // Verify the loan was defaulted
    let loan = get_latest_loan_record(&env, &borrower)
        .ok_or(ContractError::NoActiveLoan)?;
    if loan.status != LoanStatus::Defaulted {
        return Err(ContractError::NoActiveLoan);
    }

    // Create appeal record
    let appeal = SlashAppealRecord {
        borrower: borrower.clone(),
        voucher: voucher.clone(),
        evidence_hash,
        appeal_timestamp: env.ledger().timestamp(),
        approved: None,
        admin_votes: Vec::new(&env),
    };

    env.storage()
        .persistent()
        .set(&DataKey::SlashAppeal(borrower.clone(), voucher.clone()), &appeal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("appeal")),
        (voucher, borrower),
    );

    Ok(())
}

/// Issue #552: Admin votes on a slash appeal.
pub fn vote_on_slash_appeal(
    env: Env,
    admin_signers: Vec<Address>,
    borrower: Address,
    voucher: Address,
    approve: bool,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    // Verify admin approval
    crate::helpers::require_admin_approval(&env, &admin_signers);

    let mut appeal: SlashAppealRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashAppeal(borrower.clone(), voucher.clone()))
        .ok_or(ContractError::NoActiveLoan)?;

    if appeal.approved.is_some() {
        return Err(ContractError::SlashAlreadyExecuted);
    }

    appeal.approved = Some(approve);
    appeal.admin_votes = admin_signers.clone();

    env.storage()
        .persistent()
        .set(&DataKey::SlashAppeal(borrower.clone(), voucher.clone()), &appeal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("appl_vote")),
        (borrower, voucher, approve),
    );

    Ok(())
}

/// Issue #552: Execute a slash appeal if approved. Reverses the slash.
pub fn execute_slash_appeal(
    env: Env,
    borrower: Address,
    voucher: Address,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let appeal: SlashAppealRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashAppeal(borrower.clone(), voucher.clone()))
        .ok_or(ContractError::NoActiveLoan)?;

    if appeal.approved != Some(true) {
        return Err(ContractError::UnauthorizedCaller);
    }

    // Get the loan to find the token
    let loan = get_latest_loan_record(&env, &borrower)
        .ok_or(ContractError::NoActiveLoan)?;

    let token_client = soroban_sdk::token::Client::new(&env, &loan.token_address);

    // Get the slash record to find the actual effective_slash_bps that was applied
    let slash_record: SlashRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashAudit(borrower.clone()))
        .ok_or(ContractError::SlashRecordNotFound)?;

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(&env));

    let original_stake = vouches
        .iter()
        .find(|v| v.voucher == voucher && v.token == loan.token_address)
        .map(|v| v.stake)
        .unwrap_or(0);

    // Calculate the actual slashed amount based on the effective_slash_bps that was applied
    let slashed_amount = original_stake.checked_mul(slash_record.effective_slash_bps).ok_or(ContractError::ArithmeticError)? / BPS_DENOMINATOR;
    let restored_amount = original_stake.checked_sub(slashed_amount).ok_or(ContractError::ArithmeticError)?;

    if restored_amount > 0 {
        token_client.transfer(
            &env.current_contract_address(),
            &voucher,
            &restored_amount,
        );
    }

    // Remove the appeal record
    env.storage()
        .persistent()
        .remove(&DataKey::SlashAppeal(borrower.clone(), voucher.clone()));

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("appl_exec")),
        (borrower, voucher),
    );

    Ok(())
}

// â”€â”€ Issue #680: Slash threshold governance voting â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub fn propose_slash_threshold(
    env: Env,
    proposer: Address,
    new_threshold: i128,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;
    require_governance_participant(&env, &proposer)?;

    if new_threshold <= 0 || new_threshold > 10_000 {
        return Err(ContractError::InvalidBps);
    }

    let proposal_id: u64 = env
        .storage()
        .instance()
        .get::<DataKey, u64>(&DataKey::SlashThresholdProposalCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal id overflow");

    let proposal = SlashThresholdProposal {
        id: proposal_id,
        proposer: proposer.clone(),
        proposed_threshold: new_threshold,
        proposed_at: env.ledger().timestamp(),
        approve_votes: 0,
        reject_votes: 0,
        voters: Vec::new(&env),
        finalized: false,
    };

    env.storage()
        .instance()
        .set(&DataKey::SlashThresholdProposal(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::SlashThresholdProposalCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("sth_prop")),
        (proposal_id, proposer, new_threshold),
    );

    Ok(proposal_id)
}

pub fn vote_slash_threshold(
    env: Env,
    voter: Address,
    proposal_id: u64,
    approve: bool,
) -> Result<(), ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;
    require_governance_participant(&env, &voter)?;

    let mut proposal: SlashThresholdProposal = env
        .storage()
        .instance()
        .get(&DataKey::SlashThresholdProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.finalized {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let cfg = config(&env);
    let now = env.ledger().timestamp();
    if now >= proposal.proposed_at + cfg.voting_period_seconds {
        return Err(ContractError::VotingPeriodEnded);
    }

    if proposal.voters.iter().any(|v| v == voter) {
        return Err(ContractError::AlreadyVoted);
    }

    if approve {
        proposal.approve_votes += 1;
    } else {
        proposal.reject_votes += 1;
    }
    proposal.voters.push_back(voter.clone());

    env.storage()
        .instance()
        .set(&DataKey::SlashThresholdProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("sth_vote")),
        (proposal_id, voter, approve),
    );

    Ok(())
}

pub fn finalize_slash_threshold(env: Env, proposal_id: u64) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let mut proposal: SlashThresholdProposal = env
        .storage()
        .instance()
        .get(&DataKey::SlashThresholdProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.finalized {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let cfg = config(&env);
    let now = env.ledger().timestamp();
    let voting_end = proposal.proposed_at + cfg.voting_period_seconds;

    if now < voting_end {
        return Err(ContractError::TimelockNotReady);
    }
    if now > voting_end + cfg.voting_period_seconds {
        return Err(ContractError::TimelockExpired);
    }

    proposal.finalized = true;
    env.storage()
        .instance()
        .set(&DataKey::SlashThresholdProposal(proposal_id), &proposal);

    if proposal.approve_votes > proposal.reject_votes {
        let mut cfg = config(&env);
        cfg.slash_bps = proposal.proposed_threshold;
        env.storage().instance().set(&DataKey::Config, &cfg);
        env.events().publish(
            (symbol_short!("gov"), symbol_short!("sth_ok")),
            (proposal_id, proposal.proposed_threshold),
        );
    } else {
        env.events().publish(
            (symbol_short!("gov"), symbol_short!("sth_no")),
            proposal_id,
        );
    }

    Ok(())
}

pub fn get_slash_threshold_proposal(
    env: Env,
    proposal_id: u64,
) -> Option<SlashThresholdProposal> {
    env.storage()
        .instance()
        .get(&DataKey::SlashThresholdProposal(proposal_id))
}

// ── Slashing Transparency Report ──────────────────────────────────────────────

/// Generate (or refresh) the monthly slashing report for `month_id`.
///
/// `month_id` = `unix_timestamp / MONTHLY_PERIOD_SECS`.
/// Iterates all recorded slash events and aggregates those whose
/// `slash_timestamp` falls within the requested month window.
/// The result is persisted under `DataKey::SlashingReport(month_id)`.
pub fn generate_slashing_report(env: Env, month_id: u64) -> SlashingReportRecord {
    let total_ids: u64 = env
        .storage()
        .instance()
        .get(&DataKey::SlashRecordCounter)
        .unwrap_or(0);

    let month_start = month_id * MONTHLY_PERIOD_SECS;
    let month_end = month_start + MONTHLY_PERIOD_SECS;

    let mut slash_ids: Vec<u64> = Vec::new(&env);
    let mut total_slashed: i128 = 0;
    let mut total_slashes: u32 = 0;
    let mut total_reversed: u32 = 0;

    for id in 1..=total_ids {
        let record: crate::types::SlashRecord = match env
            .storage()
            .persistent()
            .get(&DataKey::SlashRecord(id))
        {
            Some(r) => r,
            None => continue,
        };

        if record.slash_timestamp >= month_start && record.slash_timestamp < month_end {
            total_slashes += 1;
            total_slashed += record.total_slashed;
            if record.reversed {
                total_reversed += 1;
            }
            slash_ids.push_back(id);
        }
    }

    let report = SlashingReportRecord {
        month_id,
        total_slashes,
        total_slashed,
        total_reversed,
        slash_ids,
    };

    env.storage()
        .persistent()
        .set(&DataKey::SlashingReport(month_id), &report);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("rpt_gen")),
        (month_id, total_slashes, total_slashed),
    );

    report
}

/// Return the cached slashing report for `month_id`, or `None` if not yet generated.
pub fn get_slashing_report(env: Env, month_id: u64) -> Option<SlashingReportRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::SlashingReport(month_id))
}

// ── Issue #687: Governance-based admin removal ────────────────────────────────

/// Propose removing a compromised admin via governance vote.
///
/// Any governance participant (active voucher or admin) may call this.
/// The proposal passes once `approve_votes >= Config.removal_vote_threshold`.
pub fn propose_admin_removal(
    env: Env,
    proposer: Address,
    admin_to_remove: Address,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;
    require_governance_participant(&env, &proposer)?;

    let cfg = config(&env);

    if cfg.removal_vote_threshold == 0 {
        return Err(ContractError::UnauthorizedCaller);
    }

    if !cfg.admins.iter().any(|a| a == admin_to_remove) {
        return Err(ContractError::UnauthorizedCaller);
    }

    let proposal_id: u64 = env
        .storage()
        .instance()
        .get::<DataKey, u64>(&DataKey::AdminRemovalProposalCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal id overflow");

    let proposal = AdminRemovalProposal {
        id: proposal_id,
        admin_to_remove: admin_to_remove.clone(),
        proposer: proposer.clone(),
        approve_votes: 0,
        reject_votes: 0,
        voters: Vec::new(&env),
        proposed_at: env.ledger().timestamp(),
        finalized: false,
    };

    env.storage()
        .instance()
        .set(&DataKey::AdminRemovalProposal(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::AdminRemovalProposalCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("rm_prop")),
        (proposal_id, proposer, admin_to_remove),
    );

    Ok(proposal_id)
}

/// Cast a vote on an admin removal proposal.
///
/// Any governance participant may vote once per proposal.
/// Voting closes once the proposal is finalized.
pub fn vote_admin_removal(
    env: Env,
    voter: Address,
    proposal_id: u64,
    approve: bool,
) -> Result<(), ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;
    require_governance_participant(&env, &voter)?;

    let mut proposal: AdminRemovalProposal = env
        .storage()
        .instance()
        .get(&DataKey::AdminRemovalProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.finalized {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    if proposal.voters.iter().any(|v| v == voter) {
        return Err(ContractError::AlreadyVoted);
    }

    if approve {
        proposal.approve_votes += 1;
    } else {
        proposal.reject_votes += 1;
    }
    proposal.voters.push_back(voter.clone());

    env.storage()
        .instance()
        .set(&DataKey::AdminRemovalProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("rm_vote")),
        (proposal_id, voter, approve),
    );

    Ok(())
}

/// Finalize an admin removal proposal.
///
/// Succeeds once `approve_votes >= Config.removal_vote_threshold`.
/// On success, the targeted admin is removed from `Config.admins`.
/// Fails if the removal would drop the admin count below `admin_threshold`.
pub fn finalize_admin_removal(env: Env, proposal_id: u64) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let mut proposal: AdminRemovalProposal = env
        .storage()
        .instance()
        .get(&DataKey::AdminRemovalProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.finalized {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let mut cfg = config(&env);

    if proposal.approve_votes < cfg.removal_vote_threshold {
        return Err(ContractError::QuorumNotMet);
    }

    let idx = cfg
        .admins
        .iter()
        .position(|a| a == proposal.admin_to_remove)
        .ok_or(ContractError::UnauthorizedCaller)? as u32;

    cfg.admins.remove(idx);

    if cfg.admins.len() < cfg.admin_threshold {
        return Err(ContractError::InvalidAdminThreshold);
    }

    env.storage().instance().set(&DataKey::Config, &cfg);

    proposal.finalized = true;
    env.storage()
        .instance()
        .set(&DataKey::AdminRemovalProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("rm_done")),
        (proposal_id, proposal.admin_to_remove.clone()),
    );

    Ok(())
}

/// Return a governance admin-removal proposal by ID.
pub fn get_admin_removal_proposal(env: Env, proposal_id: u64) -> Option<AdminRemovalProposal> {
    env.storage()
        .instance()
        .get(&DataKey::AdminRemovalProposal(proposal_id))
}


/// Issue #841: Borrower appeals a slash by initiating a 7-day escrow period.
/// The slashed funds (50% of total stake) are held in escrow.
/// Vouchers can vote within 7 days; 2/3 quorum overturn the slash.
pub fn appeal_slash(
    env: Env,
    borrower: Address,
) -> Result<(), ContractError> {
    borrower.require_auth();
    require_not_paused(&env)?;

    let now = env.ledger().timestamp();

    // Check if an appeal already exists
    if let Some(existing_escrow) = env
        .storage()
        .persistent()
        .get::<DataKey, SlashEscrow>(&DataKey::SlashEscrow(borrower.clone()))
    {
        // Check if escrow is still active
        if now < existing_escrow.release_timestamp {
            return Err(ContractError::AppealNotFound); // Appeal already in progress
        }
    }

    // Get the latest loan record to verify there was a slash
    let loan = get_latest_loan_record(&env, &borrower)
        .ok_or(ContractError::NoActiveLoan)?;

    if loan.status != LoanStatus::Defaulted {
        return Err(ContractError::InvalidStateTransition);
    }

    // Get slash record to find the escrow amount
    let slash_record: SlashRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashAudit(borrower.clone()))
        .ok_or(ContractError::SlashRecordNotFound)?;

    // Create escrow record (50% of total slashed was already burned)
    let escrow = SlashEscrow {
        borrower: borrower.clone(),
        loan_id: loan.id,
        escrow_amount: slash_record.total_slashed,
        release_timestamp: now + SLASH_APPEAL_PERIOD,
        status: AppealStatus::Pending,
    };

    env.storage()
        .persistent()
        .set(&DataKey::SlashEscrow(borrower.clone()), &escrow);

    // Initialize appeal record
    let appeal = SlashEscrowAppealRecord {
        borrower: borrower.clone(),
        loan_id: loan.id,
        approve_stake: 0,
        reject_stake: 0,
        voters: Vec::new(&env),
        appeal_timestamp: now,
    };

    env.storage()
        .persistent()
        .set(&DataKey::SlashEscrowAppeal(borrower.clone()), &appeal);

    env.events().publish(
        (symbol_short!("app"), symbol_short!("initiated")),
        (borrower.clone(), loan.id, escrow.escrow_amount),
    );

    Ok(())
}

/// Issue #841: Voucher votes on a slash appeal.
/// Requires the voucher to be registered as backing the borrower.
/// Once 2/3 quorum is reached, appeal is automatically approved.
pub fn vote_appeal(
    env: Env,
    voter: Address,
    borrower: Address,
    approve: bool,
) -> Result<(), ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;

    let now = env.ledger().timestamp();

    // Check if escrow exists and is still active
    let escrow = env
        .storage()
        .persistent()
        .get::<DataKey, SlashEscrow>(&DataKey::SlashEscrow(borrower.clone()))
        .ok_or(ContractError::AppealNotFound)?;

    if now >= escrow.release_timestamp {
        return Err(ContractError::EscrowExpired);
    }

    // Check if escrow is still pending (not already finalized)
    if escrow.status != AppealStatus::Pending {
        return Err(ContractError::InvalidStateTransition);
    }

    // Get vouches for borrower
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(&env));

    let total_stake: i128 = vouches.iter().map(|v| v.stake).sum();

    // Load appeal record
    let mut appeal: SlashEscrowAppealRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashEscrowAppeal(borrower.clone()))
        .ok_or(ContractError::AppealNotFound)?;

    let mut voting_stake: i128 = 0;
    let mut newly_voted_vouches: Vec<Address> = Vec::new(&env);

    for v in vouches.iter() {
        // Voter can vote if they are the original voucher OR the designated delegate
        if v.voucher == voter || v.delegate == Some(voter.clone()) {
            // Prevent double voting
            if !appeal.voters.iter().any(|already_voted| already_voted == v.voucher) {
                voting_stake = voting_stake.checked_add(v.stake).ok_or(ContractError::ArithmeticError)?;
                newly_voted_vouches.push_back(v.voucher.clone());
            }
        }
    }

    if voting_stake == 0 {
        return Err(ContractError::VoucherNotFound);
    }

    // Record vote
    if approve {
        appeal.approve_stake = appeal
            .approve_stake
            .checked_add(voting_stake)
            .ok_or(ContractError::ArithmeticError)?;
    } else {
        appeal.reject_stake = appeal
            .reject_stake
            .checked_add(voting_stake)
            .ok_or(ContractError::ArithmeticError)?;
    }

    for v_addr in newly_voted_vouches {
        appeal.voters.push_back(v_addr);
    }

    env.events().publish(
        (symbol_short!("app"), symbol_short!("voted")),
        (voter.clone(), borrower.clone(), approve, voting_stake),
    );

    // Check if 2/3 quorum is reached
    let quorum_reached = total_stake > 0
        && (appeal.approve_stake.checked_mul(BPS_DENOMINATOR).ok_or(ContractError::ArithmeticError)?
            / total_stake) >= APPEAL_OVERRIDE_QUORUM_BPS as i128;

    env.storage()
        .persistent()
        .set(&DataKey::SlashEscrowAppeal(borrower.clone()), &appeal);

    if quorum_reached {
        // Auto-finalize on quorum
        finalize_appeal_internal(&env, &borrower, true)?;
    }

    Ok(())
}

/// Issue #841: Finalize a slash appeal after voting or 7-day escrow period.
/// If approve_stake >= 2/3 quorum: slash is overturned, funds returned to vouchers.
/// Otherwise: funds are burned.
pub fn finalize_appeal(
    env: Env,
    borrower: Address,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let now = env.ledger().timestamp();

    // Check if escrow exists
    let escrow = env
        .storage()
        .persistent()
        .get::<DataKey, SlashEscrow>(&DataKey::SlashEscrow(borrower.clone()))
        .ok_or(ContractError::AppealNotFound)?;

    // Only finalize if escrow period has expired or quorum was reached
    if now < escrow.release_timestamp && escrow.status == AppealStatus::Pending {
        return Err(ContractError::EscrowExpired);
    }

    finalize_appeal_internal(&env, &borrower, false)
}

/// Internal helper to finalize an appeal.
fn finalize_appeal_internal(
    env: &Env,
    borrower: &Address,
    is_auto_finalize: bool,
) -> Result<(), ContractError> {
    // Get current escrow
    let mut escrow = env
        .storage()
        .persistent()
        .get::<DataKey, SlashEscrow>(&DataKey::SlashEscrow(borrower.clone()))
        .ok_or(ContractError::AppealNotFound)?;

    if escrow.status != AppealStatus::Pending {
        return Err(ContractError::InvalidStateTransition);
    }

    // Get appeal record
    let appeal: SlashEscrowAppealRecord = env
        .storage()
        .persistent()
        .get(&DataKey::SlashEscrowAppeal(borrower.clone()))
        .ok_or(ContractError::AppealNotFound)?;

    // Get total vouched stake
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));

    let total_stake: i128 = vouches.iter().map(|v| v.stake).sum();

    // Check if 2/3 quorum approved the appeal
    let appeal_approved = total_stake > 0
        && (appeal.approve_stake.checked_mul(BPS_DENOMINATOR).ok_or(ContractError::ArithmeticError)?
            / total_stake) >= APPEAL_OVERRIDE_QUORUM_BPS as i128;

    if appeal_approved {
        // Reverse the slash: restore funds to vouchers pro-rata
        escrow.status = AppealStatus::Approved;

        // Get the loan to find the token address for transfers
        let loan = get_latest_loan_record(env, borrower)
            .ok_or(ContractError::NoActiveLoan)?;
        let token_client = soroban_sdk::token::Client::new(env, &loan.token_address);
        let contract_address = env.current_contract_address();

        // Return funds to each voucher proportionally
        for vouch in vouches.iter() {
            let voucher_proportion = (vouch.stake.checked_mul(BPS_DENOMINATOR).ok_or(ContractError::ArithmeticError)? / total_stake) as u32;
            let return_amount = escrow.escrow_amount.checked_mul(voucher_proportion as i128).ok_or(ContractError::ArithmeticError)? / BPS_DENOMINATOR;

            if return_amount > 0 {
                // Transfer tokens back to voucher from escrow
                token_client.transfer(&contract_address, &vouch.voucher, &return_amount);

                env.events().publish(
                    (symbol_short!("app"), symbol_short!("funded")),
                    (borrower.clone(), vouch.voucher.clone(), return_amount),
                );
            }
        }
    } else {
        // Slash is final: burn the escrow amount
        escrow.status = AppealStatus::Rejected;
        
        // Add to slash treasury
        let current_treasury: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::SlashTreasury)
            .unwrap_or(0);
        
        let new_treasury = current_treasury.checked_add(escrow.escrow_amount).ok_or(ContractError::ArithmeticError)?;
        env.storage()
            .persistent()
            .set(&DataKey::SlashTreasury, &new_treasury);

        env.events().publish(
            (symbol_short!("app"), symbol_short!("rejected")),
            (borrower.clone(), escrow.escrow_amount),
        );
    }

    // Update escrow status
    env.storage()
        .persistent()
        .set(&DataKey::SlashEscrow(borrower.clone()), &escrow);

    env.events().publish(
        (symbol_short!("app"), symbol_short!("finalized")),
        (borrower.clone(), is_auto_finalize),
    );

    Ok(())
}


// ──────────────────────────────────────────────────────────────────────────────
// Issue #903: Risk Assessment Voting
// ──────────────────────────────────────────────────────────────────────────────

/// Propose a new risk threshold configuration.
pub fn propose_risk_threshold(
    env: Env,
    proposer: Address,
    min_risk_threshold: u32,
    max_risk_threshold: u32,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;

    // Validate thresholds
    if min_risk_threshold >= max_risk_threshold {
        panic_with_error!(&env, ContractError::InvalidAmount);
    }

    let proposal_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::RiskThresholdCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal ID overflow");

    let proposal = crate::types::RiskThresholdProposal {
        id: proposal_id,
        proposer: proposer.clone(),
        min_risk_threshold,
        max_risk_threshold,
        votes_for: 0,
        votes_against: 0,
        status: crate::types::GovernanceProposalStatus::Pending,
        created_at: env.ledger().timestamp(),
        eta: env.ledger().timestamp() + 86400,  // 24 hours
    };

    env.storage()
        .instance()
        .set(&DataKey::RiskThresholdProposal(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::RiskThresholdCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("risk"), symbol_short!("proposed")),
        (proposal_id, proposer, min_risk_threshold, max_risk_threshold),
    );

    Ok(proposal_id)
}

/// Vote on a risk threshold proposal.
pub fn vote_risk_threshold(
    env: Env,
    voter: Address,
    proposal_id: u64,
    approve: bool,
) -> Result<(), ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;

    let mut proposal: crate::types::RiskThresholdProposal = env
        .storage()
        .instance()
        .get(&DataKey::RiskThresholdProposal(proposal_id))
        .ok_or(ContractError::NoActiveLoan)?;

    let vote_weight = 1_000_i128;  // Default vote weight

    if approve {
        proposal.votes_for = proposal.votes_for.checked_add(vote_weight).expect("vote overflow");
    } else {
        proposal.votes_against = proposal
            .votes_against
            .checked_add(vote_weight)
            .expect("vote overflow");
    }

    env.storage()
        .instance()
        .set(&DataKey::RiskThresholdVote(proposal_id, voter.clone()), &approve);
    env.storage()
        .instance()
        .set(&DataKey::RiskThresholdProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("risk"), symbol_short!("voted")),
        (proposal_id, voter, approve),
    );

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Issue #904: Fee Structure Voting
// ──────────────────────────────────────────────────────────────────────────────

/// Propose a new fee structure.
pub fn propose_fee_structure(
    env: Env,
    proposer: Address,
    origination_fee_bps: u32,
    repayment_fee_bps: u32,
    late_fee_bps: u32,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;

    // Validate fee caps
    if origination_fee_bps > 500 || repayment_fee_bps > 250 || late_fee_bps > 1000 {
        panic_with_error!(&env, ContractError::InvalidAmount);
    }

    let proposal_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::FeeStructureCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal ID overflow");

    let proposal = crate::types::FeeStructureProposal {
        id: proposal_id,
        proposer: proposer.clone(),
        origination_fee_bps,
        repayment_fee_bps,
        late_fee_bps,
        votes_for: 0,
        votes_against: 0,
        status: crate::types::GovernanceProposalStatus::Pending,
        created_at: env.ledger().timestamp(),
        eta: env.ledger().timestamp() + 86400,  // 24 hours
    };

    env.storage()
        .instance()
        .set(&DataKey::FeeStructureProposal(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::FeeStructureCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("fee"), symbol_short!("proposed")),
        (
            proposal_id,
            proposer,
            origination_fee_bps,
            repayment_fee_bps,
        ),
    );

    Ok(proposal_id)
}

/// Vote on a fee structure proposal.
pub fn vote_fee_structure(
    env: Env,
    voter: Address,
    proposal_id: u64,
    approve: bool,
) -> Result<(), ContractError> {
    voter.require_auth();
    require_not_paused(&env)?;

    let mut proposal: crate::types::FeeStructureProposal = env
        .storage()
        .instance()
        .get(&DataKey::FeeStructureProposal(proposal_id))
        .ok_or(ContractError::NoActiveLoan)?;

    let vote_weight = 1_000_i128;

    if approve {
        proposal.votes_for = proposal.votes_for.checked_add(vote_weight).expect("vote overflow");
    } else {
        proposal.votes_against = proposal
            .votes_against
            .checked_add(vote_weight)
            .expect("vote overflow");
    }

    env.storage()
        .instance()
        .set(&DataKey::FeeStructureVote(proposal_id, voter.clone()), &approve);
    env.storage()
        .instance()
        .set(&DataKey::FeeStructureProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("fee"), symbol_short!("voted")),
        (proposal_id, voter, approve),
    );

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Issue #905: Withdrawal Timelock
// ──────────────────────────────────────────────────────────────────────────────

/// Queue a withdrawal with a timelock delay.
pub fn queue_withdrawal(
    env: Env,
    voucher: Address,
    borrower: Address,
    amount: i128,
    token: Address,
    delay_secs: u64,
) -> Result<u64, ContractError> {
    voucher.require_auth();
    require_not_paused(&env)?;

    if amount <= 0 {
        panic_with_error!(&env, ContractError::InsufficientFunds);
    }

    let lock_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::WithdrawalTimelockCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("lock ID overflow");

    let timelock = crate::types::WithdrawalTimelock {
        id: lock_id,
        voucher: voucher.clone(),
        borrower: borrower.clone(),
        amount,
        token: token.clone(),
        eta: env.ledger().timestamp() + delay_secs,
        executed: false,
        cancelled: false,
    };

    env.storage()
        .instance()
        .set(&DataKey::WithdrawalTimelock(lock_id), &timelock);
    env.storage()
        .instance()
        .set(&DataKey::WithdrawalTimelockCounter, &lock_id);

    env.events().publish(
        (symbol_short!("wit"), symbol_short!("queued")),
        (lock_id, voucher, borrower, amount),
    );

    Ok(lock_id)
}

/// Execute a queued withdrawal after the timelock expires.
pub fn execute_withdrawal(env: Env, lock_id: u64) -> Result<(), ContractError> {
    let mut timelock: crate::types::WithdrawalTimelock = env
        .storage()
        .instance()
        .get(&DataKey::WithdrawalTimelock(lock_id))
        .ok_or(ContractError::NoActiveLoan)?;

    if timelock.executed {
        panic_with_error!(&env, ContractError::AlreadyRepaid);
    }

    if timelock.cancelled {
        panic_with_error!(&env, ContractError::InvalidStateTransition);
    }

    if env.ledger().timestamp() < timelock.eta {
        panic_with_error!(&env, ContractError::TimelockNotReady);
    }

    timelock.executed = true;

    env.storage()
        .instance()
        .set(&DataKey::WithdrawalTimelock(lock_id), &timelock);

    env.events().publish(
        (symbol_short!("wit"), symbol_short!("executed")),
        (lock_id, timelock.voucher, timelock.amount),
    );

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Issue #906: Cross-Chain Proposal Sync
// ──────────────────────────────────────────────────────────────────────────────

/// Initiate a cross-chain proposal sync.
pub fn initiate_cross_chain_sync(
    env: Env,
    proposer: Address,
    source_chain: String,
    target_chains: Vec<String>,
    proposal_type: String,
    proposal_data: soroban_sdk::Bytes,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;

    if target_chains.is_empty() {
        panic_with_error!(&env, ContractError::InvalidAmount);
    }

    let sync_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::CrossChainSyncCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("sync ID overflow");

    let votes_required = target_chains.len() as u32;

    let sync = crate::types::CrossChainProposalSync {
        id: sync_id,
        source_chain: source_chain.clone(),
        target_chains: target_chains.clone(),
        proposal_type: proposal_type.clone(),
        proposal_data: proposal_data.clone(),
        votes_required,
        votes_received: 0,
        status: crate::types::GovernanceProposalStatus::Pending,
        created_at: env.ledger().timestamp(),
        eta: env.ledger().timestamp() + 259200,  // 3 days
    };

    env.storage()
        .instance()
        .set(&DataKey::CrossChainProposalSync(sync_id), &sync);
    env.storage()
        .instance()
        .set(&DataKey::CrossChainSyncCounter, &sync_id);

    env.events().publish(
        (symbol_short!("xchain"), symbol_short!("initiated")),
        (sync_id, proposer, source_chain, target_chains.len()),
    );

    Ok(sync_id)
}

/// Vote on a cross-chain proposal sync from another chain.
pub fn vote_cross_chain_sync(
    env: Env,
    sync_id: u64,
    chain_id: String,
    approve: bool,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let mut sync: crate::types::CrossChainProposalSync = env
        .storage()
        .instance()
        .get(&DataKey::CrossChainProposalSync(sync_id))
        .ok_or(ContractError::NoActiveLoan)?;

    if sync.status != crate::types::GovernanceProposalStatus::Pending {
        panic_with_error!(&env, ContractError::InvalidStateTransition);
    }

    if approve {
        sync.votes_received = sync
            .votes_received
            .checked_add(1)
            .expect("vote overflow");

        // Execute if quorum reached
        if sync.votes_received >= sync.votes_required {
            sync.status = crate::types::GovernanceProposalStatus::Approved;
        }
    }

    env.storage()
        .instance()
        .set(&DataKey::CrossChainProposalSync(sync_id), &sync);

    env.events().publish(
        (symbol_short!("xchain"), symbol_short!("voted")),
        (sync_id, chain_id, approve),
    );

    Ok(())
}

pub fn propose_config_change(
    env: Env,
    proposer: Address,
    new_config: crate::types::Config,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    require_not_paused(&env)?;
    let cfg = config(&env);
    assert!(
        cfg.admins.iter().any(|a| a == proposer),
        "only admins can propose config changes"
    );

    crate::helpers::validate_admin_config(
        &env,
        &new_config.admins,
        new_config.admin_threshold,
        &new_config.admin_whitelist,
        &new_config.admin_blacklist,
    )?;

    let proposal_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::TimelockCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("proposal ID overflow");

    let eta = env.ledger().timestamp() + crate::types::CONFIG_TIMELOCK_SECONDS;

    let proposal = TimelockProposal {
        id: proposal_id,
        action: TimelockAction::SetConfig(new_config),
        proposer: proposer.clone(),
        eta,
        executed: false,
        cancelled: false,
    };

    env.storage()
        .instance()
        .set(&DataKey::Timelock(proposal_id), &proposal);
    env.storage()
        .instance()
        .set(&DataKey::TimelockCounter, &proposal_id);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("cfg_prop")),
        (proposal_id, proposer, eta),
    );

    Ok(proposal_id)
}

pub fn execute_config_change(env: Env, proposal_id: u64) -> Result<(), ContractError> {
    require_not_paused(&env)?;

    let mut proposal: TimelockProposal = env
        .storage()
        .instance()
        .get(&DataKey::Timelock(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.executed {
        return Err(ContractError::SlashAlreadyExecuted);
    }
    if proposal.cancelled {
        return Err(ContractError::ProposalNotFound);
    }

    if env.ledger().timestamp() < proposal.eta {
        return Err(ContractError::ProposalNotFound);
    }

    const TIMELOCK_EXPIRY: u64 = 72 * 60 * 60;
    if env.ledger().timestamp() > proposal.eta + TIMELOCK_EXPIRY {
        return Err(ContractError::ProposalNotFound);
    }

    if let TimelockAction::SetConfig(new_config) = &proposal.action {
        proposal.executed = true;
        env.storage()
            .instance()
            .set(&DataKey::Timelock(proposal_id), &proposal);

        env.storage()
            .instance()
            .set(&DataKey::Config, new_config);

        env.events().publish(
            (symbol_short!("gov"), symbol_short!("cfg_exec")),
            (proposal_id,),
        );

        Ok(())
    } else {
        Err(ContractError::ProposalNotFound)
    }
}

pub fn cancel_config_change(
    env: Env,
    admin_signers: Vec<Address>,
    proposal_id: u64,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    crate::helpers::require_admin_approval(&env, &admin_signers);

    let mut proposal: TimelockProposal = env
        .storage()
        .instance()
        .get(&DataKey::Timelock(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.executed || proposal.cancelled {
        return Err(ContractError::SlashAlreadyExecuted);
    }

    proposal.cancelled = true;
    env.storage()
        .instance()
        .set(&DataKey::Timelock(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("cfg_cncl")),
        (proposal_id,),
    );

    Ok(())
}
