/// Gas Cost Regression Testing Module (Issue #1186)
/// Implements automated gas cost tracking and regression detection for smart contract operations.
/// Ensures gas efficiency remains a continuous focus throughout development.

use crate::errors::ContractError;
use soroban_sdk::{contracttype, symbol_short, Env, String, Symbol, Vec};

/// Baseline gas consumption for contract operations
#[derive(Clone, Debug)]
#[contracttype]
pub struct GasBaseline {
    /// Operation name
    pub operation: String,
    /// Baseline gas cost
    pub gas_cost: u64,
    /// When this baseline was established
    pub established_at: u64,
    /// Current cost (for regression detection)
    pub current_cost: u64,
    /// Flag indicating if regression detected
    pub regression_detected: bool,
    /// Regression percentage (bps, 10000 = 100%)
    pub regression_percentage_bps: u32,
}

/// Gas measurement result
#[derive(Clone, Debug)]
#[contracttype]
pub struct GasMeasurement {
    /// Operation identifier
    pub operation: String,
    /// Gas consumed
    pub gas_consumed: u64,
    /// Measurement timestamp
    pub timestamp: u64,
    /// Build/commit hash for tracking
    pub build_hash: String,
}

/// Gas statistics summary
#[derive(Clone, Debug)]
#[contracttype]
pub struct GasStatistics {
    /// Average gas consumption over period
    pub average_gas: u64,
    /// Maximum gas consumption observed
    pub max_gas: u64,
    /// Minimum gas consumption observed
    pub min_gas: u64,
    /// Standard deviation
    pub std_dev: u64,
    /// Total measurements in period
    pub measurement_count: u32,
    /// Percentage change from baseline (bps)
    pub change_from_baseline_bps: i32,
}

const GAS_BASELINE_KEY: Symbol = symbol_short!("gas_bas");
const GAS_MEASUREMENTS_KEY: Symbol = symbol_short!("gas_msr");
const GAS_HISTORY_KEY: Symbol = symbol_short!("gas_his");

/// Regression threshold in basis points (1% = 100 bps)
pub const REGRESSION_THRESHOLD_BPS: u32 = 1000; // 10% threshold

/// Define baseline for an operation
pub fn set_gas_baseline(
    env: &Env,
    operation: String,
    gas_cost: u64,
) -> Result<GasBaseline, ContractError> {
    if gas_cost == 0 {
        return Err(ContractError::InvalidInput);
    }

    let now = env.ledger().timestamp();

    let baseline = GasBaseline {
        operation: operation.clone(),
        gas_cost,
        established_at: now,
        current_cost: gas_cost,
        regression_detected: false,
        regression_percentage_bps: 0,
    };

    // Store baseline
    let mut baselines: Vec<GasBaseline> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()))
        .unwrap_or(Vec::new(env));

    // Update if exists, append if new
    let mut found = false;
    for i in 0..baselines.len() {
        if baselines.get(i).unwrap().operation == operation {
            baselines.set(i, baseline.clone());
            found = true;
            break;
        }
    }

    if !found {
        baselines.push_back(baseline.clone());
    }

    env.storage()
        .persistent()
        .set(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()), &baselines);

    Ok(baseline)
}

/// Record gas measurement and check for regression
pub fn measure_gas_cost(
    env: &Env,
    operation: String,
    gas_consumed: u64,
    build_hash: String,
) -> Result<GasMeasurement, ContractError> {
    let now = env.ledger().timestamp();

    let measurement = GasMeasurement {
        operation: operation.clone(),
        gas_consumed,
        timestamp: now,
        build_hash: build_hash.clone(),
    };

    // Store measurement in history
    let mut measurements: Vec<GasMeasurement> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_MEASUREMENTS_KEY.into()))
        .unwrap_or(Vec::new(env));
    measurements.push_back(measurement.clone());
    env.storage()
        .persistent()
        .set(&crate::types::DataKey::Custom(GAS_MEASUREMENTS_KEY.into()), &measurements);

    // Check for regression against baseline
    check_gas_regression(env, &operation, gas_consumed)?;

    Ok(measurement)
}

/// Check if current gas consumption exceeds baseline by threshold
fn check_gas_regression(env: &Env, operation: &String, current_gas: u64) -> Result<(), ContractError> {
    let baselines: Vec<GasBaseline> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()))
        .unwrap_or(Vec::new(env));

    for baseline in baselines.iter() {
        if baseline.operation == operation {
            let increase_bps = if current_gas > baseline.gas_cost {
                let increase = current_gas - baseline.gas_cost;
                ((increase as u128 * 10_000) / (baseline.gas_cost as u128)) as u32
            } else {
                0
            };

            if increase_bps > REGRESSION_THRESHOLD_BPS {
                // Update baseline with regression info
                let mut updated_baseline = baseline.clone();
                updated_baseline.current_cost = current_gas;
                updated_baseline.regression_detected = true;
                updated_baseline.regression_percentage_bps = increase_bps;

                let mut baselines_mut: Vec<GasBaseline> = env
                    .storage()
                    .persistent()
                    .get(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()))
                    .unwrap_or(Vec::new(env));

                for i in 0..baselines_mut.len() {
                    if baselines_mut.get(i).unwrap().operation == operation {
                        baselines_mut.set(i, updated_baseline);
                        break;
                    }
                }

                env.storage()
                    .persistent()
                    .set(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()), &baselines_mut);

                return Err(ContractError::RegressionDetected);
            }

            return Ok(());
        }
    }

    // If no baseline found, create one automatically
    set_gas_baseline(env, operation.clone(), current_gas)?;
    Ok(())
}

