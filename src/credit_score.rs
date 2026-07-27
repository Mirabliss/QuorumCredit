use crate::errors::ContractError;
use crate::types::{
    CreditScore, CreditScoreConfig, CreditTier, DataKey, TierRewards,
    DEFAULT_CREDIT_SCORE_CONFIG, LoanRecord, LoanStatus,
};
use soroban_sdk::{symbol_short, Address, Env, Vec};

/// Get the credit score configuration, or default if not set.
pub fn get_credit_score_config(env: &Env) -> CreditScoreConfig {
    env.storage()
        .instance()
        .get(&DataKey::CreditScoreConfig)
        .unwrap_or(DEFAULT_CREDIT_SCORE_CONFIG)
}

/// Calculate the credit tier from a score (0-1000).
pub fn calculate_tier(score: u32) -> CreditTier {
    if score < 350 {
        CreditTier::Poor
    } else if score < 550 {
        CreditTier::Fair
    } else if score < 700 {
        CreditTier::Good
    } else if score < 850 {
        CreditTier::VeryGood
    } else {
        CreditTier::Excellent
    }
}

/// Calculate repayment history score component (0-1000).
pub fn calculate_repayment_history_score(
    successful_repayments: u32,
    total_loans: u32,
    defaults: u32,
) -> u32 {
    if total_loans == 0 {
        return 500; // Neutral score for new users
    }

    let success_rate = successful_repayments as f64 / total_loans as f64;
    let default_penalty = defaults as f64 * 200.0; // Each default costs 200 points
    let base_score = success_rate * 1000.0;
    let adjusted_score = base_score - default_penalty;
    adjusted_score.max(0.0).min(1000.0) as u32
}

/// Calculate loan count score component (0-1000).
pub fn calculate_loan_count_score(total_loans: u32) -> u32 {
    // More loans with good history = higher score (up to 10 loans)
    let max_loans = 10;
    let score = (total_loans as f64 / max_loans as f64) * 1000.0;
    score.min(1000.0) as u32
}

/// Calculate account age score component (0-1000).
pub fn calculate_account_age_score(account_age: u64) -> u32 {
    // Account age in seconds, max benefit at 1 year
    let max_age = 365 * 24 * 60 * 60; // 1 year
    let score = (account_age as f64 / max_age as f64) * 1000.0;
    score.min(1000.0) as u32
}

/// Minimum stake (in stroops) for a vouch to count toward credit score reputation.
/// Vouches below this floor are ignored for reputation purposes, making trivial
/// micro-stake Sybil rings worthless. 1,000,000 stroops = 0.1 XLM.
pub const SYBIL_MIN_STAKE_FOR_CREDIT: i128 = 1_000_000;

/// Minimum age (in seconds) a vouch must have before it counts toward
/// the vouching score. 24 hours prevents same-block vouching farms.
pub const SYBIL_MIN_VOUCH_AGE_SECS: u64 = 24 * 60 * 60;

/// Saturation cap for stake-time weight (in stroops × seconds / 1e14).
/// Prevents a single mega-stake from dominating the entire score.
pub const SYBIL_STAKE_TIME_SATURATION: i128 = 100;

