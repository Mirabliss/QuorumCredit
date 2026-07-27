/// Economic Model Simulation Testing Module (Issue #1184)
/// This module implements Monte Carlo simulation for loan portfolio analysis,
/// including stress testing under various economic scenarios.

use crate::errors::ContractError;
use crate::types::{LoanRecord, LoanStatus};
use soroban_sdk::{Env, Vec, contracttype};

/// Default number of Monte Carlo simulations
pub const DEFAULT_SIMULATION_COUNT: u32 = 10_000;

/// Risk metrics thresholds
pub const VAR_CONFIDENCE_LEVEL: f64 = 0.95; // 95% confidence level
pub const CVAR_CONFIDENCE_LEVEL: f64 = 0.95; // Expected Shortfall at 95% confidence

/// Simulation parameters for Monte Carlo analysis
#[derive(Clone, Debug)]
#[contracttype]
pub struct SimulationParams {
    /// Default rate (probability of default) in basis points (0-10000)
    pub default_rate_bps: u32,
    /// Interest rate for loans in basis points (0-10000)
    pub interest_rate_bps: u32,
    /// Recovery rate (percentage of defaulted loan recovered) in basis points (0-10000)
    pub recovery_rate_bps: u32,
    /// Number of simulations to run
    pub simulation_count: u32,
    /// Initial portfolio value
    pub portfolio_value: i128,
}

/// Results of a single Monte Carlo simulation
#[derive(Clone, Debug)]
#[contracttype]
pub struct SimulationResult {
    /// Portfolio value at end of simulation period
    pub end_value: i128,
    /// Total interest collected
    pub interest_collected: i128,
    /// Total losses from defaults
    pub default_losses: i128,
    /// Number of defaulted loans
    pub defaults_count: u32,
}

/// Summary statistics from Monte Carlo simulations
#[derive(Clone, Debug)]
#[contracttype]
pub struct PortfolioStressTestResult {
    /// Value at Risk at 95% confidence
    pub var_95: i128,
    /// Expected Shortfall (CVaR) at 95% confidence
    pub cvar_95: i128,
    /// Mean portfolio value
    pub mean_value: i128,
    /// Minimum portfolio value observed
    pub min_value: i128,
    /// Maximum portfolio value observed
    pub max_value: i128,
    /// Standard deviation of outcomes
    pub std_dev: i128,
    /// Probability of portfolio loss
    pub loss_probability: u32, // in basis points (0-10000)
    /// Maximum loss scenario
    pub max_loss: i128,
    /// Simulation count used
    pub simulation_count: u32,
}

/// Pseudo-random number generator using linear congruential method
/// Safe for use in smart contracts as it's deterministic
pub struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    /// Initialize RNG with a seed
    pub fn new(seed: u64) -> Self {
        SimpleRng {
            state: seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407),
        }
    }

    /// Generate next random number between 0 and 1 (scaled to u32)
    pub fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.state >> 32) as u32
    }

    /// Generate random number between 0.0 and 1.0 (approximated)
    pub fn next_f64(&mut self) -> u64 {
        self.next_u32() as u64
    }
}

/// Run Monte Carlo simulation for portfolio stress testing
pub fn run_monte_carlo_simulation(
    params: &SimulationParams,
    seed: u64,
) -> Result<PortfolioStressTestResult, ContractError> {
    if params.simulation_count == 0 {
        return Err(ContractError::InvalidInput);
    }

    let mut results: Vec<i128> = Vec::new();
    let mut losses_count = 0u32;
    let mut total_interest = 0i128;
    let mut total_defaults = 0i128;

    let mut rng = SimpleRng::new(seed);

    // Run simulations
    for _ in 0..params.simulation_count {
        let sim_result = simulate_single_period(params, &mut rng);

        results.push(sim_result.end_value);
        total_interest = total_interest.saturating_add(sim_result.interest_collected);
        total_defaults = total_defaults.saturating_add(sim_result.default_losses);

        if sim_result.end_value < params.portfolio_value {
            losses_count = losses_count.saturating_add(1);
        }
    }

    // Calculate statistics
    calculate_portfolio_metrics(&results, params, total_interest, total_defaults, losses_count)
}

/// Simulate a single period for the portfolio
fn simulate_single_period(
    params: &SimulationParams,
    rng: &mut SimpleRng,
) -> SimulationResult {
    let default_threshold = (params.default_rate_bps as u64 * 1_000_000) / 10_000;

    let mut end_value = params.portfolio_value;
    let mut interest_collected = 0i128;
    let mut default_losses = 0i128;
    let mut defaults = 0u32;

    // Simulate ~100 loans per portfolio for reasonable distribution
    let loan_count = 100u32;
    let loan_value = params.portfolio_value / (loan_count as i128);

    for _ in 0..loan_count {
        let random_val = rng.next_f64();

        if random_val < default_threshold {
            // Loan defaults
            defaults = defaults.saturating_add(1);
            let recovery = (loan_value as u128)
                .saturating_mul(params.recovery_rate_bps as u128)
                / 10_000;
            let loss = loan_value.saturating_sub(recovery as i128);
            default_losses = default_losses.saturating_add(loss);
            end_value = end_value.saturating_sub(loss);
        } else {
            // Loan pays interest
            let interest = (loan_value as u128)
                .saturating_mul(params.interest_rate_bps as u128)
                / 10_000;
            interest_collected = interest_collected.saturating_add(interest as i128);
            end_value = end_value.saturating_add(interest as i128);
        }
    }

    SimulationResult {
        end_value,
        interest_collected,
        default_losses,
        defaults_count: defaults,
    }
}

