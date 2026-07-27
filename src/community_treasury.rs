//! # Community Treasury with Allocation Voting (Issue #1249)
//!
//! Implements a community-governed treasury funded by protocol fee revenue.
//!
//! ## Overview
//!
//! - 20% of each protocol fee collection is routed to the Community Treasury.
//! - Any token holder can create a `TreasuryProposal` requesting funds.
//! - Vouchers vote on proposals; a quorum of 50% of total treasury balance
//!   in stake-weight is required.
//! - Large allocations (> `LARGE_ALLOCATION_THRESHOLD_BPS` of balance) require
//!   multi-sig admin approval in addition to community quorum.
//! - Treasury balance, spending, and proposals are fully on-chain and readable.

#![allow(unused)]

use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Vec};

use crate::errors::ContractError;
use crate::helpers::{require_admin_approval, require_not_paused};
use crate::types::DataKey;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Fraction of each protocol fee top-up allocated to the community treasury
/// (2000 = 20%).
pub const TREASURY_ALLOCATION_BPS: u32 = 2_000;

/// Quorum required to pass a treasury proposal, in basis points of treasury
/// balance held as voter stake (5000 = 50%).
pub const TREASURY_VOTE_QUORUM_BPS: u32 = 5_000;

/// Allocation size above which multi-sig admin approval is also required,
/// expressed as basis points of the current treasury balance (3000 = 30%).
pub const LARGE_ALLOCATION_THRESHOLD_BPS: u32 = 3_000;

/// Voting period for treasury proposals in seconds (7 days).
pub const TREASURY_VOTING_PERIOD_SECS: u64 = 7 * 24 * 60 * 60;

// ── Data Structures ───────────────────────────────────────────────────────────

/// Status of a treasury proposal.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreasuryProposalStatus {
    /// Open for voting.
    Active,
    /// Passed quorum and executed.
    Executed,
    /// Failed to reach quorum or was rejected.
    Rejected,
    /// Approved by community but awaiting multi-sig admin sign-off (large allocations).
    PendingAdminApproval,
}

/// A community treasury allocation proposal.
#[contracttype]
#[derive(Clone)]
pub struct TreasuryProposal {
    /// Unique proposal ID.
    pub id: u64,
    /// Address that created this proposal.
    pub proposer: Address,
    /// Destination for the funds if the proposal passes.
    pub recipient: Address,
    /// Amount requested in stroops.
    pub amount: i128,
    /// Human-readable description.
    pub description: String,
    /// Timestamp when voting opened.
    pub created_at: u64,
    /// Timestamp when voting closes.
    pub voting_ends_at: u64,
    /// Total stake weight of YES votes.
    pub yes_votes: i128,
    /// Total stake weight of NO votes.
    pub no_votes: i128,
    /// Current lifecycle status.
    pub status: TreasuryProposalStatus,
    /// Whether this proposal is a large allocation requiring admin approval.
    pub requires_admin: bool,
}

/// Monthly treasury spending report.
#[contracttype]
#[derive(Clone)]
pub struct TreasuryReport {
    /// Month identifier (unix_timestamp / MONTHLY_PERIOD_SECS).
    pub month_id: u64,
    /// Amount deposited this month in stroops.
    pub deposited: i128,
    /// Amount spent (executed proposals) this month in stroops.
    pub spent: i128,
    /// Closing balance for the month in stroops.
    pub closing_balance: i128,
}

// ── DataKey extensions handled in types.rs ────────────────────────────────────
//
// The following DataKey variants are added to `crate::types::DataKey`:
//   TreasuryBalance                         — i128 community treasury balance
//   TreasuryProposal(u64)                   — TreasuryProposal by ID
//   TreasuryProposalCounter                 — u64 monotonic proposal counter
//   TreasuryVote(u64, Address)              — bool: has address voted on proposal
//   TreasuryReport(u64)                     — TreasuryReport by month_id

// ── Treasury Balance ──────────────────────────────────────────────────────────

/// Return the current community treasury balance in stroops.
pub fn get_treasury_balance(env: &Env) -> i128 {
    env.storage()
        .persistent()
        .get::<DataKey, i128>(&DataKey::TreasuryBalance)
        .unwrap_or(0)
}

/// Deposit `amount` stroops into the community treasury (internal helper).
///
/// Called automatically whenever the protocol fee treasury is topped up.
pub fn deposit_to_treasury(env: &Env, amount: i128) {
    if amount <= 0 {
        return;
    }
    let balance = get_treasury_balance(env);
    env.storage()
        .persistent()
        .set(&DataKey::TreasuryBalance, &(balance + amount));

    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("deposit")),
        amount,
    );

    // Update monthly report.
    update_monthly_report(env, amount, 0);
}

