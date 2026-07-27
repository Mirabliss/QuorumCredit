/// Lending Pool Composability Module (Issue #1187)
/// Enables lending pools to integrate with external DeFi protocols,
/// supporting yield farming and cross-protocol asset management.

use crate::errors::ContractError;
use crate::types::DataKey;
use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Symbol, Vec};

/// External pool interface for composability
#[derive(Clone, Debug)]
#[contracttype]
pub struct ExternalPoolInterface {
    /// Unique identifier for the external pool
    pub pool_id: u64,
    /// Name/identifier of the external protocol
    pub protocol_name: String,
    /// Address of the external pool contract
    pub pool_contract: Address,
    /// Type of yield strategy (farming, staking, etc)
    pub strategy_type: String,
    /// Whether this pool is currently active
    pub is_active: bool,
    /// Timestamp when pool was registered
    pub registered_at: u64,
}

/// Deposit record to external pool
#[derive(Clone, Debug)]
#[contracttype]
pub struct ExternalPoolDeposit {
    /// Deposit identifier
    pub deposit_id: u64,
    /// Internal pool that made the deposit
    pub internal_pool_id: u64,
    /// External pool receiving the deposit
    pub external_pool_id: u64,
    /// Amount deposited
    pub amount: i128,
    /// Timestamp of deposit
    pub deposit_time: u64,
    /// Yield earned so far
    pub yield_earned: i128,
}

/// Yield earning record
#[derive(Clone, Debug)]
#[contracttype]
pub struct YieldEarning {
    /// Deposit this yield is from
    pub deposit_id: u64,
    /// Amount of yield earned
    pub amount: i128,
    /// Timestamp of earning
    pub earned_at: u64,
    /// APY at time of earning (in basis points)
    pub apy_bps: u32,
}

/// Portfolio allocation across pools
#[derive(Clone, Debug)]
#[contracttype]
pub struct PoolAllocation {
    /// Pool identifier
    pub pool_id: u64,
    /// Allocated amount
    pub amount: i128,
    /// Percentage of total portfolio (in basis points)
    pub allocation_percentage_bps: u32,
    /// Type of pool (internal/external)
    pub pool_type: String,
}

/// Portfolio composition snapshot
#[derive(Clone, Debug)]
#[contracttype]
pub struct PortfolioSnapshot {
    /// Timestamp of snapshot
    pub timestamp: u64,
    /// Total portfolio value
    pub total_value: i128,
    /// Allocations across pools
    pub allocations: Vec<PoolAllocation>,
}

const EXTERNAL_POOLS_KEY: Symbol = symbol_short!("ext_pls");
const EXTERNAL_DEPOSITS_KEY: Symbol = symbol_short!("ext_dps");
const YIELD_EARNINGS_KEY: Symbol = symbol_short!("yld_ern");
const PORTFOLIO_SNAPSHOTS_KEY: Symbol = symbol_short!("prt_snp");
const NEXT_POOL_ID_KEY: Symbol = symbol_short!("nxt_pid");
const NEXT_DEPOSIT_ID_KEY: Symbol = symbol_short!("nxt_did");

/// Register an external pool for composability
pub fn register_external_pool(
    env: &Env,
    protocol_name: String,
    pool_contract: Address,
    strategy_type: String,
) -> Result<ExternalPoolInterface, ContractError> {
    if protocol_name.is_empty() || strategy_type.is_empty() {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    // Get next pool ID
    let next_pool_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::Custom(NEXT_POOL_ID_KEY.into()))
        .unwrap_or(1u64);

    let pool_interface = ExternalPoolInterface {
        pool_id: next_pool_id,
        protocol_name,
        pool_contract,
        strategy_type,
        is_active: true,
        registered_at: now,
    };

    // Store pool interface
    let mut pools: Vec<ExternalPoolInterface> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()))
        .unwrap_or(Vec::new(env));
    pools.push_back(pool_interface.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()), &pools);

    // Increment pool ID
    env.storage()
        .instance()
        .set(&DataKey::Custom(NEXT_POOL_ID_KEY.into()), &(next_pool_id + 1));

    Ok(pool_interface)
}