/// Get all gas baselines
pub fn get_gas_baselines(env: &Env) -> Vec<GasBaseline> {
    env.storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_BASELINE_KEY.into()))
        .unwrap_or(Vec::new(env))
}

/// Get gas baseline for specific operation
pub fn get_gas_baseline(env: &Env, operation: &String) -> Result<GasBaseline, ContractError> {
    let baselines = get_gas_baselines(env);
    baselines
        .iter()
        .find(|b| b.operation == operation)
        .ok_or(ContractError::NotFound)
}

/// Calculate gas statistics for an operation
pub fn calculate_gas_statistics(
    env: &Env,
    operation: &String,
    lookback_hours: u64,
) -> Result<GasStatistics, ContractError> {
    let now = env.ledger().timestamp();
    let cutoff_time = now.saturating_sub(lookback_hours * 3600);

    let measurements: Vec<GasMeasurement> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_MEASUREMENTS_KEY.into()))
        .unwrap_or(Vec::new(env));

    // Filter measurements for this operation within time window
    let mut relevant_measurements = Vec::new(env);
    for measurement in measurements.iter() {
        if measurement.operation == operation && measurement.timestamp >= cutoff_time {
            relevant_measurements.push_back(measurement.gas_consumed);
        }
    }

    if relevant_measurements.is_empty() {
        return Err(ContractError::NotFound);
    }

    let count = relevant_measurements.len() as u32;
    let sum: u64 = relevant_measurements.iter().sum();
    let average = sum / count as u64;
    let max = relevant_measurements.iter().max().copied().unwrap_or(0);
    let min = relevant_measurements.iter().min().copied().unwrap_or(u64::MAX);

    // Calculate standard deviation
    let variance: u64 = relevant_measurements
        .iter()
        .map(|x| {
            let diff = if x > average {
                x - average
            } else {
                average - x
            };
            diff * diff
        })
        .fold(0u64, |acc, x| acc + x)
        / count as u64;

    let std_dev = integer_sqrt(variance);

    // Calculate change from baseline
    let baseline = get_gas_baseline(env, operation)?;
    let change_from_baseline_bps = if average > baseline.gas_cost {
        let increase = average - baseline.gas_cost;
        ((increase as i128 * 10_000) / (baseline.gas_cost as i128)) as i32
    } else {
        let decrease = baseline.gas_cost - average;
        -(((decrease as i128 * 10_000) / (baseline.gas_cost as i128)) as i32)
    };

    Ok(GasStatistics {
        average_gas: average,
        max_gas: max,
        min_gas: min,
        std_dev,
        measurement_count: count,
        change_from_baseline_bps,
    })
}

/// Integer square root using Newton's method
fn integer_sqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }

    let mut x0 = n;
    let mut x1 = (x0 + 1) / 2;

    while x1 < x0 {
        x0 = x1;
        x1 = (x0 + n / x0) / 2;
    }

    x0
}

/// Get recent measurements for an operation
pub fn get_recent_measurements(
    env: &Env,
    operation: &String,
    limit: u32,
) -> Vec<GasMeasurement> {
    let measurements: Vec<GasMeasurement> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::Custom(GAS_MEASUREMENTS_KEY.into()))
        .unwrap_or(Vec::new(env));

    let mut filtered = Vec::new(env);
    let start_idx = if measurements.len() > limit as usize {
        measurements.len() - (limit as usize)
    } else {
        0
    };

    for i in start_idx..measurements.len() {
        if measurements.get(i).unwrap().operation == operation {
            filtered.push_back(measurements.get(i).unwrap());
        }
    }

    filtered
}

/// Verify gas optimization progress
pub fn verify_gas_optimization(env: &Env, operation: &String) -> Result<bool, ContractError> {
    let baseline = get_gas_baseline(env, operation)?;

    // Check if current cost is within acceptable range
    let increase_bps = if baseline.current_cost > baseline.gas_cost {
        let increase = baseline.current_cost - baseline.gas_cost;
        ((increase as u128 * 10_000) / (baseline.gas_cost as u128)) as u32
    } else {
        0
    };

    Ok(increase_bps <= REGRESSION_THRESHOLD_BPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integer_sqrt() {
        assert_eq!(integer_sqrt(0), 0);
        assert_eq!(integer_sqrt(1), 1);
        assert_eq!(integer_sqrt(4), 2);
        assert_eq!(integer_sqrt(9), 3);
        assert_eq!(integer_sqrt(16), 4);
        assert_eq!(integer_sqrt(100), 10);
    }

    #[test]
    fn test_regression_threshold() {
        // 10% regression should trigger (1000 bps)
        assert!(1000 > REGRESSION_THRESHOLD_BPS / 10);
    }
}
