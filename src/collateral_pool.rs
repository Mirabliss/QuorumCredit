/// Issue #1281: Real persistent storage for the cross-collateral pool subsystem.
///
/// All eight public entry-points previously returned stub values (pool id 0,
/// empty records, zero stake). This module now:
///   - persists CollateralPool records under `DataKey::CollateralPool(pool_id)`
///   - maintains a monotonically-increasing `DataKey::CollateralPoolCounter`
///   - maps each (member, pool) pair back via `DataKey::BorrowerPool(member, pool_id)`
///   - transfers tokens in/out of the contract following the same pattern used
///     by `vouch.rs` (balance-delta verification)
///   - enforces basic invariants (pool exists, pool is/isn't active, member checks)
use soroban_sdk::{Address, Env, Vec, token};
use crate::errors::ContractError;
use crate::types::{CollateralPool, DataKey};
use crate::helpers::{require_admin_approval, require_allowed_token};

// ── helpers ────────────────────────────────────────────────────────────────────

fn load_pool(env: &Env, pool_id: u64) -> Result<CollateralPool, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::CollateralPool(pool_id))
        .ok_or(ContractError::CollateralPoolNotFound)
}

fn save_pool(env: &Env, pool: &CollateralPool) {
    env.storage()
        .persistent()
        .set(&DataKey::CollateralPool(pool.pool_id), pool);
}

fn next_pool_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::CollateralPoolCounter)
        .unwrap_or(0u64);
    let next = id + 1;
    env.storage()
        .persistent()
        .set(&DataKey::CollateralPoolCounter, &next);
    next
}

fn member_index(pool: &CollateralPool, member: &Address) -> Option<usize> {
    for (i, m) in pool.members.iter().enumerate() {
        if m == *member {
            return Some(i);
        }
    }
    None
}

// ── public functions ───────────────────────────────────────────────────────────

/// Issue #1281: Create a new cross-collateral pool seeded with the creator's
/// initial stake. The token must be an allowed protocol token.
pub fn create_pool(
    env: Env,
    creator: Address,
    token: Address,
    initial_stake: i128,
) -> Result<u64, ContractError> {
    creator.require_auth();

    if initial_stake <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    let token_client = require_allowed_token(&env, &token)?;

    let pool_id = next_pool_id(&env);
    let contract = env.current_contract_address();

    // Transfer initial stake into the contract (balance-delta verification).
    let before = token_client.balance(&contract);
    token_client.transfer(&creator, &contract, &initial_stake);
    let after = token_client.balance(&contract);
    let received = after
        .checked_sub(before)
        .ok_or(ContractError::StakeOverflow)?;
    if received != initial_stake {
        return Err(ContractError::InsufficientFunds);
    }

    let mut members = Vec::new(&env);
    let mut stakes: Vec<i128> = Vec::new(&env);
    let mut chain_ids: Vec<u32> = Vec::new(&env);

    members.push_back(creator.clone());
    stakes.push_back(initial_stake);
    chain_ids.push_back(0u32); // native chain

    let pool = CollateralPool {
        pool_id,
        members,
        stakes,
        chain_ids,
        token,
        borrower: None,
        active: false,
        created_at: env.ledger().timestamp(),
    };
    save_pool(&env, &pool);

    // Record reverse mapping so callers can enumerate their pools.
    env.storage()
        .persistent()
        .set(&DataKey::BorrowerPool(creator, pool_id), &true);

    Ok(pool_id)
}

/// Issue #1281: Join an existing, **inactive** collateral pool.
pub fn join_pool(
    env: Env,
    voucher: Address,
    pool_id: u64,
    stake: i128,
) -> Result<(), ContractError> {
    voucher.require_auth();

    if stake <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    let mut pool = load_pool(&env, pool_id)?;

    if pool.active {
        return Err(ContractError::CollateralPoolActive);
    }

    // Reject duplicate membership.
    if member_index(&pool, &voucher).is_some() {
        return Err(ContractError::DuplicateVouch);
    }

    let token_client = token::Client::new(&env, &pool.token);
    let contract = env.current_contract_address();

    let before = token_client.balance(&contract);
    token_client.transfer(&voucher, &contract, &stake);
    let after = token_client.balance(&contract);
    let received = after
        .checked_sub(before)
        .ok_or(ContractError::StakeOverflow)?;
    if received != stake {
        return Err(ContractError::InsufficientFunds);
    }

    pool.members.push_back(voucher.clone());
    pool.stakes.push_back(stake);
    pool.chain_ids.push_back(0u32);
    save_pool(&env, &pool);

    env.storage()
        .persistent()
        .set(&DataKey::BorrowerPool(voucher, pool_id), &true);

    Ok(())
}