/// Deposit assets to an external pool for yield farming
pub fn deposit_to_external_pool(
    env: &Env,
    internal_pool_id: u64,
    external_pool_id: u64,
    amount: i128,
) -> Result<ExternalPoolDeposit, ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    // Get next deposit ID
    let next_deposit_id: u64 = env
        .storage()
        .instance()
        .get(&DataKey::Custom(NEXT_DEPOSIT_ID_KEY.into()))
        .unwrap_or(1u64);

    let deposit = ExternalPoolDeposit {
        deposit_id: next_deposit_id,
        internal_pool_id,
        external_pool_id,
        amount,
        deposit_time: now,
        yield_earned: 0,
    };

    // Store deposit record
    let mut deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));
    deposits.push_back(deposit.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()), &deposits);

    // Increment deposit ID
    env.storage()
        .instance()
        .set(&DataKey::Custom(NEXT_DEPOSIT_ID_KEY.into()), &(next_deposit_id + 1));

    Ok(deposit)
}

/// Record yield earned from external pool
pub fn record_yield_earning(
    env: &Env,
    deposit_id: u64,
    amount: i128,
    apy_bps: u32,
) -> Result<YieldEarning, ContractError> {
    if amount <= 0 || apy_bps > 10_000 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    let earning = YieldEarning {
        deposit_id,
        amount,
        earned_at: now,
        apy_bps,
    };

    // Store yield earning
    let mut earnings: Vec<YieldEarning> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(YIELD_EARNINGS_KEY.into()))
        .unwrap_or(Vec::new(env));
    earnings.push_back(earning.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(YIELD_EARNINGS_KEY.into()), &earnings);

    // Update deposit's yield_earned
    let mut deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    for i in 0..deposits.len() {
        if deposits.get(i).unwrap().deposit_id == deposit_id {
            let mut deposit = deposits.get(i).unwrap();
            deposit.yield_earned = deposit.yield_earned.saturating_add(amount);
            deposits.set(i, deposit);
            break;
        }
    }

    env.storage()
        .persistent()
        .set(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()), &deposits);

    Ok(earning)
}

/// Automatically claim all accumulated yields
pub fn claim_all_yields(env: &Env, internal_pool_id: u64) -> Result<i128, ContractError> {
    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut total_yields = 0i128;

    for deposit in deposits.iter() {
        if deposit.internal_pool_id == internal_pool_id {
            total_yields = total_yields.saturating_add(deposit.yield_earned);
        }
    }

    Ok(total_yields)
}

/// Get aggregated yield from all pools
pub fn get_aggregated_yield(env: &Env, internal_pool_id: u64) -> i128 {
    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    deposits
        .iter()
        .filter(|d| d.internal_pool_id == internal_pool_id)
        .fold(0i128, |acc, d| acc.saturating_add(d.yield_earned))
}

/// Create a portfolio composition snapshot
pub fn create_portfolio_snapshot(
    env: &Env,
    internal_pool_id: u64,
    total_value: i128,
) -> Result<PortfolioSnapshot, ContractError> {
    if total_value <= 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();
    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut allocations = Vec::new(env);

    // Aggregate by external pool
    for deposit in deposits.iter() {
        if deposit.internal_pool_id == internal_pool_id {
            let allocation_percentage_bps =
                ((deposit.amount as u128 * 10_000) / (total_value as u128)) as u32;

            let allocation = PoolAllocation {
                pool_id: deposit.external_pool_id,
                amount: deposit.amount,
                allocation_percentage_bps,
                pool_type: String::from_slice(env, "external"),
            };

            allocations.push_back(allocation);
        }
    }

    let snapshot = PortfolioSnapshot {
        timestamp: now,
        total_value,
        allocations,
    };

    // Store snapshot
    let mut snapshots: Vec<PortfolioSnapshot> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(PORTFOLIO_SNAPSHOTS_KEY.into()))
        .unwrap_or(Vec::new(env));
    snapshots.push_back(snapshot.clone());
    env.storage()
        .persistent()
        .set(&DataKey::Custom(PORTFOLIO_SNAPSHOTS_KEY.into()), &snapshots);

    Ok(snapshot)
}

