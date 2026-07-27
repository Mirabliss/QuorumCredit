//! # Issue #1243 — Dynamic Interest Rate Based on Utilization
//!
//! Implements a utilization-based dynamic interest rate model for the protocol:
//!
//! - **Utilization** = outstanding_loans / total_capital
//! - **Rate formula (two-slope model)**:
//!   - When utilization ≤ kink (80%):  `rate = base_rate_bps`
//!   - When utilization > kink (80%):
//!     `rate = base_rate_bps + ((utilization_bps - kink_bps) * premium_slope_bps / 10_000)`
//! - Rate is capped at `rate_cap_bps` and floored at `rate_floor_bps`.
//! - Rate changes are snapshotted for off-chain analysis.
//! - Admins can update the configuration via `set_utilization_rate_config`.

use crate::errors::ContractError;
use crate::helpers::{require_admin_approval, require_not_paused};
use crate::types::{
    DataKey, LoanRecord, LoanStatus, UtilizationRateConfig, UtilizationRateSnapshot,
    VouchRecord, default_utilization_rate_config, BPS_DENOMINATOR,
};
use soroban_sdk::{symbol_short, Address, Env, Vec};

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Load the utilization rate config, defaulting to the default config if not set.
pub fn load_utilization_rate_config(env: &Env) -> UtilizationRateConfig {
    env.storage()
        .instance()
        .get(&DataKey::UtilizationRateConfig)
        .unwrap_or_else(|| default_utilization_rate_config())
}

// ── Core rate calculation ─────────────────────────────────────────────────────

/// Calculate the effective interest rate (in basis points) based on current utilization.
///
/// # Arguments
/// * `outstanding_loans` — Total principal of active loans in stroops.
/// * `total_capital`     — Total vouched stake (capital) in stroops.
///
/// # Returns
/// The effective interest rate in basis points, clamped to `[rate_floor_bps, rate_cap_bps]`.
///
/// # Formula
/// ```text
/// utilization_bps = outstanding_loans * 10_000 / total_capital   (0–10000)
///
/// if utilization_bps <= kink_utilization_bps:
///     rate = base_rate_bps
/// else:
///     excess_bps = utilization_bps - kink_utilization_bps
///     rate = base_rate_bps + (excess_bps * premium_slope_bps / 10_000)
///
/// rate = clamp(rate, rate_floor_bps, rate_cap_bps)
/// ```
pub fn calculate_utilization_rate(env: &Env, outstanding_loans: i128, total_capital: i128) -> i128 {
    let config = load_utilization_rate_config(env);

    if !config.enabled || total_capital <= 0 {
        return config.base_rate_bps;
    }

    // Utilization in basis points (0–10_000)
    let utilization_bps = if outstanding_loans <= 0 {
        0
    } else {
        outstanding_loans * BPS_DENOMINATOR / total_capital
    };

    let rate = if utilization_bps <= config.kink_utilization_bps {
        config.base_rate_bps
    } else {
        let excess_bps = utilization_bps - config.kink_utilization_bps;
        // Each 1 basis-point of excess utilization adds `premium_slope_bps / 10_000` bps to rate.
        // Simplified: excess_bps * premium_slope_bps / 10_000
        let premium = excess_bps * config.premium_slope_bps / BPS_DENOMINATOR;
        config.base_rate_bps + premium
    };

    // Clamp to [floor, cap]
    rate.max(config.rate_floor_bps).min(config.rate_cap_bps)
}

// ── Protocol-level utilization snapshot ──────────────────────────────────────

/// Compute the current protocol utilization by reading all active loans.
///
/// Outstanding loans = sum of active loan principals.
/// Total capital     = sum of all vouched stakes across all borrowers.
///
/// This is an approximate value suitable for rate calculation. It iterates
/// the borrower list stored in `DataKey::BorrowerList`.
///
/// Returns `(outstanding_loans, total_capital, utilization_bps)`.
pub fn compute_protocol_utilization(env: &Env) -> (i128, i128, i128) {
    let borrowers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::BorrowerList)
        .unwrap_or(Vec::new(env));

    let mut outstanding_loans: i128 = 0;
    let mut total_capital: i128 = 0;

    for borrower in borrowers.iter() {
        // Sum active loan principals
        if let Some(loan_id) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::ActiveLoan(borrower.clone()))
        {
            if let Some(loan) = env
                .storage()
                .persistent()
                .get::<DataKey, LoanRecord>(&DataKey::Loan(loan_id))
            {
                if loan.status == LoanStatus::Active {
                    let repaid = loan.amount_repaid;
                    let principal = loan.amount;
                    let outstanding = (principal - repaid).max(0);
                    outstanding_loans += outstanding;
                }
            }
        }

        // Sum vouched stakes
        if let Some(vouches) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<VouchRecord>>(&DataKey::Vouches(borrower.clone()))
        {
            for vouch in vouches.iter() {
                total_capital += vouch.stake;
            }
        }
    }

    let utilization_bps = if total_capital > 0 {
        outstanding_loans * BPS_DENOMINATOR / total_capital
    } else {
        0
    };

    (outstanding_loans, total_capital, utilization_bps)
}