/// Calculate risk metrics from simulation results
fn calculate_portfolio_metrics(
    results: &Vec<i128>,
    params: &SimulationParams,
    total_interest: i128,
    total_defaults: i128,
    losses_count: u32,
) -> Result<PortfolioStressTestResult, ContractError> {
    if results.is_empty() {
        return Err(ContractError::InvalidInput);
    }

    let mut sorted_results = results.clone();
    // Simple bubble sort for small arrays
    for i in 0..sorted_results.len() {
        for j in i + 1..sorted_results.len() {
            if sorted_results.get(i).unwrap() > sorted_results.get(j).unwrap() {
                let temp = sorted_results.get(i).unwrap();
                sorted_results.set(i, sorted_results.get(j).unwrap());
                sorted_results.set(j, temp);
            }
        }
    }

    let mean_value = results.iter().fold(0i128, |acc, val| {
        acc.saturating_add(val)
    }) / results.len() as i128;

    // Calculate VaR (Value at Risk) at 95% confidence
    let var_index = ((results.len() as f64) * (1.0 - VAR_CONFIDENCE_LEVEL)) as usize;
    let var_95 = sorted_results.get(var_index).unwrap_or(sorted_results.get(0).unwrap());

    // Calculate CVaR (Conditional Value at Risk / Expected Shortfall)
    let cvar_index = ((results.len() as f64) * (1.0 - CVAR_CONFIDENCE_LEVEL)) as usize;
    let cvar_95 = sorted_results
        .iter()
        .take(cvar_index + 1)
        .fold(0i128, |acc, val| acc.saturating_add(val))
        / (cvar_index as i128 + 1);

    let min_value = sorted_results.get(0).unwrap();
    let max_value = sorted_results.get(sorted_results.len() - 1).unwrap();

    // Calculate standard deviation
    let variance = results
        .iter()
        .fold(0i128, |acc, val| {
            let diff = val - mean_value;
            acc.saturating_add(diff.saturating_mul(diff))
        })
        / results.len() as i128;

    let std_dev = if variance > 0 {
        // Approximate square root using integer arithmetic
        integer_sqrt(variance as u128) as i128
    } else {
        0
    };

    let loss_probability = (losses_count as u128 * 10_000 / params.simulation_count as u128) as u32;
    let max_loss = min_value.saturating_sub(params.portfolio_value);

    Ok(PortfolioStressTestResult {
        var_95,
        cvar_95,
        mean_value,
        min_value,
        max_value,
        std_dev,
        loss_probability,
        max_loss,
        simulation_count: params.simulation_count,
    })
}

/// Integer square root using binary search
fn integer_sqrt(n: u128) -> u128 {
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

/// Stress test the portfolio across multiple scenarios
pub fn stress_test_scenarios(
    base_params: &SimulationParams,
    seed: u64,
) -> Result<Vec<PortfolioStressTestResult>, ContractError> {
    let mut results = Vec::new();

    // Scenario 1: Base case
    let base_result = run_monte_carlo_simulation(base_params, seed)?;
    results.push(base_result);

    // Scenario 2: High default environment (2x default rate)
    let mut high_default_params = base_params.clone();
    high_default_params.default_rate_bps = (high_default_params.default_rate_bps as u64 * 2).min(10_000) as u32;
    let high_default_result = run_monte_carlo_simulation(&high_default_params, seed.wrapping_add(1))?;
    results.push(high_default_result);

    // Scenario 3: Low interest rate environment (50% lower rates)
    let mut low_rate_params = base_params.clone();
    low_rate_params.interest_rate_bps = low_rate_params.interest_rate_bps / 2;
    let low_rate_result = run_monte_carlo_simulation(&low_rate_params, seed.wrapping_add(2))?;
    results.push(low_rate_result);

    // Scenario 4: Poor recovery (recovery rate halved)
    let mut poor_recovery_params = base_params.clone();
    poor_recovery_params.recovery_rate_bps = poor_recovery_params.recovery_rate_bps / 2;
    let poor_recovery_result = run_monte_carlo_simulation(&poor_recovery_params, seed.wrapping_add(3))?;
    results.push(poor_recovery_result);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monte_carlo_simulation() {
        let params = SimulationParams {
            default_rate_bps: 500,  // 5% default rate
            interest_rate_bps: 1000, // 10% interest rate
            recovery_rate_bps: 5000, // 50% recovery rate
            simulation_count: 1000,
            portfolio_value: 1_000_000,
        };

        let result = run_monte_carlo_simulation(&params, 12345).unwrap();

        // Basic sanity checks
        assert!(result.mean_value > 0);
        assert!(result.min_value <= result.mean_value);
        assert!(result.mean_value <= result.max_value);
        assert!(result.loss_probability <= 10_000);
    }

    #[test]
    fn test_stress_test_scenarios() {
        let params = SimulationParams {
            default_rate_bps: 300,
            interest_rate_bps: 800,
            recovery_rate_bps: 6000,
            simulation_count: 500,
            portfolio_value: 500_000,
        };

        let results = stress_test_scenarios(&params, 54321).unwrap();
        assert!(results.len() == 4); // 4 scenarios
    }
}