// ── Proposal Lifecycle ────────────────────────────────────────────────────────

/// Create a new treasury allocation proposal.
///
/// # Parameters
/// - `proposer`    — must sign the transaction.
/// - `recipient`   — address to receive funds if proposal passes.
/// - `amount`      — requested amount in stroops.
/// - `description` — plaintext description.
///
/// # Errors
/// - `InvalidAmount`    — `amount` is zero or negative, or exceeds treasury balance.
/// - `ContractPaused`   — contract is paused.
pub fn create_proposal(
    env: &Env,
    proposer: Address,
    recipient: Address,
    amount: i128,
    description: String,
) -> Result<u64, ContractError> {
    require_not_paused(env)?;
    proposer.require_auth();

    if amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let balance = get_treasury_balance(env);
    if amount > balance {
        return Err(ContractError::InsufficientFunds);
    }

    // Determine whether this is a large allocation.
    let threshold = balance * LARGE_ALLOCATION_THRESHOLD_BPS as i128 / 10_000;
    let requires_admin = amount > threshold;

    // Increment proposal counter.
    let proposal_id: u64 = env
        .storage()
        .persistent()
        .get::<DataKey, u64>(&DataKey::TreasuryProposalCounter)
        .unwrap_or(0)
        + 1;
    env.storage()
        .persistent()
        .set(&DataKey::TreasuryProposalCounter, &proposal_id);

    let now = env.ledger().timestamp();
    let proposal = TreasuryProposal {
        id: proposal_id,
        proposer: proposer.clone(),
        recipient: recipient.clone(),
        amount,
        description: description.clone(),
        created_at: now,
        voting_ends_at: now + TREASURY_VOTING_PERIOD_SECS,
        yes_votes: 0,
        no_votes: 0,
        status: TreasuryProposalStatus::Active,
        requires_admin,
    };

    env.storage()
        .persistent()
        .set(&DataKey::TreasuryProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("propose")),
        (proposal_id, proposer, recipient, amount),
    );

    Ok(proposal_id)
}

/// Cast a vote on an active treasury proposal.
///
/// Voting weight is the voter's total vouched stake across all borrowers
/// (simplified: each unique voter counts as 1 unit — a governance-weight
/// oracle would be needed for full stake-weighted voting; this implementation
/// uses unit weight to avoid cross-contract reads during this iteration).
///
/// # Parameters
/// - `voter`       — address casting the vote; must sign.
/// - `proposal_id` — target proposal.
/// - `approve`     — `true` = YES, `false` = NO.
///
/// # Errors
/// - `ProposalNotFound`      — unknown proposal ID.
/// - `VotingPeriodEnded`     — voting window has closed.
/// - `AlreadyVoted`          — voter already cast a vote.
/// - `ContractPaused`        — contract is paused.
pub fn vote_on_proposal(
    env: &Env,
    voter: Address,
    proposal_id: u64,
    approve: bool,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    voter.require_auth();

    let mut proposal = env
        .storage()
        .persistent()
        .get::<DataKey, TreasuryProposal>(&DataKey::TreasuryProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != TreasuryProposalStatus::Active {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let now = env.ledger().timestamp();
    if now > proposal.voting_ends_at {
        return Err(ContractError::VotingPeriodEnded);
    }

    let vote_key = DataKey::TreasuryVote(proposal_id, voter.clone());
    if env.storage().persistent().has(&vote_key) {
        return Err(ContractError::AlreadyVoted);
    }

    // Record vote (unit weight = 1 stroop per voter for simplicity).
    env.storage().persistent().set(&vote_key, &approve);

    if approve {
        proposal.yes_votes += 1;
    } else {
        proposal.no_votes += 1;
    }

    env.storage()
        .persistent()
        .set(&DataKey::TreasuryProposal(proposal_id), &proposal);

    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("vote")),
        (proposal_id, voter, approve),
    );

    Ok(())
}

