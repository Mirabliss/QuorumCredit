//! # Issue #1241 — Governance Token with DAO Voting
//!
//! Implements a GOV token system that enables decentralized protocol decisions:
//!
//! - GOV tokens are minted by admins and transferred between holders.
//! - 1 GOV token = 1 vote.
//! - Voting power can be delegated to another address.
//! - Creating a proposal requires holding ≥ 1% of total GOV supply.
//! - Voting uses token-weighted votes; quorum requires ≥ 10% of supply to participate.
//! - Governance participation metrics are tracked on-chain.

use crate::errors::ContractError;
use crate::helpers::{require_admin_approval, require_not_paused};
use crate::types::{
    DaoProposal, DaoProposalStatus, DataKey, GovDelegation, GovParticipationMetrics,
    GovTokenBalance, DAO_TIMELOCK_SECS, DAO_VOTING_PERIOD_SECS, GOV_BPS_DENOMINATOR,
    GOV_PROPOSAL_THRESHOLD_BPS, GOV_QUORUM_BPS,
};
use soroban_sdk::{symbol_short, Address, Env, String, Vec};

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Load governance participation metrics, defaulting to zero if not set.
fn load_metrics(env: &Env) -> GovParticipationMetrics {
    env.storage()
        .instance()
        .get(&DataKey::GovParticipationMetrics)
        .unwrap_or(GovParticipationMetrics {
            total_supply: 0,
            proposals_created: 0,
            total_votes_cast: 0,
            unique_voters: 0,
        })
}

/// Save governance participation metrics.
fn save_metrics(env: &Env, metrics: &GovParticipationMetrics) {
    env.storage()
        .instance()
        .set(&DataKey::GovParticipationMetrics, metrics);
}

/// Load a holder's GOV token balance, defaulting to zero.
fn load_balance(env: &Env, holder: &Address) -> GovTokenBalance {
    env.storage()
        .persistent()
        .get(&DataKey::GovTokenBalance(holder.clone()))
        .unwrap_or(GovTokenBalance {
            holder: holder.clone(),
            balance: 0,
            first_received_at: 0,
            votes_cast: 0,
        })
}

/// Load the next DAO proposal ID (auto-increment).
fn next_proposal_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::DaoProposalCounter)
        .unwrap_or(0u64);
    let next = id + 1;
    env.storage()
        .instance()
        .set(&DataKey::DaoProposalCounter, &next);
    id
}

/// Resolve the effective voter for a holder, following a single delegation hop.
/// Circular delegation is prevented by limiting to one hop.
fn resolve_delegate(env: &Env, holder: &Address) -> Address {
    if let Some(delegation) = env
        .storage()
        .persistent()
        .get::<DataKey, GovDelegation>(&DataKey::GovDelegation(holder.clone()))
    {
        // Only follow if the delegate hasn't further delegated back to original
        if delegation.delegate != *holder {
            return delegation.delegate;
        }
    }
    holder.clone()
}

// ── Public functions ──────────────────────────────────────────────────────────

/// Mint GOV tokens to a recipient. Admin-only.
///
/// Emits event: `gov/mint` with `(recipient, amount)`.
pub fn mint_gov_tokens(
    env: Env,
    admin_signers: Vec<Address>,
    recipient: Address,
    amount: i128,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    require_admin_approval(&env, &admin_signers);

    if amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let mut bal = load_balance(&env, &recipient);
    if bal.balance == 0 {
        bal.first_received_at = env.ledger().timestamp();
    }
    bal.balance += amount;
    env.storage()
        .persistent()
        .set(&DataKey::GovTokenBalance(recipient.clone()), &bal);

    let mut metrics = load_metrics(&env);
    metrics.total_supply += amount;
    save_metrics(&env, &metrics);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("mint")),
        (recipient, amount),
    );

    Ok(())
}

/// Transfer GOV tokens from one holder to another.
///
/// Emits event: `gov/transfer` with `(from, to, amount)`.
pub fn transfer_gov_tokens(
    env: Env,
    from: Address,
    to: Address,
    amount: i128,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    from.require_auth();

    if amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let mut from_bal = load_balance(&env, &from);
    if from_bal.balance < amount {
        return Err(ContractError::InsufficientFunds);
    }
    from_bal.balance -= amount;
    env.storage()
        .persistent()
        .set(&DataKey::GovTokenBalance(from.clone()), &from_bal);

    let mut to_bal = load_balance(&env, &to);
    if to_bal.balance == 0 {
        to_bal.first_received_at = env.ledger().timestamp();
    }
    to_bal.balance += amount;
    env.storage()
        .persistent()
        .set(&DataKey::GovTokenBalance(to.clone()), &to_bal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("xfer")),
        (from, to, amount),
    );

    Ok(())
}

