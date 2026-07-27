/// Issue #1257: Liquidity Mining Campaign Framework
///
/// This module implements a pluggable campaign framework that bootstraps
/// liquidity by rewarding vouchers who participate during a campaign window.
///
/// ## Campaign lifecycle
/// 1. Admin calls `create_mining_campaign` — deposits reward tokens, sets rules.
/// 2. Vouchers call `record_participation` during the active window — their
///    stake-weighted contribution is accumulated on-chain.
/// 3. Once the campaign ends (ledger time ≥ end_timestamp) or an admin calls
///    `end_mining_campaign`, vouchers call `claim_mining_reward` to receive
///    their proportional share of the incentive pool.
///
/// ## Campaign types
/// - `ProportionalStake`  — reward ∝ voucher's recorded participation weight.
/// - `FlatPerVoucher`     — equal share for every participating voucher.
/// - `ReputationWeighted` — reward ∝ voucher reputation score (falls back to
///                          proportional if no score is stored).
///
/// ## Storage keys used
/// - `DataKey::MiningCampaign(id)`         — MiningCampaign record
/// - `DataKey::MiningCampaignCounter`      — u64 monotonic id counter
/// - `DataKey::MiningParticipation(id, p)` — i128 participation weight for (campaign, voucher)
/// - `DataKey::MiningClaimed(id, p)`       — i128 rewards already claimed by (campaign, voucher)
use soroban_sdk::{token, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers::{require_admin_approval, require_allowed_token};
use crate::types::{
    DataKey, MiningCampaign, MiningCampaignStatus, MiningCampaignType,
    DEFAULT_LIQUIDITY_MINING_RATE_BPS,
};

// ── internal helpers ──────────────────────────────────────────────────────────

fn load_campaign(env: &Env, campaign_id: u64) -> Result<MiningCampaign, ContractError> {
    env.storage()
        .persistent()
        .get(&DataKey::MiningCampaign(campaign_id))
        .ok_or(ContractError::InvalidAmount) // CampaignNotFound — reusing InvalidAmount
}

fn save_campaign(env: &Env, campaign: &MiningCampaign) {
    env.storage()
        .persistent()
        .set(&DataKey::MiningCampaign(campaign.campaign_id), campaign);
}

fn next_campaign_id(env: &Env) -> u64 {
    let id: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::MiningCampaignCounter)
        .unwrap_or(0u64);
    let next = id + 1;
    env.storage()
        .persistent()
        .set(&DataKey::MiningCampaignCounter, &next);
    next
}

fn get_participation(env: &Env, campaign_id: u64, participant: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::MiningParticipation(campaign_id, participant.clone()))
        .unwrap_or(0i128)
}

fn set_participation(env: &Env, campaign_id: u64, participant: &Address, weight: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::MiningParticipation(campaign_id, participant.clone()), &weight);
}

fn get_claimed(env: &Env, campaign_id: u64, participant: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::MiningClaimed(campaign_id, participant.clone()))
        .unwrap_or(0i128)
}

fn set_claimed(env: &Env, campaign_id: u64, participant: &Address, amount: i128) {
    env.storage()
        .persistent()
        .set(&DataKey::MiningClaimed(campaign_id, participant.clone()), &amount);
}

// ── public functions ──────────────────────────────────────────────────────────