// ── Snapshot & query ──────────────────────────────────────────────────────────

/// Snapshot the current utilization and effective rate, storing it for analysis.
///
/// Emits event: `rate/snapshot` with `(utilization_bps, effective_rate_bps)`.
pub fn snapshot_utilization_rate(env: Env) -> Result<UtilizationRateSnapshot, ContractError> {
    require_not_paused(&env)?;

    let (outstanding_loans, total_capital, utilization_bps) =
        compute_protocol_utilization(&env);

    let effective_rate_bps = calculate_utilization_rate(&env, outstanding_loans, total_capital);

    let snapshot = UtilizationRateSnapshot {
        recorded_at: env.ledger().timestamp(),
        utilization_bps,
        effective_rate_bps,
        outstanding_loans,
        total_capital,
    };

    env.storage()
        .instance()
        .set(&DataKey::UtilizationRateSnapshot, &snapshot);

    env.events().publish(
        (symbol_short!("rate"), symbol_short!("snap")),
        (utilization_bps, effective_rate_bps),
    );

    Ok(snapshot)
}

/// Get the current effective interest rate based on protocol utilization.
///
/// This is a pure read — it does NOT write a snapshot.
///
/// Returns the rate in basis points.
pub fn get_current_utilization_rate(env: Env) -> i128 {
    let (outstanding, capital, _) = compute_protocol_utilization(&env);
    calculate_utilization_rate(&env, outstanding, capital)
}

/// Get the most recently stored utilization rate snapshot.
pub fn get_utilization_rate_snapshot(env: Env) -> Option<UtilizationRateSnapshot> {
    env.storage()
        .instance()
        .get(&DataKey::UtilizationRateSnapshot)
}

/// Get the utilization rate configuration.
pub fn get_utilization_rate_config(env: Env) -> UtilizationRateConfig {
    load_utilization_rate_config(&env)
}

// ── Admin: update configuration ───────────────────────────────────────────────

/// Update the utilization rate configuration. Admin-only.
///
/// Validates that:
/// - `base_rate_bps` ≥ `rate_floor_bps`
/// - `rate_cap_bps`  ≥ `base_rate_bps`
/// - `kink_utilization_bps` is in [0, 10_000]
/// - `premium_slope_bps` ≥ 0
///
/// Emits event: `rate/config` with `(base_rate_bps, kink_utilization_bps, premium_slope_bps, rate_cap_bps)`.
pub fn set_utilization_rate_config(
    env: Env,
    admin_signers: Vec<Address>,
    config: UtilizationRateConfig,
) -> Result<(), ContractError> {
    require_not_paused(&env)?;
    require_admin_approval(&env, &admin_signers);

    // Validate config
    if config.base_rate_bps < 0
        || config.rate_floor_bps < 0
        || config.rate_cap_bps < 0
        || config.kink_utilization_bps < 0
        || config.premium_slope_bps < 0
    {
        return Err(ContractError::InvalidDynamicRateConfig);
    }
    if config.base_rate_bps < config.rate_floor_bps {
        return Err(ContractError::InvalidDynamicRateConfig);
    }
    if config.rate_cap_bps < config.base_rate_bps {
        return Err(ContractError::InvalidDynamicRateConfig);
    }
    if config.kink_utilization_bps > BPS_DENOMINATOR {
        return Err(ContractError::InvalidDynamicRateConfig);
    }

    env.storage()
        .instance()
        .set(&DataKey::UtilizationRateConfig, &config);

    env.events().publish(
        (symbol_short!("rate"), symbol_short!("config")),
        (
            config.base_rate_bps,
            config.kink_utilization_bps,
            config.premium_slope_bps,
            config.rate_cap_bps,
        ),
    );

    Ok(())
}