/// Delegate voting power to another address.
///
/// The delegator's tokens remain in their wallet; the delegate can vote on their behalf.
/// Pass `delegate == delegator` to revoke an existing delegation.
///
/// Emits event: `gov/delegate` with `(delegator, delegate)`.
pub fn delegate_gov_vote(
    env: Env,
    delegator: Address,
    delegate: Address,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    delegator.require_auth();

    // Prevent circular delegation: check that `delegate` hasn't already delegated back.
    if delegate != delegator {
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, GovDelegation>(&DataKey::GovDelegation(delegate.clone()))
        {
            if existing.delegate == delegator {
                return Err(ContractError::CircularDelegation);
            }
        }
    }

    if delegate == delegator {
        // Revoke delegation
        env.storage()
            .persistent()
            .remove(&DataKey::GovDelegation(delegator.clone()));
    } else {
        let delegation = GovDelegation {
            delegator: delegator.clone(),
            delegate: delegate.clone(),
            set_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::GovDelegation(delegator.clone()), &delegation);
    }

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("deleg")),
        (delegator, delegate),
    );

    Ok(())
}

/// Create a new DAO governance proposal.
///
/// Requirements:
/// - Proposer must hold ≥ 1% of total GOV supply.
/// - `description` must not be empty.
///
/// Returns the proposal ID.
///
/// Emits event: `gov/propose` with `(proposer, proposal_id, description)`.
pub fn create_dao_proposal(
    env: Env,
    proposer: Address,
    description: String,
) -> Result<u64, ContractError> {
    require_not_paused(&env)?;
    proposer.require_auth();

    let metrics = load_metrics(&env);
    if metrics.total_supply == 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // 1% of total supply threshold
    let threshold = metrics.total_supply * GOV_PROPOSAL_THRESHOLD_BPS / GOV_BPS_DENOMINATOR;
    let proposer_bal = load_balance(&env, &proposer);
    if proposer_bal.balance < threshold {
        return Err(ContractError::InsufficientFunds);
    }

    let now = env.ledger().timestamp();
    let proposal_id = next_proposal_id(&env);

    let proposal = DaoProposal {
        id: proposal_id,
        proposer: proposer.clone(),
        description: description.clone(),
        votes_for: 0,
        votes_against: 0,
        voters: Vec::new(&env),
        status: DaoProposalStatus::Active,
        created_at: now,
        voting_ends_at: now + DAO_VOTING_PERIOD_SECS,
        executable_at: now + DAO_VOTING_PERIOD_SECS + DAO_TIMELOCK_SECS,
    };

    env.storage()
        .persistent()
        .set(&DataKey::DaoProposal(proposal_id), &proposal);

    // Update metrics
    let mut updated_metrics = metrics;
    updated_metrics.proposals_created += 1;
    save_metrics(&env, &updated_metrics);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("propose")),
        (proposer, proposal_id, description),
    );

    Ok(proposal_id)
}