/// Calculate vouching activity score component (0-1000).
///
/// **Sybil-resistant redesign (replaces raw voucher-count logic):**
/// Score is now based on the *total stake-time weight* of qualifying vouches
/// for the borrower as the scored party, not raw vouch count.
///
/// For each VouchRecord of the borrower:
///   1. Ignore vouches with `stake < SYBIL_MIN_STAKE_FOR_CREDIT` (trivial-stake floor).
///   2. Ignore vouches younger than `SYBIL_MIN_VOUCH_AGE_SECS` (flash-vouch floor).
///   3. Compute `weight = stake_xlm * age_days` (stake in XLM units × age in days).
///   4. Apply diminishing returns: contribution = sqrt(weight), capped per vouch at
///      `SYBIL_STAKE_TIME_SATURATION` (unit: sqrt(XLM·days)).
///   5. Sum contributions across all qualifying vouches, saturate total at 100,
///      and scale to 0–1000.
///
/// A Sybil ring of N addresses each staking the minimum with 0-day-old vouches
/// contributes 0. A single genuine voucher staking 10 XLM for 30 days contributes
/// sqrt(10 * 30) ≈ 17, far more than 17 dust accounts cycling micro-stakes.
pub fn calculate_vouching_score(
    voucher_count: u32,          // kept for backwards-compatible call sites that only have count
    env: Option<&Env>,           // Some(env) enables stake-time weighting; None = legacy path
    borrower: Option<&Address>,  // borrower whose incoming vouches to examine
) -> u32 {
    // ── Stake-time path ─────────────────────────────────────────────────
    if let (Some(env), Some(borrower)) = (env, borrower) {
        let now = env.ledger().timestamp();

        // Load all vouches for this borrower
        let vouches: soroban_sdk::Vec<crate::types::VouchRecord> = env
            .storage()
            .persistent()
            .get(&crate::types::DataKey::Vouches(borrower.clone()))
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));

        let mut total_weight_scaled: u64 = 0; // sum of sqrt(stake_xlm * age_days) * 1000 (fixed-point)

        for v in vouches.iter() {
            // Floor 1: ignore trivial stakes
            if v.stake < SYBIL_MIN_STAKE_FOR_CREDIT {
                continue;
            }

            // Floor 2: ignore vouches younger than the minimum age
            let age_secs = now.saturating_sub(v.vouch_timestamp);
            if age_secs < SYBIL_MIN_VOUCH_AGE_SECS {
                continue;
            }

            // Convert to human-readable units to keep numbers manageable:
            // stake_xlm_units = stake / 10_000_000  (7 decimals)
            // age_days        = age_secs / 86400
            // We work in integer arithmetic: stake_deci_xlm (0.1 XLM units) × age_days
            let stake_deci_xlm = (v.stake / 1_000_000) as u64;  // 0.1 XLM units
            let age_days = (age_secs / 86_400) as u64;

            if stake_deci_xlm == 0 || age_days == 0 {
                continue;
            }

            // weight = stake_deci_xlm * age_days
            let weight = stake_deci_xlm.saturating_mul(age_days);

            // Integer sqrt (Babylonian method, 64-bit)
            let sqrt_weight = integer_sqrt_u64(weight);

            // Cap per-vouch contribution at SYBIL_STAKE_TIME_SATURATION
            let contribution = sqrt_weight.min(SYBIL_STAKE_TIME_SATURATION as u64);

            total_weight_scaled = total_weight_scaled.saturating_add(contribution);
        }

        // Saturate total at SYBIL_STAKE_TIME_SATURATION and scale to 0–1000
        let saturation = SYBIL_STAKE_TIME_SATURATION as u64;
        let total_capped = total_weight_scaled.min(saturation);
        let score = (total_capped * 1000 / saturation) as u32;
        return score.min(1000);
    }

    // ── Legacy fallback path (no env/borrower available) ────────────────
    // Kept so that unit tests that only test the scoring formula still compile.
    let max_vouches = 20u32;
    let score = (voucher_count.min(max_vouches) as u64 * 1000 / max_vouches as u64) as u32;
    score.min(1000)
}

/// Integer square root (floor) for u64, used in stake-time weight calculation.
/// Uses the Babylonian (Newton's method) algorithm.
pub fn integer_sqrt_u64(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Calculate repayment timeliness score component (0-1000).
pub fn calculate_timeliness_score(avg_repayment_time: i64) -> u32 {
    // Positive = early repayment, Negative = late repayment
    // Max benefit for 7 days early, max penalty for 7 days late
    let early_threshold: i64 = 7 * 24 * 60 * 60; // 7 days in seconds
    let late_threshold: i64 = -7 * 24 * 60 * 60; // 7 days late in seconds

    if avg_repayment_time >= early_threshold {
        return 1000;
    } else if avg_repayment_time <= late_threshold {
        return 0;
    } else {
        // Linear interpolation from late_threshold (0) to early_threshold (1000)
        let range = early_threshold - late_threshold; // = 14 * 24 * 60 * 60
        let position = avg_repayment_time - late_threshold;
        let score = (position as f64 / range as f64) * 1000.0;
        score.max(0.0).min(1000.0) as u32
    }
}

/// Helper: Calculate total borrowed amount across all borrower's loans.
fn calculate_total_borrowed(env: &Env, borrower: &Address) -> i128 {
    let _total_loans: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCount(borrower.clone()))
        .unwrap_or(0);
    
    let mut total: i128 = 0;
    let counter: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCounter)
        .unwrap_or(0);
    
    for loan_id in 1..=counter {
        if let Some(loan) = env
            .storage()
            .persistent()
            .get::<DataKey, LoanRecord>(&DataKey::Loan(loan_id))
        {
            if loan.borrower == *borrower {
                total = total.saturating_add(loan.amount);
            }
        }
    }
    
    total
}

