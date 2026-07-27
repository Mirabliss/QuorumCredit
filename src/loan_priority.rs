/// Issue: Loan Priority / Subordination (senior-junior debt structures).
///
/// Today every loan shares equal claim on default proceeds. This module adds a
/// priority queue so loans can be tagged Senior / Mezzanine / Junior, and default
/// proceeds are routed through a waterfall: Senior is made whole first, then
/// Mezzanine, then Junior absorbs any shortfall.
use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers;

/// Priority tier for a loan. Lower-numbered tiers are paid first in a default
/// waterfall.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoanPriority {
    Senior,
    Mezzanine,
    Junior,
}

impl LoanPriority {
    /// Sort rank used to order the priority queue (0 = paid first).
    fn rank(&self) -> u32 {
        match self {
            LoanPriority::Senior => 0,
            LoanPriority::Mezzanine => 1,
            LoanPriority::Junior => 2,
        }
    }
}

/// A single entry in the priority queue: a loan tagged with its tranche.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriorityLoanEntry {
    pub loan_id: u64,
    pub borrower: Address,
    pub priority: LoanPriority,
    pub amount: i128,
}

/// The full ordered priority queue, persisted so waterfall routing can be
/// replayed deterministically.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriorityQueue {
    pub entries: Vec<PriorityLoanEntry>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// One line of the default waterfall distribution record: how much of the
/// total recovered proceeds a given loan/tranche actually received.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaterfallDistributionEntry {
    pub loan_id: u64,
    pub borrower: Address,
    pub priority: LoanPriority,
    pub owed: i128,
    pub paid: i128,
}

/// A full record of one waterfall run, for audit purposes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaterfallRun {
    pub run_id: u64,
    pub total_proceeds: i128,
    pub distributed: i128,
    pub shortfall: i128,
    pub timestamp: u64,
    pub entries: Vec<WaterfallDistributionEntry>,
}

/// A pending governance proposal to change a loan's priority tier.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriorityChangeProposal {
    pub proposal_id: u64,
    pub loan_id: u64,
    pub new_priority: LoanPriority,
    pub proposer: Address,
    pub approvals: Vec<Address>,
    pub created_at: u64,
    pub executed: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum PriorityDataKey {
    Queue,
    WaterfallRunCounter,
    WaterfallRun(u64),
    ProposalCounter,
    Proposal(u64),
}

/// Build (or replace) the loan priority queue from a caller-supplied set of
/// (loan, tranche) tuples. The queue is stored sorted Senior → Mezzanine →
/// Junior so waterfall routing can walk it in order.
///
/// Task: `create_loan_priority_queue(env, loans: Vec<PriorityLoanEntry>)`.
pub fn create_loan_priority_queue(
    env: Env,
    admin_signers: Vec<Address>,
    loans: Vec<PriorityLoanEntry>,
) -> Result<(), ContractError> {
    helpers::require_admin_approval(&env, &admin_signers);

    if loans.is_empty() {
        return Err(ContractError::InvalidAmount);
    }

    // Simple insertion sort by tranche rank; queues are small (per-loan-pool
    // scale) so O(n^2) is fine and keeps this dependency-free under no_std.
    let mut sorted: Vec<PriorityLoanEntry> = Vec::new(&env);
    for entry in loans.iter() {
        let mut insert_at = sorted.len();
        for i in 0..sorted.len() {
            if sorted.get(i).unwrap().priority.rank() > entry.priority.rank() {
                insert_at = i;
                break;
            }
        }
        sorted.insert(insert_at, entry.clone());
    }

    let now = env.ledger().timestamp();
    let queue = PriorityQueue {
        entries: sorted,
        created_at: now,
        updated_at: now,
    };
    env.storage()
        .persistent()
        .set(&PriorityDataKey::Queue, &queue);

    env.events().publish(
        (symbol_short!("prio"), symbol_short!("queue_set")),
        (loans.len() as u32, now),
    );

    Ok(())
}

/// Read the current priority queue.
pub fn get_loan_priority_queue(env: Env) -> Vec<PriorityLoanEntry> {
    env.storage()
        .persistent()
        .get::<PriorityDataKey, PriorityQueue>(&PriorityDataKey::Queue)
        .map(|q| q.entries)
        .unwrap_or(Vec::new(&env))
}