/// Cast a vote on an active DAO proposal.
///
/// - `vote_for`: true = vote FOR, false = vote AGAINST.
/// - Voting weight is the effective voter's GOV balance (respects delegation).
/// - Each address may vote only once per proposal.
///
/// When total participation ≥ quorum and majority votes FOR, proposal is set to Passed.
/// When total participation ≥ quorum and majority votes AGAINST, proposal is set to Failed.
///
/// Emits event: `gov/vote` with `(voter, proposal_id, vote_for, weight)`.
pub fn vote_on_proposal(
    env: Env,
    voter: Address,
    proposal_id: u64,
    vote_for: bool,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    voter.require_auth();

    let mut proposal: DaoProposal = env
        .storage()
        .persistent()
        .get(&DataKey::DaoProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != DaoProposalStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    let now = env.ledger().timestamp();
    if now > proposal.voting_ends_at {
        // Auto-finalise expired proposal before rejecting the vote
        finalize_proposal_status(&env, &mut proposal);
        env.storage()
            .persistent()
            .set(&DataKey::DaoProposal(proposal_id), &proposal);
        return Err(ContractError::VotingPeriodEnded);
    }

    // Resolve effective voter (delegation)
    let effective_voter = resolve_delegate(&env, &voter);

    // Check for duplicate vote (both original and effective voter)
    for existing in proposal.voters.iter() {
        if existing == voter || existing == effective_voter {
            return Err(ContractError::AlreadyVoted);
        }
    }

    // Voting weight = effective voter's GOV balance
    let weight = load_balance(&env, &effective_voter).balance;
    if weight == 0 {
        return Err(ContractError::InsufficientFunds);
    }

    if vote_for {
        proposal.votes_for += weight;
    } else {
        proposal.votes_against += weight;
    }
    proposal.voters.push_back(voter.clone());

    // Check if quorum is met and auto-finalise
    let metrics = load_metrics(&env);
    let total_votes = proposal.votes_for + proposal.votes_against;
    let quorum_required = metrics.total_supply * GOV_QUORUM_BPS / GOV_BPS_DENOMINATOR;

    if total_votes >= quorum_required {
        finalize_proposal_status(&env, &mut proposal);
    }

    env.storage()
        .persistent()
        .set(&DataKey::DaoProposal(proposal_id), &proposal);

    // Update participation metrics
    let mut updated_metrics = metrics;
    updated_metrics.total_votes_cast += 1;
    save_metrics(&env, &updated_metrics);

    // Update voter's votes_cast counter
    let mut voter_bal = load_balance(&env, &voter);
    voter_bal.votes_cast += 1;
    env.storage()
        .persistent()
        .set(&DataKey::GovTokenBalance(voter.clone()), &voter_bal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("vote")),
        (voter, proposal_id, vote_for, weight),
    );

    Ok(())
}

/// Finalise a DAO proposal after the voting period ends.
///
/// Anyone may call this once `voting_ends_at` has passed to update the status.
/// Auto-execution happens when quorum is met and majority voted FOR, after the timelock.
///
/// Emits event: `gov/finalize` with `(proposal_id, status)`.
pub fn finalize_dao_proposal(
    env: Env,
    proposal_id: u64,
) -> Result<DaoProposalStatus, ContractError> {
    let mut proposal: DaoProposal = env
        .storage()
        .persistent()
        .get(&DataKey::DaoProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != DaoProposalStatus::Active {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let now = env.ledger().timestamp();
    if now <= proposal.voting_ends_at {
        return Err(ContractError::VotingPeriodEnded);
    }

    finalize_proposal_status(&env, &mut proposal);
    let status = proposal.status.clone();

    env.storage()
        .persistent()
        .set(&DataKey::DaoProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("gov"), symbol_short!("final")),
        (proposal_id, status.clone()),
    );

    Ok(status)
}

/// Internal helper to determine and set final proposal status based on votes.
fn finalize_proposal_status(env: &Env, proposal: &mut DaoProposal) {
    let metrics = load_metrics(env);
    let total_votes = proposal.votes_for + proposal.votes_against;
    let quorum_required = metrics.total_supply * GOV_QUORUM_BPS / GOV_BPS_DENOMINATOR;

    if total_votes < quorum_required {
        // Quorum not met → Failed
        proposal.status = DaoProposalStatus::Failed;
    } else if proposal.votes_for > proposal.votes_against {
        proposal.status = DaoProposalStatus::Passed;
    } else {
        proposal.status = DaoProposalStatus::Failed;
    }
}

/// Get a holder's GOV token balance.
pub fn get_gov_balance(env: Env, holder: Address) -> GovTokenBalance {
    load_balance(&env, &holder)
}

/// Get a DAO proposal by ID.
pub fn get_dao_proposal(env: Env, proposal_id: u64) -> Option<DaoProposal> {
    env.storage()
        .persistent()
        .get(&DataKey::DaoProposal(proposal_id))
}

/// Get governance participation metrics.
pub fn get_gov_metrics(env: Env) -> GovParticipationMetrics {
    load_metrics(&env)
}

/// Get the vote delegation for a holder (if any).
pub fn get_gov_delegation(env: Env, delegator: Address) -> Option<GovDelegation> {
    env.storage()
        .persistent()
        .get(&DataKey::GovDelegation(delegator))
}