/// Helper: Calculate total repaid amount across all borrower's loans.
fn calculate_total_repaid(env: &Env, borrower: &Address) -> i128 {
    let _total_loans: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCount(borrower.clone()))
        .unwrap_or(0);
    
    let mut total: i128 = 0;
    let counter: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCounter)
        .unwrap_or(0);
    
    for loan_id in 1..=counter {
        if let Some(loan) = env
            .storage()
            .persistent()
            .get::<DataKey, LoanRecord>(&DataKey::Loan(loan_id))
        {
            if loan.borrower == *borrower {
                total = total.saturating_add(loan.amount_repaid);
            }
        }
    }
    
    total
}

/// Helper: Calculate average repayment time (in seconds relative to deadline) across fully-repaid loans.
/// Returns positive value if repaid early (average time before deadline), negative if late.
fn calculate_avg_repayment_time(env: &Env, borrower: &Address) -> i64 {
    let counter: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCounter)
        .unwrap_or(0);
    
    let mut repayment_times: Vec<i64> = Vec::new(env);
    
    for loan_id in 1..=counter {
        if let Some(loan) = env
            .storage()
            .persistent()
            .get::<DataKey, LoanRecord>(&DataKey::Loan(loan_id))
        {
            if loan.borrower == *borrower && loan.status == LoanStatus::Repaid {
                if let Some(repayment_ts) = loan.repayment_timestamp {
                    // Calculate time to repayment relative to deadline
                    // Positive = early (repaid before deadline), Negative = late
                    let time_vs_deadline = (loan.deadline as i64) - (repayment_ts as i64);
                    repayment_times.push_back(time_vs_deadline);
                }
            }
        }
    }
    
    if repayment_times.len() == 0 {
        return 0; // No repaid loans, neutral timeliness
    }
    
    let sum: i64 = repayment_times.iter().sum();
    sum / repayment_times.len() as i64
}

/// Calculate overall credit score based on factors.
pub fn calculate_credit_score(
    env: &Env,
    borrower: &Address,
) -> Result<CreditScore, ContractError> {
    let config = get_credit_score_config(env);
    if !config.enabled {
        return Err(ContractError::CreditScoreCalculationFailed);
    }

    let factors = config.factors;

    // Get borrower statistics
    let total_loans: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCount(borrower.clone()))
        .unwrap_or(0);

    let successful_repayments: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::RepaymentCount(borrower.clone()))
        .unwrap_or(0);

    let defaults: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::DefaultCount(borrower.clone()))
        .unwrap_or(0);

    let account_age: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::BorrowerRegistered(borrower.clone()))
        .unwrap_or(env.ledger().timestamp());

    let account_age = env.ledger().timestamp() - account_age;

    let voucher_count: u32 = env
        .storage()
        .instance()
        .get::<DataKey, soroban_sdk::Vec<crate::types::VouchHistoryEntry>>(&DataKey::VoucherHistory(borrower.clone()))
        .map(|history| history.len())
        .unwrap_or(0);

    // Calculate component scores
    let repayment_history_score =
        calculate_repayment_history_score(successful_repayments, total_loans, defaults);
    let loan_count_score = calculate_loan_count_score(total_loans);
    let account_age_score = calculate_account_age_score(account_age);
    // Use stake-time-weighted vouching score (Sybil resistant)
    let vouching_score = calculate_vouching_score(voucher_count, Some(env), Some(borrower));
    
    // Calculate real timeliness from actual repayment history
    let avg_repayment_time_secs = calculate_avg_repayment_time(env, borrower);
    let timeliness_score = calculate_timeliness_score(avg_repayment_time_secs);

    // Weighted average
    let weighted_score = (repayment_history_score as u64 * factors.repayment_history_weight as u64
        + loan_count_score as u64 * factors.loan_count_weight as u64
        + account_age_score as u64 * factors.account_age_weight as u64
        + vouching_score as u64 * factors.vouching_weight as u64
        + timeliness_score as u64 * factors.timeliness_weight as u64)
        / 10000;

    let score = weighted_score as u32;
    let tier = calculate_tier(score);
    
    // Calculate real aggregates from borrower's loan history
    let total_borrowed = calculate_total_borrowed(env, borrower);
    let total_repaid = calculate_total_repaid(env, borrower);

    let credit_score = CreditScore {
        score,
        tier,
        last_updated: env.ledger().timestamp(),
        total_loans,
        successful_repayments,
        defaults,
        total_borrowed,
        total_repaid,
        account_age,
        voucher_count,
        avg_repayment_time: avg_repayment_time_secs,
    };

    Ok(credit_score)
}