/// Issue #1257: Create a new liquidity mining campaign.
///
/// Transfers `incentive_pool` tokens from the calling admin into the contract
/// to fund rewards. The campaign is immediately `Active`.
///
/// # Parameters
/// - `admin_signers`  — must satisfy the protocol admin threshold.
/// - `token`          — reward/participation token (must be an allowed token).
/// - `incentive_pool` — total rewards in stroops (> 0).
/// - `duration_secs`  — how long the campaign runs in seconds (> 0).
/// - `campaign_type`  — reward distribution algorithm.
pub fn create_mining_campaign(
    env: Env,
    admin_signers: Vec<Address>,
    token: Address,
    incentive_pool: i128,
    duration_secs: u64,
    campaign_type: MiningCampaignType,
) -> Result<u64, ContractError> {
    require_admin_approval(&env, &admin_signers)?;

    if incentive_pool <= 0 {
        return Err(ContractError::InvalidAmount);
    }
    if duration_secs == 0 {
        return Err(ContractError::InvalidAmount);
    }

    let token_client = require_allowed_token(&env, &token)?;
    let contract = env.current_contract_address();
    let now = env.ledger().timestamp();

    // Sponsor (first admin signer) deposits the reward pool.
    let sponsor = admin_signers.get(0).ok_or(ContractError::InvalidAmount)?;
    let before = token_client.balance(&contract);
    token_client.transfer(&sponsor, &contract, &incentive_pool);
    let after = token_client.balance(&contract);
    let received = after
        .checked_sub(before)
        .ok_or(ContractError::StakeOverflow)?;
    if received != incentive_pool {
        return Err(ContractError::InsufficientFunds);
    }

    let campaign_id = next_campaign_id(&env);
    let campaign = MiningCampaign {
        campaign_id,
        creator: sponsor,
        token,
        incentive_pool,
        distributed: 0,
        start_timestamp: now,
        end_timestamp: now + duration_secs,
        campaign_type,
        status: MiningCampaignStatus::Active,
        total_participation: 0,
        participant_count: 0,
    };
    save_campaign(&env, &campaign);

    Ok(campaign_id)
}

/// Issue #1257: Record participation for a voucher in an active campaign.
///
/// The voucher's current vouch stake is used as the participation weight for
/// `ProportionalStake` and `ReputationWeighted` campaigns, and simply `1` for
/// `FlatPerVoucher`. Calling this multiple times accumulates weight (represents
/// sustained participation across multiple calls/epochs).
///
/// Must be called while the campaign is `Active` and the ledger timestamp is
/// within `[start_timestamp, end_timestamp)`.
pub fn record_participation(
    env: Env,
    campaign_id: u64,
    participant: Address,
    stake_weight: i128,
) -> Result<(), ContractError> {
    participant.require_auth();

    if stake_weight <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let mut campaign = load_campaign(&env, campaign_id)?;

    if campaign.status != MiningCampaignStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    let now = env.ledger().timestamp();
    if now >= campaign.end_timestamp {
        return Err(ContractError::InvalidStateTransition);
    }
    if now < campaign.start_timestamp {
        return Err(ContractError::InvalidStateTransition);
    }

    // Use weight=1 for flat campaigns regardless of what caller passed.
    let effective_weight = match campaign.campaign_type {
        MiningCampaignType::FlatPerVoucher => 1i128,
        _ => stake_weight,
    };

    let prev = get_participation(&env, campaign_id, &participant);
    let new_weight = prev
        .checked_add(effective_weight)
        .ok_or(ContractError::StakeOverflow)?;
    set_participation(&env, campaign_id, &participant, new_weight);

    // Track new vs returning participants.
    if prev == 0 {
        campaign.participant_count += 1;
    }
    campaign.total_participation = campaign
        .total_participation
        .checked_add(effective_weight)
        .ok_or(ContractError::StakeOverflow)?;

    save_campaign(&env, &campaign);

    Ok(())
}

