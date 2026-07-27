//! Issue: Loan performance attribution analysis.
//!
//! Tracks the factors that plausibly drive a loan's outcome (borrower credit
//! score, vouch quality, sector, region) and computes how much each factor
//! contributed to the observed result. This is intentionally a heuristic,
//! on-chain-cheap model rather than a full statistical regression — Soroban
//! contracts cannot run arbitrary numerical libraries, so contribution
//! weights are fixed basis-point coefficients tuned to be directionally
//! correct rather than empirically fit.

use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Vec};

use crate::types::{DataKey, LoanRecord, LoanStatus};

/// Local storage keys for this module. Kept separate from the shared
/// `DataKey` enum so this feature can be added without touching existing
/// storage layouts or exhaustive matches elsewhere in the contract.
#[contracttype]
#[derive(Clone)]
pub enum AttrKey {
    /// loan_id -> PerformanceFactors
    Factors(u64),
    /// loan_id -> Attribution (cached result of the last analysis run)
    Attribution(u64),
    /// factor name (as a short symbol-like string) -> FactorAggregate
    FactorAggregate(String),
    /// monotonically increasing counter of loans that have recorded factors
    FactorLoanCount,
}

/// Snapshot of the drivers believed to influence a loan's outcome, captured
/// at (or shortly after) disbursement so they reflect conditions at
/// origination rather than being back-filled after the fact.
#[contracttype]
#[derive(Clone)]
pub struct PerformanceFactors {
    pub loan_id: u64,
    pub borrower: Address,
    /// Borrower credit score at time of recording (0-1000 scale).
    pub credit_score: u32,
    /// Aggregate vouch quality, in basis points of the vouch pool's
    /// reputation-weighted strength relative to raw stake (10_000 = neutral).
    pub vouch_quality_bps: u32,
    /// Borrower-declared or admin-tagged sector (e.g. "agriculture", "retail").
    pub sector: String,
    /// Borrower-declared or admin-tagged region (e.g. "west-africa").
    pub region: String,
    pub recorded_at: u64,
}

/// The realized outcome of a loan, derived from `LoanStatus`.
#[contracttype]
#[derive(Clone, PartialEq, Eq)]
pub enum LoanOutcome {
    Success,
    Failure,
    Pending,
}

/// The contribution of a single factor to a loan's outcome.
#[contracttype]
#[derive(Clone)]
pub struct FactorContribution {
    pub factor_name: String,
    /// Signed contribution in basis points; positive pushes toward success,
    /// negative pushes toward failure.
    pub contribution_bps: i128,
    /// Weight applied to this factor in the overall attribution model,
    /// in basis points of 10_000.
    pub weight_bps: u32,
}

/// Full attribution result for a single loan.
#[contracttype]
#[derive(Clone)]
pub struct Attribution {
    pub loan_id: u64,
    pub outcome: LoanOutcome,
    pub contributions: Vec<FactorContribution>,
    /// Aggregate weighted score across all factors, in basis points.
    /// Positive values are associated with successful outcomes.
    pub total_score_bps: i128,
    pub generated_at: u64,
}

/// Running aggregate of a single factor's historical contribution, used to
/// build factor-level performance reports and to seed the predictive model.
#[contracttype]
#[derive(Clone)]
pub struct FactorAggregate {
    pub factor_name: String,
    pub loans_observed: u32,
    pub successes: u32,
    pub failures: u32,
    /// Sum of contribution_bps across all observed loans for this factor.
    pub cumulative_contribution_bps: i128,
}

/// A performance report broken down by contributing factor.
#[contracttype]
#[derive(Clone)]
pub struct FactorPerformanceReport {
    pub factors: Vec<FactorAggregate>,
    pub total_loans_analyzed: u32,
    pub generated_at: u64,
}

const FACTOR_CREDIT_SCORE: &str = "credit_score";
const FACTOR_VOUCH_QUALITY: &str = "vouch_quality";
const FACTOR_SECTOR: &str = "sector";
const FACTOR_REGION: &str = "region";