/// Get current portfolio allocation
pub fn get_portfolio_allocation(env: &Env, internal_pool_id: u64) -> Vec<PoolAllocation> {
    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let total: i128 = deposits
        .iter()
        .filter(|d| d.internal_pool_id == internal_pool_id)
        .map(|d| d.amount)
        .sum();

    if total == 0 {
        return Vec::new(env);
    }

    let mut allocations = Vec::new(env);
    for deposit in deposits.iter() {
        if deposit.internal_pool_id == internal_pool_id {
            let allocation_percentage_bps =
                ((deposit.amount as u128 * 10_000) / (total as u128)) as u32;

            let allocation = PoolAllocation {
                pool_id: deposit.external_pool_id,
                amount: deposit.amount,
                allocation_percentage_bps,
                pool_type: String::from_slice(env, "external"),
            };

            allocations.push_back(allocation);
        }
    }

    allocations
}

/// Get all active external pools
pub fn get_active_pools(env: &Env) -> Vec<ExternalPoolInterface> {
    let pools: Vec<ExternalPoolInterface> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut active_pools = Vec::new(env);
    for pool in pools.iter() {
        if pool.is_active {
            active_pools.push_back(pool);
        }
    }

    active_pools
}

/// Get external pool by ID
pub fn get_external_pool(env: &Env, pool_id: u64) -> Result<ExternalPoolInterface, ContractError> {
    let pools: Vec<ExternalPoolInterface> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()))
        .unwrap_or(Vec::new(env));

    pools
        .iter()
        .find(|p| p.pool_id == pool_id)
        .ok_or(ContractError::NotFound)
}

/// Deactivate an external pool
pub fn deactivate_pool(env: &Env, pool_id: u64) -> Result<(), ContractError> {
    let mut pools: Vec<ExternalPoolInterface> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut found = false;
    for i in 0..pools.len() {
        if pools.get(i).unwrap().pool_id == pool_id {
            let mut pool = pools.get(i).unwrap();
            pool.is_active = false;
            pools.set(i, pool);
            found = true;
            break;
        }
    }

    if !found {
        return Err(ContractError::NotFound);
    }

    env.storage()
        .persistent()
        .set(&DataKey::Custom(EXTERNAL_POOLS_KEY.into()), &pools);

    Ok(())
}

/// Get total value locked across all external pools
pub fn get_total_external_tvl(env: &Env) -> i128 {
    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    deposits.iter().fold(0i128, |acc, d| acc.saturating_add(d.amount))
}

/// Calculate weighted average APY for a pool
pub fn calculate_weighted_avg_apy(env: &Env, internal_pool_id: u64) -> Result<u32, ContractError> {
    let earnings: Vec<YieldEarning> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(YIELD_EARNINGS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let deposits: Vec<ExternalPoolDeposit> = env
        .storage()
        .persistent()
        .get(&DataKey::Custom(EXTERNAL_DEPOSITS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let pool_deposits: Vec<ExternalPoolDeposit> = deposits
        .iter()
        .filter(|d| d.internal_pool_id == internal_pool_id)
        .collect::<Vec<_>>();

    if pool_deposits.is_empty() {
        return Err(ContractError::NotFound);
    }

    let total_amount: i128 = pool_deposits.iter().map(|d| d.amount).sum();

    let mut weighted_apy = 0u64;
    for deposit in pool_deposits.iter() {
        let relevant_earnings: Vec<YieldEarning> = earnings
            .iter()
            .filter(|e| e.deposit_id == deposit.deposit_id)
            .collect::<Vec<_>>();

        for earning in relevant_earnings.iter() {
            let weight = ((deposit.amount as u128 * 10_000) / (total_amount as u128)) as u64;
            weighted_apy = weighted_apy.saturating_add((earning.apy_bps as u64 * weight) / 10_000);
        }
    }

    Ok(weighted_apy as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_allocation_calculation() {
        // Test that allocation percentages are calculated correctly
        let amount = 500i128;
        let total = 1000i128;
        let allocation_percentage_bps = ((amount as u128 * 10_000) / (total as u128)) as u32;

        assert_eq!(allocation_percentage_bps, 5000); // 50%
    }
}
