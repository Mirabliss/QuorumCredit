use crate::errors::ContractError;
use crate::helpers::config;
use crate::types::{
    DataKey, VouchRecord, VouchReputationWeight, VoucherStats, WeightedVouchDistribution,
    BPS_DENOMINATOR,
};
use soroban_sdk::{Address, Env, Vec};

/// Issue #1173: Calculate weighted vouch strength based on voucher reputation.
/// Formula: base_strength × (1 + (voucher_score / 1000))
/// Capped at 1.5x multiplier for vouchers with 1500+ reputation
pub fn calculate_weighted_vouch_strength(
    env: &Env,
    vouch_id: u64,
    voucher: &Address,
    base_strength: i128,
) -> Result<VouchReputationWeight, ContractError> {
    // Get voucher's reputation stats
    let stats: VoucherStats = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherStats(voucher.clone()))
        .unwrap_or(VoucherStats {
            successful_vouches: 0,
            total_vouches_slashed: 0,
            total_stake: 0,
            total_yield_earned: 0,
        });

    // Calculate reputation score (0-1000 scale)
    let reputation_score = calculate_reputation_score(&stats);

    // Calculate weight multiplier
    // Formula: 1 + (reputation_score / 1000), capped at 1.5x
    let weight_multiplier_bps = calculate_weight_multiplier_bps(reputation_score);

    // Calculate weighted strength
    let weighted_strength = apply_weight_multiplier(base_strength, weight_multiplier_bps)?;

    let weight_record = VouchReputationWeight {
        vouch_id,
        base_strength,
        voucher_reputation: reputation_score,
        weighted_strength,
        weight_multiplier_bps,
        calculated_at: env.ledger().timestamp(),
    };

    // Store the weight record for this vouch
    env.storage()
        .persistent()
        .set(&DataKey::VouchReputationWeight(vouch_id), &weight_record.clone());

    Ok(weight_record)
}

/// Issue #1173: Calculate reputation score from voucher stats.
/// Returns a score from 0-1000 based on successful vouch history.
fn calculate_reputation_score(stats: &VoucherStats) -> u32 {
    // Base score: 500 (neutral)
    let mut score = 500u32;

    // Increase score for successful vouches: +1 per successful vouch, max 250 points
    let success_bonus = (stats.successful_vouches as u32).min(250);
    score = score.saturating_add(success_bonus);

    // Decrease score for slashed vouches: -5 per slashed vouch, max 500 points
    let slash_penalty = (stats.total_vouches_slashed as u32 * 5).min(500);
    score = score.saturating_sub(slash_penalty);

    // Clamp score to 0-1000 range
    score.min(1000)
}

/// Issue #1173: Calculate weight multiplier in basis points.
/// Base multiplier is 1000 bps (1.0x).
/// For each 100 reputation points above 500, add 50 bps (up to 1500 bps max for 1.5x).
fn calculate_weight_multiplier_bps(reputation_score: u32) -> u32 {
    let base_multiplier = 1000u32;

    // Calculate additional multiplier based on reputation above neutral (500)
    if reputation_score >= 500 {
        let reputation_above_neutral = reputation_score - 500;
        let additional_bps = (reputation_above_neutral as u32 * 50) / 100; // 50 bps per 100 reputation points
        let max_additional = 500; // Cap at 500 bps additional (1.5x total)
        base_multiplier + additional_bps.min(max_additional)
    } else {
        // For reputation below 500, decrease multiplier: -50 bps per 100 reputation points
        let reputation_below_neutral = 500 - reputation_score;
        let penalty_bps = (reputation_below_neutral as u32 * 50) / 100;
        let min_multiplier = 500; // Floor at 500 bps (0.5x)
        base_multiplier.saturating_sub(penalty_bps).max(min_multiplier)
    }
}

/// Apply weight multiplier to base strength.
fn apply_weight_multiplier(base_strength: i128, multiplier_bps: u32) -> Result<i128, ContractError> {
    // weighted_strength = base_strength * multiplier_bps / 10_000
    let weighted = base_strength
        .checked_mul(multiplier_bps as i128)
        .ok_or(ContractError::ArithmeticOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ContractError::ArithmeticError)?;

    Ok(weighted)
}