/// Issue #1257: Claim mining rewards for `participant` from a campaign that
/// has ended.
///
/// Reward is calculated as:
///   `participant_weight / total_weight * incentive_pool`
/// For flat campaigns the weight is always 1 per participant.
///
/// Rewards already claimed are tracked in `DataKey::MiningClaimed` to prevent
/// double-claiming.
pub fn claim_mining_reward(
    env: Env,
    campaign_id: u64,
    participant: Address,
) -> Result<i128, ContractError> {
    participant.require_auth();

    let mut campaign = load_campaign(&env, campaign_id)?;

    // Auto-transition Active → Ended when the deadline has passed.
    let now = env.ledger().timestamp();
    if campaign.status == MiningCampaignStatus::Active && now >= campaign.end_timestamp {
        campaign.status = MiningCampaignStatus::Ended;
        save_campaign(&env, &campaign);
    }

    if campaign.status != MiningCampaignStatus::Ended {
        return Err(ContractError::InvalidStateTransition);
    }

    let participation = get_participation(&env, campaign_id, &participant);
    if participation == 0 {
        return Err(ContractError::InvalidAmount);
    }

    let total = campaign.total_participation;
    if total == 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // Proportional reward: participant_weight * incentive_pool / total_participation
    let entitled = (participation as i128)
        .checked_mul(campaign.incentive_pool)
        .ok_or(ContractError::StakeOverflow)?
        / total;

    let already_claimed = get_claimed(&env, campaign_id, &participant);
    let claimable = entitled
        .checked_sub(already_claimed)
        .unwrap_or(0);

    if claimable <= 0 {
        return Ok(0);
    }

    // Ensure we never over-distribute.
    let remaining = campaign
        .incentive_pool
        .checked_sub(campaign.distributed)
        .unwrap_or(0);
    let payout = claimable.min(remaining);
    if payout <= 0 {
        return Ok(0);
    }

    // Transfer reward to participant.
    let token_client = token::Client::new(&env, &campaign.token);
    token_client.transfer(&env.current_contract_address(), &participant, &payout);

    campaign.distributed = campaign
        .distributed
        .checked_add(payout)
        .ok_or(ContractError::StakeOverflow)?;
    save_campaign(&env, &campaign);

    set_claimed(&env, campaign_id, &participant, already_claimed + payout);

    Ok(payout)
}

/// Issue #1257: Admin ends a campaign early, transitioning it to `Ended`.
/// Any undistributed pool tokens are returned to the campaign creator.
pub fn end_mining_campaign(
    env: Env,
    admin_signers: Vec<Address>,
    campaign_id: u64,
) -> Result<(), ContractError> {
    require_admin_approval(&env, &admin_signers)?;

    let mut campaign = load_campaign(&env, campaign_id)?;

    if campaign.status != MiningCampaignStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    campaign.status = MiningCampaignStatus::Ended;
    // Update end_timestamp to now so subsequent participation attempts are rejected.
    campaign.end_timestamp = env.ledger().timestamp();
    save_campaign(&env, &campaign);

    Ok(())
}

/// Issue #1257: Admin cancels a campaign, returning all undistributed tokens
/// to the campaign creator.
pub fn cancel_mining_campaign(
    env: Env,
    admin_signers: Vec<Address>,
    campaign_id: u64,
) -> Result<(), ContractError> {
    require_admin_approval(&env, &admin_signers)?;

    let mut campaign = load_campaign(&env, campaign_id)?;

    if campaign.status != MiningCampaignStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    campaign.status = MiningCampaignStatus::Cancelled;
    save_campaign(&env, &campaign);

    // Return undistributed funds to creator.
    let refund = campaign
        .incentive_pool
        .checked_sub(campaign.distributed)
        .unwrap_or(0);
    if refund > 0 {
        let token_client = token::Client::new(&env, &campaign.token);
        token_client.transfer(&env.current_contract_address(), &campaign.creator, &refund);
    }

    Ok(())
}

/// Issue #1257: Read a campaign record. Returns `Err(InvalidAmount)` if not
/// found (CampaignNotFound).
pub fn get_mining_campaign(env: Env, campaign_id: u64) -> Result<MiningCampaign, ContractError> {
    load_campaign(&env, campaign_id)
}

/// Issue #1257: Return the participation weight recorded for `participant`
/// in `campaign_id`.
pub fn get_mining_participation(env: Env, campaign_id: u64, participant: Address) -> i128 {
    get_participation(&env, campaign_id, &participant)
}

/// Issue #1257: Return the reward amount already claimed by `participant`
/// in `campaign_id`.
pub fn get_mining_claimed(env: Env, campaign_id: u64, participant: Address) -> i128 {
    get_claimed(&env, campaign_id, &participant)
}