/// Route recovered default proceeds through the Senior → Mezzanine → Junior
/// waterfall. Senior loans are made whole first (up to `amount` owed), any
/// remainder flows to Mezzanine, and Junior absorbs whatever is left — which
/// may be zero in a severe default. Returns the full distribution record and
/// persists it for later audit/read-back via `get_waterfall_run`.
pub fn route_default_proceeds(
    env: Env,
    admin_signers: Vec<Address>,
    total_proceeds: i128,
) -> Result<WaterfallRun, ContractError> {
    helpers::require_admin_approval(&env, &admin_signers);

    if total_proceeds < 0 {
        return Err(ContractError::InvalidAmount);
    }

    let queue = env
        .storage()
        .persistent()
        .get::<PriorityDataKey, PriorityQueue>(&PriorityDataKey::Queue)
        .ok_or(ContractError::InvalidAmount)?;

    let mut remaining = total_proceeds;
    let mut entries: Vec<WaterfallDistributionEntry> = Vec::new(&env);
    let mut distributed: i128 = 0;

    for e in queue.entries.iter() {
        let paid = if remaining >= e.amount {
            e.amount
        } else if remaining > 0 {
            remaining
        } else {
            0
        };
        remaining -= paid;
        distributed += paid;

        entries.push_back(WaterfallDistributionEntry {
            loan_id: e.loan_id,
            borrower: e.borrower.clone(),
            priority: e.priority.clone(),
            owed: e.amount,
            paid,
        });
    }

    let run_id: u64 = env
        .storage()
        .persistent()
        .get(&PriorityDataKey::WaterfallRunCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .ok_or(ContractError::ArithmeticError)?;
    env.storage()
        .persistent()
        .set(&PriorityDataKey::WaterfallRunCounter, &run_id);

    let run = WaterfallRun {
        run_id,
        total_proceeds,
        distributed,
        shortfall: total_proceeds - distributed,
        timestamp: env.ledger().timestamp(),
        entries,
    };
    env.storage()
        .persistent()
        .set(&PriorityDataKey::WaterfallRun(run_id), &run);

    env.events().publish(
        (symbol_short!("prio"), symbol_short!("waterfl")),
        (run_id, total_proceeds, distributed),
    );

    Ok(run)
}

/// Read back a previously executed waterfall distribution run.
pub fn get_waterfall_run(env: Env, run_id: u64) -> Option<WaterfallRun> {
    env.storage()
        .persistent()
        .get(&PriorityDataKey::WaterfallRun(run_id))
}

/// Propose a change to a loan's priority tier. Requires the same multi-admin
/// approval threshold as other governance actions; the proposer is recorded
/// as the first approval.
pub fn propose_priority_change(
    env: Env,
    proposer: Address,
    loan_id: u64,
    new_priority: LoanPriority,
) -> Result<u64, ContractError> {
    proposer.require_auth();
    if !helpers::is_admin(&env, &proposer) {
        return Err(ContractError::UnauthorizedCaller);
    }

    let proposal_id: u64 = env
        .storage()
        .persistent()
        .get(&PriorityDataKey::ProposalCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .ok_or(ContractError::ArithmeticError)?;
    env.storage()
        .persistent()
        .set(&PriorityDataKey::ProposalCounter, &proposal_id);

    let mut approvals = Vec::new(&env);
    approvals.push_back(proposer.clone());

    let proposal = PriorityChangeProposal {
        proposal_id,
        loan_id,
        new_priority,
        proposer,
        approvals,
        created_at: env.ledger().timestamp(),
        executed: false,
    };
    env.storage()
        .persistent()
        .set(&PriorityDataKey::Proposal(proposal_id), &proposal);

    Ok(proposal_id)
}

/// Approve a pending priority-change proposal. Once the number of distinct
/// admin approvals meets the contract's admin threshold, the change is
/// applied to the priority queue (the loan is re-tagged and the queue
/// re-sorted).
pub fn approve_priority_change(
    env: Env,
    approver: Address,
    proposal_id: u64,
) -> Result<bool, ContractError> {
    approver.require_auth();
    if !helpers::is_admin(&env, &approver) {
        return Err(ContractError::UnauthorizedCaller);
    }

    let mut proposal: PriorityChangeProposal = env
        .storage()
        .persistent()
        .get(&PriorityDataKey::Proposal(proposal_id))
        .ok_or(ContractError::InvalidAmount)?;

    if proposal.executed {
        return Err(ContractError::InvalidAmount);
    }
    if !proposal.approvals.iter().any(|a| a == approver) {
        proposal.approvals.push_back(approver);
    }

    let cfg = helpers::config(&env);
    let threshold_met = proposal.approvals.len() >= cfg.admin_threshold;

    if threshold_met {
        let mut queue = env
            .storage()
            .persistent()
            .get::<PriorityDataKey, PriorityQueue>(&PriorityDataKey::Queue)
            .ok_or(ContractError::InvalidAmount)?;

        let mut updated: Vec<PriorityLoanEntry> = Vec::new(&env);
        for e in queue.entries.iter() {
            if e.loan_id == proposal.loan_id {
                updated.push_back(PriorityLoanEntry {
                    loan_id: e.loan_id,
                    borrower: e.borrower.clone(),
                    priority: proposal.new_priority.clone(),
                    amount: e.amount,
                });
            } else {
                updated.push_back(e.clone());
            }
        }

        // Re-sort after the tranche change.
        let mut sorted: Vec<PriorityLoanEntry> = Vec::new(&env);
        for entry in updated.iter() {
            let mut insert_at = sorted.len();
            for i in 0..sorted.len() {
                if sorted.get(i).unwrap().priority.rank() > entry.priority.rank() {
                    insert_at = i;
                    break;
                }
            }
            sorted.insert(insert_at, entry.clone());
        }

        queue.entries = sorted;
        queue.updated_at = env.ledger().timestamp();
        env.storage()
            .persistent()
            .set(&PriorityDataKey::Queue, &queue);

        proposal.executed = true;
    }

    env.storage()
        .persistent()
        .set(&PriorityDataKey::Proposal(proposal_id), &proposal);

    Ok(proposal.executed)
}

/// Read a priority-change governance proposal.
pub fn get_priority_change_proposal(env: Env, proposal_id: u64) -> Option<PriorityChangeProposal> {
    env.storage()
        .persistent()
        .get(&PriorityDataKey::Proposal(proposal_id))
}