/// Update credit score for a borrower.
pub fn update_credit_score(env: Env, borrower: Address) -> Result<(), ContractError> {
    let credit_score = calculate_credit_score(&env, &borrower)?;
    env.storage()
        .persistent()
        .set(&DataKey::CreditScore(borrower.clone()), &credit_score);

    env.events().publish(
        (symbol_short!("credit"), symbol_short!("update")),
        (borrower, credit_score.score, credit_score.tier),
    );

    Ok(())
}

/// Get credit score for a borrower.
pub fn get_credit_score(env: Env, borrower: Address) -> Option<CreditScore> {
    env.storage()
        .persistent()
        .get(&DataKey::CreditScore(borrower))
}

/// Get tier rewards for a given credit tier.
pub fn get_tier_rewards(env: Env, tier: CreditTier) -> TierRewards {
    let config = get_credit_score_config(&env);
    match tier {
        CreditTier::Poor => config.poor_rewards,
        CreditTier::Fair => config.fair_rewards,
        CreditTier::Good => config.good_rewards,
        CreditTier::VeryGood => config.very_good_rewards,
        CreditTier::Excellent => config.excellent_rewards,
    }
}

/// Set credit score configuration.
pub fn set_credit_score_config(
    env: Env,
    admin_signers: Vec<Address>,
    config: CreditScoreConfig,
) -> Result<(), ContractError> {
    crate::helpers::require_admin_approval(&env, &admin_signers);

    // Validate configuration
    let total_weight = config.factors.repayment_history_weight
        + config.factors.loan_count_weight
        + config.factors.account_age_weight
        + config.factors.vouching_weight
        + config.factors.timeliness_weight;

    if total_weight != 10000 {
        return Err(ContractError::InvalidCreditConfig);
    }

    env.storage()
        .instance()
        .set(&DataKey::CreditScoreConfig, &config);

    env.events().publish(
        (symbol_short!("credit"), symbol_short!("config")),
        admin_signers.get(0),
    );

    Ok(())
}

/// Get credit score configuration.
pub fn get_credit_score_config_view(env: Env) -> CreditScoreConfig {
    get_credit_score_config(&env)
}

/// Apply tier rewards to yield calculation.
pub fn apply_tier_rewards_to_yield(
    env: &Env,
    borrower: &Address,
    base_yield_bps: i128,
) -> i128 {
    let credit_score = match get_credit_score(env.clone(), borrower.clone()) {
        Some(score) => score,
        None => return base_yield_bps,
    };

    let rewards = get_tier_rewards(env.clone(), credit_score.tier);
    base_yield_bps + rewards.yield_bonus_bps as i128
}

/// Apply tier rewards to max loan amount.
pub fn apply_tier_rewards_to_max_loan(
    env: &Env,
    borrower: &Address,
    base_max_loan: i128,
) -> i128 {
    let credit_score = match get_credit_score(env.clone(), borrower.clone()) {
        Some(score) => score,
        None => return base_max_loan,
    };

    let rewards = get_tier_rewards(env.clone(), credit_score.tier);
    base_max_loan * rewards.max_loan_multiplier as i128 / 100
}

/// Apply tier rewards to minimum stake.
pub fn apply_tier_rewards_to_min_stake(
    env: &Env,
    borrower: &Address,
    base_min_stake: i128,
) -> i128 {
    let credit_score = match get_credit_score(env.clone(), borrower.clone()) {
        Some(score) => score,
        None => return base_min_stake,
    };

    let rewards = get_tier_rewards(env.clone(), credit_score.tier);
    let reduction = base_min_stake * rewards.min_stake_reduction_bps as i128 / 10000;
    base_min_stake - reduction.max(0)
}

/// Apply tier rewards to loan duration.
pub fn apply_tier_rewards_to_duration(
    env: &Env,
    borrower: &Address,
    base_duration: u64,
) -> u64 {
    let credit_score = match get_credit_score(env.clone(), borrower.clone()) {
        Some(score) => score,
        None => return base_duration,
    };

    let rewards = get_tier_rewards(env.clone(), credit_score.tier);
    base_duration + rewards.duration_extension
}

/// Apply tier rewards to protocol fee.
pub fn apply_tier_rewards_to_fee(
    env: &Env,
    borrower: &Address,
    base_fee_bps: u32,
) -> u32 {
    let credit_score = match get_credit_score(env.clone(), borrower.clone()) {
        Some(score) => score,
        None => return base_fee_bps,
    };

    let rewards = get_tier_rewards(env.clone(), credit_score.tier);
    let discount = base_fee_bps * rewards.fee_discount_bps as u32 / 10000;
    base_fee_bps - discount
}