/// Record the performance drivers for a loan. Should be called once, near
/// disbursement time, so the recorded factors reflect origination
/// conditions rather than being influenced by the eventual outcome.
pub fn record_performance_factors(
    env: Env,
    loan_id: u64,
    borrower: Address,
    credit_score: u32,
    vouch_quality_bps: u32,
    sector: String,
    region: String,
) -> PerformanceFactors {
    let factors = PerformanceFactors {
        loan_id,
        borrower,
        credit_score,
        vouch_quality_bps,
        sector,
        region,
        recorded_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&AttrKey::Factors(loan_id), &factors);

    let count: u32 = env
        .storage()
        .instance()
        .get(&AttrKey::FactorLoanCount)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&AttrKey::FactorLoanCount, &(count + 1));

    env.events().publish(
        (symbol_short!("attr"), symbol_short!("recorded")),
        (loan_id, credit_score, vouch_quality_bps),
    );

    factors
}

fn loan_outcome(status: &LoanStatus) -> LoanOutcome {
    match status {
        LoanStatus::Repaid => LoanOutcome::Success,
        LoanStatus::Defaulted | LoanStatus::PartialDefault => LoanOutcome::Failure,
        LoanStatus::ForgivenDefault => LoanOutcome::Failure,
        LoanStatus::Active | LoanStatus::None => LoanOutcome::Pending,
    }
}

/// Compute how much each tracked factor contributed to a loan's outcome.
///
/// The model is a simple, explainable weighted heuristic:
/// - Credit score is normalized against the 0-1000 scale and centered at
///   the 550 "fair" boundary, weighted 40%.
/// - Vouch quality is centered at the 10_000 bps neutral point, weighted 35%.
/// - Sector and region are qualitative tags; in the absence of an on-chain
///   distribution to compare against, they are recorded with neutral
///   (zero) contribution but full weight, so downstream aggregation
///   (`generate_factor_performance_report`) can accumulate directional
///   signal across many loans even when a single loan can't self-attribute.
pub fn analyze_loan_performance_attribution(env: Env, loan_id: u64) -> Attribution {
    let loan: Option<LoanRecord> = env.storage().persistent().get(&DataKey::Loan(loan_id));
    let outcome = match &loan {
        Some(l) => loan_outcome(&l.status),
        None => LoanOutcome::Pending,
    };

    let factors: Option<PerformanceFactors> =
        env.storage().persistent().get(&AttrKey::Factors(loan_id));

    let mut contributions: Vec<FactorContribution> = Vec::new(&env);
    let mut total_score_bps: i128 = 0;

    if let Some(f) = factors {
        // Credit score contribution: centered at 550, scaled to +/-4000 bps max.
        let credit_delta = f.credit_score as i128 - 550;
        let credit_contribution = (credit_delta * 4000) / 450; // 450 = 1000-550
        contributions.push_back(FactorContribution {
            factor_name: String::from_str(&env, FACTOR_CREDIT_SCORE),
            contribution_bps: credit_contribution,
            weight_bps: 4000,
        });
        total_score_bps += credit_contribution;

        // Vouch quality contribution: centered at 10_000 (neutral), scaled to +/-3500 bps.
        let quality_delta = f.vouch_quality_bps as i128 - 10_000;
        let quality_contribution = (quality_delta * 3500) / 10_000;
        contributions.push_back(FactorContribution {
            factor_name: String::from_str(&env, FACTOR_VOUCH_QUALITY),
            contribution_bps: quality_contribution,
            weight_bps: 3500,
        });
        total_score_bps += quality_contribution;

        // Sector and region: qualitative tags, tracked with neutral
        // per-loan contribution but real weight so the aggregate report
        // can surface directional trends once enough loans accumulate.
        contributions.push_back(FactorContribution {
            factor_name: f.sector.clone(),
            contribution_bps: 0,
            weight_bps: 1500,
        });
        contributions.push_back(FactorContribution {
            factor_name: f.region.clone(),
            contribution_bps: 0,
            weight_bps: 1000,
        });

        update_factor_aggregates(&env, &contributions, &outcome);
    }

    let attribution = Attribution {
        loan_id,
        outcome,
        contributions,
        total_score_bps,
        generated_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&AttrKey::Attribution(loan_id), &attribution);

    attribution
}