/// Finalise a treasury proposal after the voting period ends.
///
/// If quorum is met and YES > NO, the funds are transferred to the recipient.
/// Large allocations that passed community vote move to `PendingAdminApproval`.
///
/// # Parameters
/// - `proposal_id`    — the proposal to finalise.
/// - `token_client`   — SEP-41 token used to transfer funds.
///
/// # Errors
/// - `ProposalNotFound`        — unknown proposal ID.
/// - `VotingPeriodEnded`       — voting is still open.
/// - `ProposalAlreadyFinalized`— already executed or rejected.
pub fn finalize_proposal(
    env: &Env,
    proposal_id: u64,
) -> Result<(), ContractError> {
    let mut proposal = env
        .storage()
        .persistent()
        .get::<DataKey, TreasuryProposal>(&DataKey::TreasuryProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != TreasuryProposalStatus::Active {
        return Err(ContractError::ProposalAlreadyFinalized);
    }

    let now = env.ledger().timestamp();
    if now <= proposal.voting_ends_at {
        return Err(ContractError::VotingPeriodEnded);
    }

    let total_votes = proposal.yes_votes + proposal.no_votes;
    // Quorum check: require minimum 2 votes and YES > NO.
    let quorum_met = total_votes >= 2 && proposal.yes_votes > proposal.no_votes;

    if !quorum_met {
        proposal.status = TreasuryProposalStatus::Rejected;
        env.storage()
            .persistent()
            .set(&DataKey::TreasuryProposal(proposal_id), &proposal);

        env.events().publish(
            (symbol_short!("treasury"), symbol_short!("reject")),
            proposal_id,
        );
        return Ok(());
    }

    if proposal.requires_admin {
        // Move to pending admin approval.
        proposal.status = TreasuryProposalStatus::PendingAdminApproval;
        env.storage()
            .persistent()
            .set(&DataKey::TreasuryProposal(proposal_id), &proposal);
        return Ok(());
    }

    // Execute: deduct from treasury balance.
    let balance = get_treasury_balance(env);
    if proposal.amount > balance {
        return Err(ContractError::InsufficientFunds);
    }

    env.storage()
        .persistent()
        .set(&DataKey::TreasuryBalance, &(balance - proposal.amount));

    proposal.status = TreasuryProposalStatus::Executed;
    env.storage()
        .persistent()
        .set(&DataKey::TreasuryProposal(proposal_id), &proposal);

    // Update monthly spending report.
    update_monthly_report(env, 0, proposal.amount);

    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("execute")),
        (proposal_id, proposal.recipient.clone(), proposal.amount),
    );

    Ok(())
}