/// Issue #1281 / #966: Join an existing, **inactive** pool from another chain.
/// The voucher must already be bridge-validated for `chain_id`.
pub fn join_pool_cross_chain(
    env: Env,
    voucher: Address,
    pool_id: u64,
    stake: i128,
    chain_id: u32,
) -> Result<(), ContractError> {
    voucher.require_auth();

    if stake <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // Verify bridge validation for this chain.
    let validated: bool = env
        .storage()
        .persistent()
        .get(&DataKey::BridgeValidated(voucher.clone(), chain_id))
        .unwrap_or(false);
    if !validated {
        return Err(ContractError::InvalidChain);
    }

    let mut pool = load_pool(&env, pool_id)?;

    if pool.active {
        return Err(ContractError::CollateralPoolActive);
    }

    if member_index(&pool, &voucher).is_some() {
        return Err(ContractError::DuplicateVouch);
    }

    // Cross-chain stake is represented as a signed ledger entry; no on-chain
    // token transfer occurs here (the originating chain already locked the funds).
    pool.members.push_back(voucher.clone());
    pool.stakes.push_back(stake);
    pool.chain_ids.push_back(chain_id);
    save_pool(&env, &pool);

    env.storage()
        .persistent()
        .set(&DataKey::BorrowerPool(voucher, pool_id), &true);

    Ok(())
}

/// Issue #1281: Leave an **inactive** pool, withdrawing the caller's stake.
pub fn leave_pool(
    env: Env,
    voucher: Address,
    pool_id: u64,
) -> Result<(), ContractError> {
    voucher.require_auth();

    let mut pool = load_pool(&env, pool_id)?;

    if pool.active {
        return Err(ContractError::CollateralPoolActive);
    }

    let idx = member_index(&pool, &voucher)
        .ok_or(ContractError::NotPoolMember)?;

    let stake = pool.stakes.get(idx as u32).unwrap_or(0);
    let chain_id = pool.chain_ids.get(idx as u32).unwrap_or(0);

    // Rebuild members / stakes / chain_ids with the departing member removed.
    let mut new_members = Vec::new(&env);
    let mut new_stakes: Vec<i128> = Vec::new(&env);
    let mut new_chains: Vec<u32> = Vec::new(&env);
    for (i, m) in pool.members.iter().enumerate() {
        if i != idx {
            new_members.push_back(m.clone());
            new_stakes.push_back(pool.stakes.get(i as u32).unwrap_or(0));
            new_chains.push_back(pool.chain_ids.get(i as u32).unwrap_or(0));
        }
    }

    pool.members = new_members;
    pool.stakes = new_stakes;
    pool.chain_ids = new_chains;
    save_pool(&env, &pool);

    // Remove reverse mapping.
    env.storage()
        .persistent()
        .remove(&DataKey::BorrowerPool(voucher.clone(), pool_id));

    // Return tokens for native-chain members only.
    if chain_id == 0 && stake > 0 {
        let token_client = token::Client::new(&env, &pool.token);
        token_client.transfer(&env.current_contract_address(), &voucher, &stake);
    }

    Ok(())
}

/// Issue #1281: Admin assigns a borrower to a pool, locking its collateral.
pub fn assign_pool_to_borrower(
    env: Env,
    admin_signers: Vec<Address>,
    pool_id: u64,
    borrower: Address,
) -> Result<(), ContractError> {
    require_admin_approval(&env, &admin_signers)?;

    let mut pool = load_pool(&env, pool_id)?;

    if pool.active {
        return Err(ContractError::CollateralPoolActive);
    }

    // Ensure the borrower has no active loan (mirrors vouch.rs guard).
    if env
        .storage()
        .persistent()
        .has(&DataKey::ActiveLoan(borrower.clone()))
    {
        return Err(ContractError::PoolBorrowerActiveLoan);
    }

    pool.borrower = Some(borrower);
    pool.active = true;
    save_pool(&env, &pool);

    Ok(())
}

/// Issue #1281: Read a collateral pool record from persistent storage.
pub fn get_pool(
    env: Env,
    pool_id: u64,
) -> Result<CollateralPool, ContractError> {
    load_pool(&env, pool_id)
}

/// Issue #1281: Total stake held in a collateral pool (sum of all native-chain
/// members' stakes).
pub fn get_pool_total_stake(
    env: Env,
    pool_id: u64,
) -> Result<i128, ContractError> {
    let pool = load_pool(&env, pool_id)?;
    let mut total: i128 = 0;
    for (i, chain_id) in pool.chain_ids.iter().enumerate() {
        if chain_id == 0 {
            let s = pool.stakes.get(i as u32).unwrap_or(0);
            total = total.checked_add(s).ok_or(ContractError::StakeOverflow)?;
        }
    }
    Ok(total)
}

/// Issue #1281: Total stake contributed to a pool from a specific chain.
pub fn get_pool_chain_stake(
    env: Env,
    pool_id: u64,
    chain_id: u32,
) -> Result<i128, ContractError> {
    let pool = load_pool(&env, pool_id)?;
    let mut total: i128 = 0;
    for (i, cid) in pool.chain_ids.iter().enumerate() {
        if cid == chain_id {
            let s = pool.stakes.get(i as u32).unwrap_or(0);
            total = total.checked_add(s).ok_or(ContractError::StakeOverflow)?;
        }
    }
    Ok(total)
}