fn update_factor_aggregates(env: &Env, contributions: &Vec<FactorContribution>, outcome: &LoanOutcome) {
    if *outcome == LoanOutcome::Pending {
        // Don't pollute historical aggregates with unresolved loans.
        return;
    }

    for c in contributions.iter() {
        let key = AttrKey::FactorAggregate(c.factor_name.clone());
        let mut agg: FactorAggregate = env.storage().persistent().get(&key).unwrap_or(FactorAggregate {
            factor_name: c.factor_name.clone(),
            loans_observed: 0,
            successes: 0,
            failures: 0,
            cumulative_contribution_bps: 0,
        });

        agg.loans_observed += 1;
        agg.cumulative_contribution_bps += c.contribution_bps;
        match outcome {
            LoanOutcome::Success => agg.successes += 1,
            LoanOutcome::Failure => agg.failures += 1,
            LoanOutcome::Pending => {}
        }

        env.storage().persistent().set(&key, &agg);
    }
}

/// Generate a performance report aggregating all factors observed so far
/// across every loan that has been analyzed via
/// `analyze_loan_performance_attribution`.
pub fn generate_factor_performance_report(env: Env) -> FactorPerformanceReport {
    let names = [
        FACTOR_CREDIT_SCORE,
        FACTOR_VOUCH_QUALITY,
        FACTOR_SECTOR,
        FACTOR_REGION,
    ];

    let mut factors: Vec<FactorAggregate> = Vec::new(&env);
    let mut total_loans_analyzed: u32 = 0;

    for name in names.iter() {
        let key = AttrKey::FactorAggregate(String::from_str(&env, name));
        if let Some(agg) = env.storage().persistent().get::<AttrKey, FactorAggregate>(&key) {
            total_loans_analyzed = total_loans_analyzed.max(agg.loans_observed);
            factors.push_back(agg);
        }
    }

    FactorPerformanceReport {
        factors,
        total_loans_analyzed,
        generated_at: env.ledger().timestamp(),
    }
}

/// Predict the likelihood of successful repayment for a hypothetical loan,
/// given its factors, based on historical attribution data. Returns a
/// basis-point probability estimate (0-10_000).
///
/// This is a lightweight predictive model built directly from the
/// attribution aggregates: each factor's historical success rate is
/// blended, weighted by how many loans have been observed for that factor
/// (more observations => more confidence => more weight).
pub fn predict_loan_success_probability_bps(
    env: Env,
    credit_score: u32,
    vouch_quality_bps: u32,
    sector: String,
    region: String,
) -> u32 {
    let report = generate_factor_performance_report(env.clone());

    // Baseline prior: 50% success probability with no data.
    let mut weighted_sum: i128 = 5_000 * 100;
    let mut weight_total: i128 = 100;

    for agg in report.factors.iter() {
        if agg.loans_observed == 0 {
            continue;
        }
        let is_relevant = agg.factor_name == String::from_str(&env, FACTOR_CREDIT_SCORE)
            || agg.factor_name == String::from_str(&env, FACTOR_VOUCH_QUALITY)
            || agg.factor_name == sector
            || agg.factor_name == region;
        if !is_relevant {
            continue;
        }

        let success_rate_bps = (agg.successes as i128 * 10_000) / agg.loans_observed as i128;
        let confidence_weight = agg.loans_observed as i128;
        weighted_sum += success_rate_bps * confidence_weight;
        weight_total += confidence_weight;
    }

    // Fold in the raw input signal directly, since it may not yet be
    // reflected in the historical aggregates (e.g. a brand-new sector).
    let credit_signal_bps = (credit_score as i128 * 10_000) / 1000;
    weighted_sum += credit_signal_bps * 50;
    weight_total += 50;

    let quality_signal_bps = vouch_quality_bps as i128;
    weighted_sum += quality_signal_bps.min(20_000) * 30;
    weight_total += 30;

    let prediction = if weight_total > 0 {
        weighted_sum / weight_total
    } else {
        5_000
    };

    prediction.clamp(0, 10_000) as u32
}