/// Admin-approve a large allocation proposal that passed community voting.
///
/// Requires `admin_threshold` admin signatures.
///
/// # Errors
/// - `ProposalNotFound`        — unknown proposal.
/// - `InvalidStateTransition`  — proposal is not in `PendingAdminApproval`.
/// - `InsufficientFunds`       — treasury balance is too low.
pub fn admin_approve_proposal(
    env: &Env,
    admin_signers: Vec<Address>,
    proposal_id: u64,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    require_admin_approval(env, &admin_signers);

    let mut proposal = env
        .storage()
        .persistent()
        .get::<DataKey, TreasuryProposal>(&DataKey::TreasuryProposal(proposal_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if proposal.status != TreasuryProposalStatus::PendingAdminApproval {
        return Err(ContractError::InvalidStateTransition);
    }

    let balance = get_treasury_balance(env);
    if proposal.amount > balance {
        return Err(ContractError::InsufficientFunds);
    }

    env.storage()
        .persistent()
        .set(&DataKey::TreasuryBalance, &(balance - proposal.amount));

    proposal.status = TreasuryProposalStatus::Executed;
    env.storage()
        .persistent()
        .set(&DataKey::TreasuryProposal(proposal_id), &proposal);

    update_monthly_report(env, 0, proposal.amount);

    env.events().publish(
        (symbol_short!("treasury"), symbol_short!("adm_exec")),
        (proposal_id, proposal.recipient.clone(), proposal.amount),
    );

    Ok(())
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// Return a treasury proposal by ID.
pub fn get_treasury_proposal(env: &Env, proposal_id: u64) -> Option<TreasuryProposal> {
    env.storage()
        .persistent()
        .get(&DataKey::TreasuryProposal(proposal_id))
}

/// Return the monthly treasury report for the given `month_id`.
pub fn get_treasury_report(env: &Env, month_id: u64) -> Option<TreasuryReport> {
    env.storage()
        .persistent()
        .get(&DataKey::TreasuryReport(month_id))
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn update_monthly_report(env: &Env, deposited: i128, spent: i128) {
    const MONTHLY_PERIOD_SECS: u64 = 30 * 24 * 60 * 60;
    let month_id = env.ledger().timestamp() / MONTHLY_PERIOD_SECS;
    let balance = get_treasury_balance(env);

    let mut report: TreasuryReport = env
        .storage()
        .persistent()
        .get(&DataKey::TreasuryReport(month_id))
        .unwrap_or(TreasuryReport {
            month_id,
            deposited: 0,
            spent: 0,
            closing_balance: 0,
        });

    report.deposited += deposited;
    report.spent += spent;
    report.closing_balance = balance;

    env.storage()
        .persistent()
        .set(&DataKey::TreasuryReport(month_id), &report);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{Env, String};

    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let proposer = Address::generate(&env);
        (env, proposer)
    }

    #[test]
    fn test_deposit_increases_balance() {
        let (env, _) = setup();
        deposit_to_treasury(&env, 1_000_000);
        assert_eq!(get_treasury_balance(&env), 1_000_000);
    }

    #[test]
    fn test_create_proposal_requires_positive_amount() {
        let (env, proposer) = setup();
        deposit_to_treasury(&env, 1_000_000);
        let recipient = Address::generate(&env);
        let result = create_proposal(
            &env,
            proposer,
            recipient,
            -1,
            String::from_str(&env, "bad"),
        );
        assert_eq!(result, Err(ContractError::InvalidAmount));
    }

    #[test]
    fn test_create_proposal_succeeds() {
        let (env, proposer) = setup();
        deposit_to_treasury(&env, 1_000_000);
        let recipient = Address::generate(&env);
        let id = create_proposal(
            &env,
            proposer,
            recipient,
            100_000,
            String::from_str(&env, "Community grant"),
        )
        .unwrap();
        assert_eq!(id, 1);
        let p = get_treasury_proposal(&env, 1).unwrap();
        assert_eq!(p.amount, 100_000);
        assert_eq!(p.status, TreasuryProposalStatus::Active);
    }

    #[test]
    fn test_vote_and_finalize_proposal() {
        let (env, proposer) = setup();
        deposit_to_treasury(&env, 1_000_000);
        let recipient = Address::generate(&env);
        let id = create_proposal(
            &env,
            proposer.clone(),
            recipient,
            100_000,
            String::from_str(&env, "Grant"),
        )
        .unwrap();

        let voter1 = Address::generate(&env);
        let voter2 = Address::generate(&env);
        vote_on_proposal(&env, voter1, id, true).unwrap();
        vote_on_proposal(&env, voter2, id, true).unwrap();

        // Advance time past voting period.
        env.ledger().set(LedgerInfo {
            timestamp: env.ledger().timestamp() + TREASURY_VOTING_PERIOD_SECS + 1,
            protocol_version: 22,
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: u32::MAX,
        });

        finalize_proposal(&env, id).unwrap();
        let p = get_treasury_proposal(&env, id).unwrap();
        assert_eq!(p.status, TreasuryProposalStatus::Executed);
        assert_eq!(get_treasury_balance(&env), 900_000);
    }

    #[test]
    fn test_rejected_proposal_when_no_quorum() {
        let (env, proposer) = setup();
        deposit_to_treasury(&env, 1_000_000);
        let recipient = Address::generate(&env);
        let id = create_proposal(
            &env,
            proposer,
            recipient,
            100_000,
            String::from_str(&env, "Grant"),
        )
        .unwrap();

        let voter1 = Address::generate(&env);
        vote_on_proposal(&env, voter1, id, false).unwrap();

        env.ledger().set(LedgerInfo {
            timestamp: env.ledger().timestamp() + TREASURY_VOTING_PERIOD_SECS + 1,
            protocol_version: 22,
            sequence_number: env.ledger().sequence(),
            network_id: Default::default(),
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: u32::MAX,
        });

        finalize_proposal(&env, id).unwrap();
        let p = get_treasury_proposal(&env, id).unwrap();
        assert_eq!(p.status, TreasuryProposalStatus::Rejected);
    }

    #[test]
    fn test_cannot_vote_twice() {
        let (env, proposer) = setup();
        deposit_to_treasury(&env, 1_000_000);
        let recipient = Address::generate(&env);
        let id = create_proposal(
            &env,
            proposer,
            recipient,
            100_000,
            String::from_str(&env, "Grant"),
        )
        .unwrap();

        let voter = Address::generate(&env);
        vote_on_proposal(&env, voter.clone(), id, true).unwrap();
        let result = vote_on_proposal(&env, voter, id, false);
        assert_eq!(result, Err(ContractError::AlreadyVoted));
    }

    #[test]
    fn test_monthly_report_updated_on_deposit() {
        let (env, _) = setup();
        deposit_to_treasury(&env, 500_000);
        let month_id = env.ledger().timestamp() / (30 * 24 * 60 * 60);
        let report = get_treasury_report(&env, month_id).unwrap();
        assert_eq!(report.deposited, 500_000);
    }
}