/// Issue #1173: Update weighted vouch distribution for a borrower.
/// Recalculates the aggregate weighted strength for quorum calculations.
pub fn update_weighted_vouch_distribution(
    env: &Env,
    borrower: &Address,
    token: &Address,
    vouches: &Vec<VouchRecord>,
) -> Result<(), ContractError> {
    let mut total_base_stake: i128 = 0;
    let mut total_weighted_stake: i128 = 0;
    let mut total_weight_multiplier: u32 = 0;
    let vouch_count = vouches.len();

    for vouch in vouches.iter() {
        total_base_stake = total_base_stake
            .checked_add(vouch.stake)
            .ok_or(ContractError::ArithmeticOverflow)?;

        // Calculate weight for this vouch
        let weight_record = calculate_weighted_vouch_strength(env, 0, &vouch.voucher, vouch.stake)?;

        total_weighted_stake = total_weighted_stake
            .checked_add(weight_record.weighted_strength)
            .ok_or(ContractError::ArithmeticOverflow)?;

        total_weight_multiplier = total_weight_multiplier
            .saturating_add(weight_record.weight_multiplier_bps);
    }

    // Calculate average weight multiplier
    let average_weight_multiplier_bps = if vouch_count > 0 {
        total_weight_multiplier / vouch_count as u32
    } else {
        1000 // Default to 1.0x
    };

    let distribution = WeightedVouchDistribution {
        borrower: borrower.clone(),
        token: token.clone(),
        total_base_stake,
        total_weighted_stake,
        vouch_count: vouch_count as u32,
        average_weight_multiplier_bps,
        updated_at: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&DataKey::WeightedVouchDistribution(borrower.clone(), token.clone()), &distribution);

    Ok(())
}

/// Issue #1173: Get weighted vouch distribution for quorum calculations.
pub fn get_weighted_vouch_distribution(
    env: &Env,
    borrower: &Address,
    token: &Address,
) -> Result<WeightedVouchDistribution, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::WeightedVouchDistribution(borrower.clone(), token.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)
}

/// Issue #1173: Integrate weighted strength into quorum calculation.
/// Uses weighted stake instead of raw stake for quorum threshold checks.
pub fn calculate_weighted_quorum_stake(
    env: &Env,
    borrower: &Address,
    token: &Address,
) -> Result<i128, ContractError> {
    match get_weighted_vouch_distribution(env, borrower, token) {
        Ok(distribution) => Ok(distribution.total_weighted_stake),
        Err(_) => {
            // If no weighted distribution exists, return 0
            Ok(0)
        }
    }
}

/// Issue #1173: Get weight distribution statistics for analytics.
/// Returns statistics about how reputation weighting affects quorum.
pub fn get_weight_distribution_stats(
    env: &Env,
    borrower: &Address,
    token: &Address,
) -> Result<WeightDistributionStats, ContractError> {
    let distribution = get_weighted_vouch_distribution(env, borrower, token)?;

    let weight_impact_bps = if distribution.total_base_stake > 0 {
        (distribution.total_weighted_stake as u32 * BPS_DENOMINATOR as u32 / distribution.total_base_stake as u32).min(10000)
    } else {
        0
    };

    Ok(WeightDistributionStats {
        borrower: borrower.clone(),
        token: token.clone(),
        total_base_stake: distribution.total_base_stake,
        total_weighted_stake: distribution.total_weighted_stake,
        vouch_count: distribution.vouch_count,
        average_weight_multiplier_bps: distribution.average_weight_multiplier_bps,
        weight_impact_bps, // How much reputation weighting increases the total (in bps)
        updated_at: distribution.updated_at,
    })
}

#[derive(Clone)]
pub struct WeightDistributionStats {
    pub borrower: Address,
    pub token: Address,
    pub total_base_stake: i128,
    pub total_weighted_stake: i128,
    pub vouch_count: u32,
    pub average_weight_multiplier_bps: u32,
    pub weight_impact_bps: u32, // Impact of reputation weighting as percentage
    pub updated_at: u64,
}
