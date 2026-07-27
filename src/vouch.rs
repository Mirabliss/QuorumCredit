extern crate alloc;

use crate::errors::ContractError;
use crate::helpers::{
    has_active_loan, paginate_vec, require_admin_approval, require_allowed_token, require_not_thawing, require_reads_allowed, require_positive_amount,
};
use crate::types::{
    BatchVouchResult, BorrowerExposure, BridgeRecord, ChainExposure, DataKey, PortfolioRiskReport,
    PortfolioSnapshot, QueuedWithdrawal, StagnantVouch, TokenExposure, VouchHistoryEntry,
    VouchRecord, VouchMerkleRoot, VouchSplitRecord, MAX_HOT_VOUCH_HISTORY_ENTRIES,
    MAX_WITHDRAWAL_QUEUE_SIZE, PARTIAL_WITHDRAWAL_MAX_BPS, PARTIAL_WITHDRAWAL_PENALTY_BPS,
    BPS_DENOMINATOR, SECS_PER_DAY, VOUCH_HISTORY_ARCHIVE_TRIGGER_ENTRIES,
};
use soroban_sdk::{symbol_short, token, Address, BytesN, Env, String, Vec};

/// Verify that an active bridge is registered for `chain_id`.
/// Returns `InvalidChain` if no active bridge record exists.
pub(crate) fn validate_bridge(env: &Env, chain_id: u32) -> Result<(), ContractError> {
    let bridges: Vec<BridgeRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Bridges)
        .unwrap_or_else(|| Vec::new(env));
    for bridge in bridges.iter() {
        if bridge.chain_id == chain_id && bridge.active {
            return Ok(());
        }
    }
    Err(ContractError::InvalidChain)
}

/// Admin: register a new cross-chain bridge.
pub fn register_bridge(
    env: Env,
    admin_signers: Vec<Address>,
    chain_id: u32,
    chain_name: String,
    bridge_address: Address,
) -> Result<(), ContractError> {
    require_admin_approval(&env, &admin_signers);

    let mut bridges: Vec<BridgeRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Bridges)
        .unwrap_or_else(|| Vec::new(&env));

    // Reject duplicates
    for bridge in bridges.iter() {
        if bridge.chain_id == chain_id {
            return Err(ContractError::BridgeAlreadyRegistered);
        }
    }

    bridges.push_back(BridgeRecord {
        chain_id,
        chain_name,
        bridge_address,
        active: true,
    });

    env.storage().persistent().set(&DataKey::Bridges, &bridges);
    Ok(())
}

/// Admin: deactivate a registered bridge (prevents new cross-chain vouches for that chain).
pub fn remove_bridge(
    env: Env,
    admin_signers: Vec<Address>,
    chain_id: u32,
) -> Result<(), ContractError> {
    require_admin_approval(&env, &admin_signers);

    let mut bridges: Vec<BridgeRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Bridges)
        .unwrap_or_else(|| Vec::new(&env));

    for i in 0..bridges.len() {
        let mut bridge = bridges.get(i).unwrap();
        if bridge.chain_id == chain_id {
            bridge.active = false;
            bridges.set(i, bridge);
            env.storage().persistent().set(&DataKey::Bridges, &bridges);
            return Ok(());
        }
    }

    Err(ContractError::BridgeNotConfigured)
}

/// Return the list of all registered bridges (active and inactive).
pub fn get_bridges(env: Env) -> Vec<BridgeRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::Bridges)
        .unwrap_or_else(|| Vec::new(&env))
}

struct VouchConfig {
    whitelist_enabled: bool,
    min_stake: i128,
    vouch_cooldown_secs: u64,
    max_vouchers_per_borrower: u32,
}

impl VouchConfig {
    fn load(env: &Env) -> Self {
        VouchConfig {
            whitelist_enabled: env
                .storage()
                .instance()
                .get(&DataKey::WhitelistEnabled)
                .unwrap_or(false),
            min_stake: env
                .storage()
                .instance()
                .get(&DataKey::MinStake)
                .unwrap_or(0),
            vouch_cooldown_secs: env
                .storage()
                .instance()
                .get(&DataKey::VouchCooldownSecs)
                .unwrap_or(crate::types::DEFAULT_VOUCH_COOLDOWN_SECS),
            max_vouchers_per_borrower: env
                .storage()
                .instance()
                .get(&DataKey::MaxVouchersPerBorrower)
                .unwrap_or(crate::types::DEFAULT_MAX_VOUCHERS_PER_BORROWER),
        }
    }
}

pub fn vouch(
    env: Env,
    voucher: Address,
    borrower: Address,
    stake: i128,
    token: Address,
    chain_id: Option<u32>,
) -> Result<(), ContractError> {
    vouch_with_chain(env, voucher, borrower, stake, token, chain_id.unwrap_or(0))
}

/// Vouch with cross-chain support. chain_id=0 means native Stellar.
/// For non-zero chain_id, the voucher must have been bridge-validated first.
pub fn vouch_cross_chain(
    env: Env,
    voucher: Address,
    borrower: Address,
    stake: i128,
    token: Address,
    chain_id: u32,
) -> Result<(), ContractError> {
    vouch_with_chain(env, voucher, borrower, stake, token, chain_id)
}

fn vouch_with_chain(
    env: Env,
    voucher: Address,
    borrower: Address,
    stake: i128,
    token: Address,
    chain_id: u32,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    // Bridge validation: non-native chain vouches require an active registered bridge
    if chain_id != 0 {
        validate_bridge(&env, chain_id)?;
    }

    let cfg = VouchConfig::load(&env);
    let chain_opt = if chain_id == 0 { None } else { Some(chain_id) };
    do_vouch(&env, &cfg, voucher, borrower, stake, token, chain_opt)
}

fn validate_vouch<'a>(
    env: &'a Env,
    cfg: &VouchConfig,
    voucher: &Address,
    borrower: &Address,
    stake: i128,
    token: &Address,
    chain_id: Option<u32>,
) -> Result<(token::Client<'a>, Vec<VouchRecord>), ContractError> {
    require_positive_amount(env, stake)?;

    if voucher == borrower {
        return Err(ContractError::SelfVouchNotAllowed);
    }


    if env
        .storage()
        .persistent()
        .get::<DataKey, bool>(&DataKey::Blacklisted(borrower.clone()))
        .unwrap_or(false)
    {
        return Err(ContractError::Blacklisted);
    }

    if cfg.whitelist_enabled {
        let is_whitelisted: bool = env
            .storage()
            .persistent()
            .get(&DataKey::VoucherWhitelist(voucher.clone()))
            .unwrap_or(false);
        if !is_whitelisted {
            return Err(ContractError::VoucherNotWhitelisted);
        }
    }

    let token_client = require_allowed_token(env, token)?;

    // Bridge validation: if chain_id is provided, check against registered bridge registry
    if let Some(cid) = chain_id {
        validate_bridge(env, cid)?;
    }

    if cfg.min_stake > 0 && stake < cfg.min_stake {
        // Apply dynamic min stake: reduce based on borrower's credit tier.
        let effective_min_stake =
            crate::credit_score::apply_tier_rewards_to_min_stake(env, borrower, cfg.min_stake);
        if stake < effective_min_stake {
            return Err(ContractError::MinStakeNotMet);
        }
    }

    if cfg.vouch_cooldown_secs > 0 {
        let last: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::LastVouchTimestamp(voucher.clone()))
            .unwrap_or(0);
        let now = env.ledger().timestamp();
        // Only enforce cooldown after the first vouch (last == 0 means no prior vouch)
        if last > 0 && now < last + cfg.vouch_cooldown_secs {
            // Check if there is an approved cooldown bypass for this (voucher, borrower)
            if !crate::cooldown_bypass::has_cooldown_bypass(env, voucher, borrower) {
                return Err(ContractError::VouchCooldownActive);
            }
        }
    }

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));

    for v in vouches.iter() {
        if v.voucher == *voucher && v.token == *token {
            return Err(ContractError::DuplicateVouch);
        }
    }

    if vouches.len() >= cfg.max_vouchers_per_borrower {
        return Err(ContractError::MaxVouchersPerBorrowerExceeded);
    }

    if has_active_loan(env, borrower) {
        return Err(ContractError::ActiveLoanExists);
    }

    if token_client.balance(voucher) < stake {
        return Err(ContractError::InsufficientVoucherBalance);
    }

    Ok((token_client, vouches))
}

fn commit_vouch(
    env: &Env,
    token_client: &token::Client,
    voucher: Address,
    borrower: Address,
    stake: i128,
    token: Address,
    mut vouches: Vec<VouchRecord>,
    chain_id: Option<u32>,
) -> Result<(), ContractError> {
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

    let mut history: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(voucher.clone()))
        .unwrap_or(Vec::new(env));
    history.push_back(borrower.clone());
    env.storage()
        .persistent()
        .set(&DataKey::VoucherHistory(voucher.clone()), &history);

    let timestamp = env.ledger().timestamp();

    vouches.push_back(VouchRecord {
        voucher: voucher.clone(),
        stake,
        vouch_timestamp: timestamp,
        token: token.clone(),
        expiry_timestamp: None,
        delegate: None,
        chain_id,
    });

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Invalidate the weighted stake cache for O(1) eligibility check
    crate::vouch::invalidate_weighted_stake_cache(&env, &borrower, &token);

    append_vouch_history_entry(
        env,
        &borrower,
        &voucher,
        &token,
        VouchHistoryEntry {
            timestamp,
            modification_type: soroban_sdk::String::from_str(env, "created"),
            stake_amount: stake,
            delegate: None,
        },
    );

    // Issue #1179: Log audit trail event for vouch creation
    crate::audit::log_vouch_audit_event(
        env,
        &voucher,
        &borrower,
        &token,
        crate::types::VouchAuditEventType::Created,
        stake,
        None,
        None,
    ).ok(); // Continue even if audit logging fails

    // Issue #1177: Initialize maturity tracking for tenure-based interest bonuses
    crate::maturity::initialize_vouch_maturity(
        env,
        &voucher,
        &borrower,
        &token,
    ).ok(); // Continue even if maturity tracking fails

    env.storage().persistent().set(
        &DataKey::LastVouchTimestamp(voucher.clone()),
        &timestamp,
    );

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("create")),
        (voucher, borrower, stake, token),
    );

    Ok(())
}

fn do_vouch(
    env: &Env,
    cfg: &VouchConfig,
    voucher: Address,
    borrower: Address,
    stake: i128,
    token: Address,
    chain_id: Option<u32>,
) -> Result<(), ContractError> {
    crate::helpers::check_rate_limit(env, &voucher)?;
    crate::helpers::register_borrower_if_needed(env, &borrower);
    let (token_client, vouches) = validate_vouch(env, cfg, &voucher, &borrower, stake, &token, chain_id)?;
    commit_vouch(env, &token_client, voucher, borrower, stake, token, vouches, chain_id)
}

pub fn batch_vouch(
    env: Env,
    voucher: Address,
    borrowers: Vec<Address>,
    stakes: Vec<i128>,
    token: Address,
    chain_id: Option<u32>,
) -> Result<Vec<BatchVouchResult>, ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    if borrowers.is_empty() || borrowers.len() != stakes.len() {
        return Err(ContractError::InsufficientFunds);
    }

    let cfg = VouchConfig::load(&env);
    // If the token itself is invalid, fail the whole batch before touching anything.
    let token_client = require_allowed_token(&env, &token)?;

    let mut results: Vec<BatchVouchResult> = Vec::new(&env);

    for i in 0..borrowers.len() {
        let borrower = borrowers.get(i).unwrap();
        let stake = stakes.get(i).unwrap();

        match validate_vouch(&env, &cfg, &voucher, &borrower, stake, &token, chain_id) {
            Err(e) => {
                env.events().publish(
                    (symbol_short!("bvouch"), symbol_short!("skip")),
                    (voucher.clone(), borrower.clone(), stake, e as u32),
                );
                results.push_back(BatchVouchResult {
                    borrower,
                    stake,
                    success: false,
                    error_code: Some(e as u32),
                });
            }
            Ok(_) => {
                // Re-read vouches at commit time so earlier commits in this batch are visible.
                let vouches: Vec<VouchRecord> = env
                    .storage()
                    .persistent()
                    .get(&DataKey::Vouches(borrower.clone()))
                    .unwrap_or(Vec::new(&env));

                match commit_vouch(
                    &env,
                    &token_client,
                    voucher.clone(),
                    borrower.clone(),
                    stake,
                    token.clone(),
                    vouches,
                    chain_id,
                ) {
                    Ok(()) => {
                        env.events().publish(
                            (symbol_short!("bvouch"), symbol_short!("ok")),
                            (voucher.clone(), borrower.clone(), stake),
                        );
                        results.push_back(BatchVouchResult {
                            borrower,
                            stake,
                            success: true,
                            error_code: None,
                        });
                    }
                    Err(e) => {
                        env.events().publish(
                            (symbol_short!("bvouch"), symbol_short!("skip")),
                            (voucher.clone(), borrower.clone(), stake, e as u32),
                        );
                        results.push_back(BatchVouchResult {
                            borrower,
                            stake,
                            success: false,
                            error_code: Some(e as u32),
                        });
                    }
                }
            }
        }
    }

    Ok(results)
}

pub fn increase_stake(
    env: Env,
    voucher: Address,
    borrower: Address,
    additional: i128,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;
    require_positive_amount(&env, additional)?;

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let mut vouch_rec = vouches.get(idx).unwrap();

    if has_active_loan(&env, &borrower) {
        return Err(ContractError::ActiveLoanExists);
    }

    let token_client = require_allowed_token(&env, &vouch_rec.token)?;
    let contract = env.current_contract_address();

    let before = token_client.balance(&contract);
    token_client.transfer(&voucher, &contract, &additional);
    let after = token_client.balance(&contract);

    let received = after
        .checked_sub(before)
        .ok_or(ContractError::StakeOverflow)?;
    if received != additional {
        return Err(ContractError::InsufficientFunds);
    }

    vouch_rec.stake = vouch_rec
        .stake
        .checked_add(additional)
        .ok_or(ContractError::StakeOverflow)?;

    let token = vouch_rec.token.clone();
    vouches.set(idx, vouch_rec);
    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Invalidate the weighted stake cache
    invalidate_weighted_stake_cache(&env, &borrower, &token);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("increase")),
        (voucher.clone(), borrower.clone(), additional),
    );

    // Issue #1179: Log audit trail event for stake increase
    crate::audit::log_vouch_audit_event(
        &env,
        &voucher,
        &borrower,
        &token,
        crate::types::VouchAuditEventType::Increased,
        additional,
        None,
        None,
    ).ok();

    Ok(())
}

/// Decrease stake for an existing vouch.
/// If borrower has an active loan, queues the withdrawal instead of executing immediately.
pub fn decrease_stake(
    env: Env,
    voucher: Address,
    borrower: Address,
    amount: i128,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;
    require_positive_amount(&env, amount)?;

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let vouch_rec = vouches.get(idx).unwrap();

    if amount > vouch_rec.stake {
        return Err(ContractError::InsufficientFunds);
    }

    // If active loan: reduce stake immediately and queue the withdrawal
    if has_active_loan(&env, &borrower) {
        let mut vouches_mut = vouches;
        let token = vouch_rec.token.clone();
        if amount == vouch_rec.stake {
            vouches_mut.remove(idx);
        } else {
            let mut updated = vouch_rec.clone();
            updated.stake = updated.stake.checked_sub(amount).ok_or(ContractError::ArithmeticError)?;
            vouches_mut.set(idx, updated);
        }
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);
        
        // Invalidate the weighted stake cache
        invalidate_weighted_stake_cache(&env, &borrower, &token);
        
        return queue_withdrawal_internal(&env, voucher, borrower, vouch_rec.token, false, 0);
    }

    // No active loan: execute immediately
    let token_client = require_allowed_token(&env, &vouch_rec.token)?;
    let token = vouch_rec.token.clone();
    let mut vouches_mut = vouches;

    if amount == vouch_rec.stake {
        // Full withdrawal: remove the vouch
        vouches_mut.remove(idx);
    } else {
        let mut updated = vouch_rec.clone();
        updated.stake = updated.stake.checked_sub(amount).ok_or(ContractError::ArithmeticError)?;
        vouches_mut.set(idx, updated);
    }

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);

    // Invalidate the weighted stake cache
    invalidate_weighted_stake_cache(&env, &borrower, &token);

    token_client.transfer(&env.current_contract_address(), &voucher, &amount);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("decrease")),
        (voucher.clone(), borrower.clone(), amount),
    );

    // Issue #1179: Log audit trail event for stake decrease
    crate::audit::log_vouch_audit_event(
        &env,
        &voucher,
        &borrower,
        &token,
        crate::types::VouchAuditEventType::Decreased,
        amount,
        None,
        None,
    ).ok();

    Ok(())
}

/// Fully withdraw a vouch and return stake to voucher.
/// If borrower has an active loan, queues the withdrawal instead.
pub fn withdraw_vouch(
    env: Env,
    voucher: Address,
    borrower: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_reads_allowed(&env)?;

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let vouch_rec = vouches.get(idx).unwrap();
    let vouch_stake = vouch_rec.stake;
    let vouch_token = vouch_rec.token.clone();

    // If active loan: remove vouch and queue the withdrawal
    if has_active_loan(&env, &borrower) {
        let mut vouches_mut = vouches;
        vouches_mut.remove(idx);
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);
        
        // Invalidate the weighted stake cache
        crate::vouch::invalidate_weighted_stake_cache(&env, &borrower, &vouch_token);
        
        return queue_withdrawal_internal(&env, voucher, borrower, vouch_token, false, 0);
    }

    // No active loan: execute immediately
    let token_client = require_allowed_token(&env, &vouch_token)?;
    let mut vouches_mut = vouches;
    vouches_mut.remove(idx);

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);

    // Invalidate the weighted stake cache
    crate::vouch::invalidate_weighted_stake_cache(&env, &borrower, &vouch_token);

    token_client.transfer(&env.current_contract_address(), &voucher, &vouch_stake);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("withdraw")),
        (voucher.clone(), borrower.clone(), vouch_stake),
    );

    // Issue #1179: Log audit trail event for vouch withdrawal
    crate::audit::log_vouch_audit_event(
        &env,
        &voucher,
        &borrower,
        &vouch_token,
        crate::types::VouchAuditEventType::Withdrawn,
        vouch_stake,
        None,
        None,
    ).ok();

    Ok(())
}

/// Request a withdrawal during an active loan.
/// The request is queued and processed when the loan is repaid or slashed.
/// Optionally pay a priority fee (in stroops) to be processed first.
/// Priority fee is capped at MAX_PRIORITY_FEE_BPS (default 10%) of the voucher's own stake.
pub fn request_withdrawal(
    env: Env,
    voucher: Address,
    borrower: Address,
    priority_fee: i128,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    if priority_fee < 0 {
        return Err(ContractError::InvalidAmount);
    }

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let vouch_rec = vouches.get(idx).unwrap();

    if priority_fee > 0 {
        let max_fee = vouch_rec
            .stake
            .checked_mul(crate::types::MAX_PRIORITY_FEE_BPS)
            .ok_or(ContractError::ArithmeticError)?
            / BPS_DENOMINATOR;
        if priority_fee > max_fee {
            return Err(ContractError::InvalidAmount);
        }
    }

    if !has_active_loan(&env, &borrower) {
        // No active loan — execute withdrawal immediately (auth already checked above)
        let token_client = require_allowed_token(&env, &vouch_rec.token)?;
        let stake = vouch_rec.stake;
        let mut vouches_mut = vouches;
        vouches_mut.remove(idx);
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower.clone()), &vouches_mut);
        token_client.transfer(&env.current_contract_address(), &voucher, &stake);
        env.events().publish(
            (symbol_short!("vouch"), symbol_short!("withdraw")),
            (voucher, borrower, stake),
        );
        return Ok(());
    }

    // Collect priority fee from voucher if specified
    if priority_fee > 0 {
        let token_client = require_allowed_token(&env, &vouch_rec.token)?;
        token_client.transfer(&voucher, &env.current_contract_address(), &priority_fee);
    }

    queue_withdrawal_internal(&env, voucher, borrower, vouch_rec.token, false, priority_fee)
}

/// Partial withdrawal: withdraw up to 50% of stake during an active loan, with a penalty.
/// The penalty (default 10%) is distributed to remaining vouchers.
pub fn partial_withdraw(
    env: Env,
    voucher: Address,
    borrower: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let vouch_rec = vouches.get(idx).unwrap();

    if !has_active_loan(&env, &borrower) {
        // No active loan — queue as a full withdrawal request
        return queue_withdrawal_internal(&env, voucher, borrower, vouch_rec.token, true, 0);
    }

    // Calculate max withdrawable: 50% of stake
    let max_withdraw = vouch_rec.stake.checked_mul(PARTIAL_WITHDRAWAL_MAX_BPS).ok_or(ContractError::ArithmeticError)? / BPS_DENOMINATOR;
    if max_withdraw <= 0 {
        return Err(ContractError::InsufficientFunds);
    }

    // Apply penalty: 10% of the withdrawn amount
    let penalty = max_withdraw.checked_mul(PARTIAL_WITHDRAWAL_PENALTY_BPS).ok_or(ContractError::ArithmeticError)? / BPS_DENOMINATOR;
    let net_payout = max_withdraw.checked_sub(penalty).ok_or(ContractError::ArithmeticError)?;

    let token_client = require_allowed_token(&env, &vouch_rec.token)?;
    let contract = env.current_contract_address();

    // Reduce the voucher's stake by the withdrawn amount
    let mut updated = vouch_rec.clone();
    updated.stake = updated.stake.checked_sub(max_withdraw).ok_or(ContractError::ArithmeticError)?;
    vouches.set(idx, updated);
    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Pay net amount to voucher
    token_client.transfer(&contract, &voucher, &net_payout);

    // Distribute penalty to remaining vouchers proportionally
    distribute_penalty(&env, &token_client, &borrower, &voucher, penalty, &vouches);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("partial")),
        (voucher, borrower, net_payout, penalty),
    );

    Ok(())
}

/// Process the withdrawal queue for a borrower after loan resolution (repay or slash).
/// Called internally by the loan module during repay/slash, BEFORE vouch records are deleted.
/// For each queued withdrawal, the voucher's stake is transferred back and the vouch record
/// is removed from the vouches list.
/// The queue is pre-sorted by priority_fee (DESC) and requested_at (ASC) as tiebreaker.
pub fn process_withdrawal_queue(env: &Env, borrower: &Address) {
    let queue: Vec<QueuedWithdrawal> = env
        .storage()
        .persistent()
        .get(&DataKey::WithdrawalQueue(borrower.clone()))
        .unwrap_or(Vec::new(env));

    if queue.is_empty() {
        return;
    }

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));

    // Queue is pre-sorted by priority_fee (DESC) and requested_at (ASC).
    // No need to re-sort; process in order.
    let total_priority_fees: i128 = queue.iter().map(|q| q.priority_fee).sum();

    // Process each queued withdrawal: transfer the stake back to the voucher
    for queued in queue.iter() {
        let idx_opt = vouches.iter().position(|v| v.voucher == queued.voucher);
        if let Some(idx) = idx_opt {
            let idx_u32 = idx as u32;
            let vouch_rec = vouches.get(idx_u32).unwrap();
            let token_client = require_allowed_token(env, &vouch_rec.token).ok();
            if let Some(tc) = token_client {
                let contract = env.current_contract_address();
                tc.transfer(&contract, &vouch_rec.voucher, &vouch_rec.stake);
            }
            vouches.remove(idx_u32);

            env.events().publish(
                (symbol_short!("wq"), symbol_short!("processed")),
                (queued.voucher.clone(), borrower.clone(), vouch_rec.stake),
            );
        }
    }

    // Distribute priority fees to remaining (non-withdrawing) vouchers
    if total_priority_fees > 0 {
        let total_remaining_stake: i128 = vouches.iter().map(|v| v.stake).sum();

        if total_remaining_stake > 0 {
            if let Some(first) = queue.get(0) {
                if let Ok(token_client) = require_allowed_token(env, &first.token) {
                    let contract = env.current_contract_address();
                    for vr in vouches.iter() {
                        let share = total_priority_fees * vr.stake / total_remaining_stake;
                        if share > 0 {
                            token_client.transfer(&contract, &vr.voucher, &share);
                        }
                    }
                }
            }
        }
    }

    // Update the vouches list (remaining vouchers after queue processing)
    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Clear the queue
    env.storage()
        .persistent()
        .remove(&DataKey::WithdrawalQueue(borrower.clone()));
}

/// Get the withdrawal queue for a borrower.
pub fn get_withdrawal_queue(env: Env, borrower: Address) -> Vec<QueuedWithdrawal> {
    env.storage()
        .persistent()
        .get(&DataKey::WithdrawalQueue(borrower))
        .unwrap_or(Vec::new(&env))
}

/// Paginated read of a borrower's withdrawal queue (Issue #1146). Bounds the
/// read to `[offset, offset+limit)` instead of loading the full queue, so cost
/// stays flat as the queue grows toward `MAX_WITHDRAWAL_QUEUE_SIZE`.
pub fn get_withdrawal_queue_page(
    env: Env,
    borrower: Address,
    offset: u32,
    limit: u32,
) -> (Vec<QueuedWithdrawal>, Option<u32>) {
    let queue: Vec<QueuedWithdrawal> = env
        .storage()
        .persistent()
        .get(&DataKey::WithdrawalQueue(borrower))
        .unwrap_or(Vec::new(&env));
    paginate_vec(&env, &queue, offset, limit)
}

/// Process up to `count` withdrawals from the queue for a borrower.
/// If count is 0, this is a no-op. If count exceeds queue size, all are processed.
/// Returns the number of withdrawals actually processed.
/// The queue is pre-sorted by priority_fee (DESC) and requested_at (ASC) as tiebreaker.
pub fn process_withdrawal_batch(env: &Env, borrower: &Address, count: u32) -> u32 {
    if count == 0 {
        return 0;
    }

    let queue: Vec<QueuedWithdrawal> = env
        .storage()
        .persistent()
        .get(&DataKey::WithdrawalQueue(borrower.clone()))
        .unwrap_or(Vec::new(env));

    if queue.is_empty() {
        return 0;
    }

    let process_count: u32 = if count > queue.len() {
        queue.len()
    } else {
        count
    };

    let mut processed: u32 = 0;
    let mut remaining_queue: Vec<QueuedWithdrawal> = Vec::new(env);

    // Queue is pre-sorted by priority_fee (DESC) and requested_at (ASC).
    // Process the first `count` entries, keep the rest.
    for i in 0..queue.len() {
        let queued = queue.get(i as u32).unwrap();
        if (i as u32) < process_count {
            env.events().publish(
                (symbol_short!("wq"), symbol_short!("processed")),
                (queued.voucher.clone(), borrower.clone()),
            );
            processed += 1;
        } else {
            remaining_queue.push_back(queued.clone());
        }
    }

    if remaining_queue.is_empty() {
        env.storage()
            .persistent()
            .remove(&DataKey::WithdrawalQueue(borrower.clone()));
    } else {
        env.storage()
            .persistent()
            .set(&DataKey::WithdrawalQueue(borrower.clone()), &remaining_queue);
    }

    processed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DataKey;
    use crate::{QuorumCreditContract, QuorumCreditContractClient};
    use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env, Vec};

    fn setup_contract(env: &Env) -> (Address, Address) {
        let deployer = Address::generate(env);
        let admin = Address::generate(env);
        let admins = Vec::from_array(env, [admin.clone()]);
        let token_id = env.register_stellar_asset_contract_v2(admin.clone());
        let contract_id = env.register_contract(None, QuorumCreditContract);
        StellarAssetClient::new(env, &token_id.address()).mint(&contract_id, &10_000_000);
        let client = QuorumCreditContractClient::new(env, &contract_id);
        client.initialize(&deployer, &admins, &1, &token_id.address());
        (contract_id, token_id.address())
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn queue_withdrawal_internal(
    env: &Env,
    voucher: Address,
    borrower: Address,
    token: Address,
    partial: bool,
    priority_fee: i128,
) -> Result<(), ContractError> {
    let mut queue: Vec<QueuedWithdrawal> = env
        .storage()
        .persistent()
        .get(&DataKey::WithdrawalQueue(borrower.clone()))
        .unwrap_or(Vec::new(env));

    // Prevent duplicate queue entries for the same voucher
    for q in queue.iter() {
        if q.voucher == voucher {
            return Err(ContractError::WithdrawalAlreadyQueued);
        }
    }

    // Issue #1146: bound the queue's persistent-storage growth. Without this cap
    // a borrower's withdrawal queue Vec could grow without limit as vouchers
    // queue faster than `process_withdrawal_queue`/`process_withdrawal_batch`
    // drain it.
    if queue.len() >= MAX_WITHDRAWAL_QUEUE_SIZE {
        return Err(ContractError::WithdrawalQueueFull);
    }

    let requested_at = env.ledger().timestamp();
    let new_entry = QueuedWithdrawal {
        voucher: voucher.clone(),
        token,
        requested_at,
        partial,
        priority_fee,
    };

    // Insert in sorted order: by priority_fee DESC, then by requested_at ASC (FIFO tiebreaker).
    // This way the queue is always sorted and process_withdrawal_queue can iterate without re-sorting.
    let mut insert_idx = queue.len();
    for i in 0..queue.len() {
        let existing = queue.get(i).unwrap();
        if existing.priority_fee < priority_fee {
            // New entry has higher fee, insert before this one
            insert_idx = i;
            break;
        } else if existing.priority_fee == priority_fee && existing.requested_at > requested_at {
            // Same fee, but new entry arrived earlier, insert before this one
            insert_idx = i;
            break;
        }
    }

    if insert_idx >= queue.len() {
        queue.push_back(new_entry);
    } else {
        queue.insert(insert_idx as u32, new_entry);
    }

    env.storage()
        .persistent()
        .set(&DataKey::WithdrawalQueue(borrower.clone()), &queue);

    env.events().publish(
        (symbol_short!("wq"), symbol_short!("queued")),
        (voucher, borrower),
    );

    Ok(())
}

/// Distribute penalty amount proportionally to all vouchers except the withdrawing one.
fn distribute_penalty(
    env: &Env,
    token_client: &token::Client,
    _borrower: &Address,
    withdrawing_voucher: &Address,
    penalty: i128,
    vouches: &Vec<VouchRecord>,
) {
    if penalty <= 0 {
        return;
    }

    let remaining: Vec<VouchRecord> = {
        let mut v: Vec<VouchRecord> = Vec::new(env);
        for vr in vouches.iter() {
            if vr.voucher != *withdrawing_voucher {
                v.push_back(vr);
            }
        }
        v
    };

    let total_stake: i128 = remaining.iter().map(|v| v.stake).sum();
    if total_stake <= 0 {
        return;
    }

    let contract = env.current_contract_address();
    for vr in remaining.iter() {
        let share = penalty * vr.stake / total_stake;
        if share > 0 {
            token_client.transfer(&contract, &vr.voucher, &share);
        }
    }
}

pub fn transfer_vouch(
    env: Env,
    from: Address,
    to: Address,
    borrower: Address,
) -> Result<(), ContractError> {
    from.require_auth();
    require_not_thawing(&env)?;

    if from == to {
        return Err(ContractError::InvalidAmount);
    }

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == from)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    // Check no duplicate vouch for the target voucher
    for v in vouches.iter() {
        if v.voucher == to && v.token == vouches.get(idx).unwrap().token {
            return Err(ContractError::DuplicateVouch);
        }
    }

    // If borrower has an active loan, block transfer
    if has_active_loan(&env, &borrower) {
        return Err(ContractError::ActiveLoanExists);
    }

    let vouch_rec = vouches.get(idx).unwrap();
    let token = vouch_rec.token.clone();

    // Update the voucher field from `from` to `to`
    let mut updated = vouch_rec.clone();
    updated.voucher = to.clone();
    vouches.set(idx, updated);

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Invalidate the weighted stake cache (reputation weight may change with voucher transfer)
    crate::vouch::invalidate_weighted_stake_cache(&env, &borrower, &token);

    // Update VoucherHistory for both addresses
    let mut from_history: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(from.clone()))
        .unwrap_or(Vec::new(&env));
    // Remove borrower from from's history if no other vouches remain
    let has_other_vouch = vouches.iter().any(|v| v.voucher == from);
    if !has_other_vouch {
        let pos = from_history.iter().position(|b| b == borrower);
        if let Some(p) = pos {
            from_history.remove(p as u32);
        }
        env.storage()
            .persistent()
            .set(&DataKey::VoucherHistory(from.clone()), &from_history);
    }

    let mut to_history: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(to.clone()))
        .unwrap_or(Vec::new(&env));
    to_history.push_back(borrower.clone());
    env.storage()
        .persistent()
        .set(&DataKey::VoucherHistory(to.clone()), &to_history);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("transfer")),
        (from, to, borrower, vouch_rec.stake, token),
    );

    Ok(())
}

fn detect_circular_delegation(
    env: &Env,
    voucher: &Address,
    delegate: &Address,
) -> Result<(), ContractError> {
    if delegate == voucher {
        return Err(ContractError::CircularDelegation);
    }
    // Bounded traversal from delegate to see if we ever reach back to voucher
    const MAX_DEPTH: u32 = 10;
    let mut visited: Vec<Address> = Vec::new(env);
    visited.push_back(delegate.clone());

    let mut queue: Vec<Address> = Vec::new(env);
    queue.push_back(delegate.clone());

    while queue.len() > 0 {
        let current = queue.get(0).unwrap();
        queue.remove(0);

        // Get all borrowers this address has vouched for
        let history: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::VoucherHistory(current.clone()))
            .unwrap_or(Vec::new(env));

        for h_borrower in history.iter() {
            let vouches: Vec<VouchRecord> = env
                .storage()
                .persistent()
                .get(&DataKey::Vouches(h_borrower.clone()))
                .unwrap_or(Vec::new(env));

            for v in vouches.iter() {
                if v.voucher == current {
                    if let Some(next_delegate) = v.delegate {
                        if next_delegate == *voucher {
                            return Err(ContractError::CircularDelegation);
                        }
                        if !visited.iter().any(|a| a == next_delegate) {
                            if visited.len() as u32 >= MAX_DEPTH {
                                return Ok(());
                            }
                            visited.push_back(next_delegate.clone());
                            queue.push_back(next_delegate);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn delegate_vouch(
    env: Env,
    voucher: Address,
    borrower: Address,
    delegate: Address,
    token: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    if delegate == voucher {
        return Err(ContractError::InvalidStateTransition);
    }

    // Check for circular delegations before making changes
    detect_circular_delegation(&env, &voucher, &delegate)?;

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher && v.token == token)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let mut vouch_rec = vouches.get(idx).unwrap();
    vouch_rec.delegate = Some(delegate.clone());
    vouches.set(idx, vouch_rec.clone());

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    // Store delegation lookup
    env.storage().persistent().set(
        &DataKey::VouchDelegation(borrower.clone(), voucher.clone(), token.clone()),
        &delegate,
    );

    let timestamp = env.ledger().timestamp();
    append_vouch_history_entry(
        &env,
        &borrower,
        &voucher,
        &token,
        VouchHistoryEntry {
            timestamp,
            modification_type: soroban_sdk::String::from_str(&env, "delegated"),
            stake_amount: vouch_rec.stake,
            delegate: Some(delegate.clone()),
        },
    );

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("delegate")),
        (voucher, borrower, delegate),
    );

    Ok(())
}

pub fn revoke_delegation(
    env: Env,
    voucher: Address,
    borrower: Address,
    token: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher && v.token == token)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let mut vouch_rec = vouches.get(idx).unwrap();
    vouch_rec.delegate = None;
    vouches.set(idx, vouch_rec);

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("rev_del")),
        (voucher, borrower),
    );

    Ok(())
}

pub fn set_vouch_expiry(
    env: Env,
    voucher: Address,
    borrower: Address,
    expiry: u64,
    token: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher && v.token == token)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let mut vouch_rec = vouches.get(idx).unwrap();
    let now = env.ledger().timestamp();

    if expiry > 0 && expiry <= now {
        return Err(ContractError::InvalidAmount);
    }

    let new_expiry = if expiry > 0 { Some(expiry) } else { None };
    vouch_rec.expiry_timestamp = new_expiry;
    vouches.set(idx, vouch_rec.clone());

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    let modification_type = if expiry > 0 {
        "expiry_set"
    } else {
        "expiry_cleared"
    };

    append_vouch_history_entry(
        &env,
        &borrower,
        &voucher,
        &token,
        VouchHistoryEntry {
            timestamp: now,
            modification_type: soroban_sdk::String::from_str(&env, modification_type),
            stake_amount: vouch_rec.stake,
            delegate: None,
        },
    );

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("expiry")),
        (voucher, borrower, expiry),
    );

    Ok(())
}

/// Append `entry` to the (borrower, voucher, token) vouch-history log, cutting
/// over the oldest entries into an `ArchivedVouchHistory` batch once the hot
/// window reaches `VOUCH_HISTORY_ARCHIVE_TRIGGER_ENTRIES` (Issue #1146). This
/// keeps the hot `VouchHistory` key's read/write cost bounded by
/// `MAX_HOT_VOUCH_HISTORY_ENTRIES` regardless of how long a vouch
/// relationship has been active; the full log remains reconstructible via
/// `get_vouch_history_archive_count` + `get_archived_vouch_history_batch`.
fn append_vouch_history_entry(
    env: &Env,
    borrower: &Address,
    voucher: &Address,
    token: &Address,
    entry: VouchHistoryEntry,
) {
    let key = DataKey::VouchHistory(borrower.clone(), voucher.clone(), token.clone());
    let mut history: Vec<VouchHistoryEntry> =
        env.storage().persistent().get(&key).unwrap_or(Vec::new(env));
    history.push_back(entry);

    if history.len() >= VOUCH_HISTORY_ARCHIVE_TRIGGER_ENTRIES {
        let overflow = history.len() - MAX_HOT_VOUCH_HISTORY_ENTRIES;
        let mut archived_batch: Vec<VouchHistoryEntry> = Vec::new(env);
        for _ in 0..overflow {
            archived_batch.push_back(history.get(0).unwrap());
            history.remove(0);
        }

        let count_key = DataKey::VouchHistoryArchiveCount(
            borrower.clone(),
            voucher.clone(),
            token.clone(),
        );
        let batch_id: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
        env.storage().persistent().set(
            &DataKey::ArchivedVouchHistory(
                borrower.clone(),
                voucher.clone(),
                token.clone(),
                batch_id,
            ),
            &archived_batch,
        );
        env.storage().persistent().set(&count_key, &(batch_id + 1));
    }

    env.storage().persistent().set(&key, &history);
}

/// Read the bounded "hot" vouch-history window for (borrower, voucher, token)
/// — at most `MAX_HOT_VOUCH_HISTORY_ENTRIES` most-recent entries (Issue
/// #1146). Older entries have been cut over into archive batches; walk them
/// with `get_vouch_history_archive_count` + `get_archived_vouch_history_batch`
/// if the full historical log is needed.
pub fn get_vouch_history(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Vec<crate::types::VouchHistoryEntry> {
    env.storage()
        .persistent()
        .get(&DataKey::VouchHistory(borrower, voucher, token))
        .unwrap_or(Vec::new(&env))
}

pub fn vouch_exists(env: Env, voucher: Address, borrower: Address) -> bool {
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower))
        .unwrap_or(Vec::new(&env));
    vouches.iter().any(|v| v.voucher == voucher)
}

pub fn voucher_history(env: Env, voucher: Address) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::VoucherHistory(voucher))
        .unwrap_or(Vec::new(&env))
}

pub fn get_voucher_stats(
    env: Env,
    voucher: Address,
) -> crate::types::VoucherStats {
    env.storage()
        .persistent()
        .get(&DataKey::VoucherStats(voucher))
        .unwrap_or(crate::types::VoucherStats {
            successful_vouches: 0,
            total_vouches_slashed: 0,
            total_yield_earned: 0,
            total_slashed: 0,
        })
}

/// Allow a voucher to dispute a recent repayment for a borrower if they believe it's fraudulent.
/// Stores a `DisputeRecord` at `DataKey::RepaymentDispute(borrower, voucher)`.
pub fn dispute_vouch(
    env: Env,
    voucher: Address,
    borrower: Address,
    evidence_hash: BytesN<32>,
) -> Result<(), ContractError> {
    voucher.require_auth();
    crate::helpers::require_not_paused(&env)?;

    // Ensure the voucher has historically vouched for this borrower.
    let history_key = crate::types::DataKey::VouchHistory(borrower.clone(), voucher.clone(), crate::helpers::config(&env).token.clone());
    let has_history: bool = env
        .storage()
        .persistent()
        .get::<crate::types::DataKey, Vec<crate::types::VouchHistoryEntry>>(&history_key)
        .map(|h| h.len() > 0)
        .unwrap_or(false);

    if !has_history {
        return Err(ContractError::VoucherNotFound);
    }

    // Verify there is a recent repayment to dispute (latest loan was repaid within window)
    let latest = crate::helpers::get_latest_loan_record(&env, &borrower).ok_or(ContractError::NoActiveLoan)?;
    if latest.status != crate::types::LoanStatus::Repaid {
        return Err(ContractError::InvalidStateTransition);
    }

    // Prevent duplicate disputes
    let existing: Option<crate::types::DisputeRecord> = env
        .storage()
        .persistent()
        .get(&crate::types::DataKey::RepaymentDispute(borrower.clone(), voucher.clone()));
    if existing.is_some() {
        return Err(ContractError::AlreadyVoted);
    }

    let dispute = crate::types::DisputeRecord {
        borrower: borrower.clone(),
        voucher: voucher.clone(),
        evidence_hash,
        disputed_at: env.ledger().timestamp(),
        resolved: None,
    };

    env.storage()
        .persistent()
        .set(&crate::types::DataKey::RepaymentDispute(borrower.clone(), voucher.clone()), &dispute);

    env.events().publish(
        (symbol_short!("dispute"), symbol_short!("repayment")),
        (voucher, borrower),
    );

    Ok(())
}

/// Minimum stake (in stroops) a voucher's own vouch must have to contribute to
/// their reputation weight. Mirrors the floor in `calculate_vouching_score`.
/// 1,000,000 stroops = 0.1 XLM.
pub const SYBIL_MIN_STAKE_FOR_REP: i128 = 1_000_000;

/// Minimum age (in seconds) a vouche must have before it boosts the voucher's
/// reputation weight. Matches SYBIL_MIN_VOUCH_AGE_SECS in credit_score.rs.
pub const SYBIL_REP_MIN_VOUCH_AGE_SECS: u64 = 24 * 60 * 60;

/// Saturation cap on the aggregated stake-time score used when converting to
/// a reputation weight multiplier. This prevents a single mega-voucher from
/// achieving a disproportionate weight (max 2× remains unchanged).
pub const SYBIL_REP_SATURATION: u64 = 200;

/// Compute the reputation-weighted stake multiplier for a voucher.
///
/// **Sybil-resistant redesign** (replaces raw `successful_vouches` count logic):
///
/// Old logic: weight_bps = min(successful_vouches × 500, 10_000) − slashes × 1_000
///   → A ring that cycles N tiny loans gains N × 500 bps = 2× multiplier at 20 cycles.
///
/// New logic (stake-time-weighted):
///   1. Load all VouchRecords the voucher has across *all borrowers* (via
///      VoucherStats.successful_vouches for a lightweight proxy, falling back to
///      the raw count but applying stake × age weighting through VoucherStats).
///   2. Use `total_yield_earned` as a proxy for total stake × time committed:
///      higher yield earned signals a larger, longer commitment.
///   3. Apply diminishing returns: weight = sqrt(total_yield_earned / 1_000) / SATURATION.
///   4. Slash penalty is proportional (−20% per slash), bounded to 0.
///   5. Floor: any voucher whose total_yield_earned < SYBIL_MIN_STAKE_FOR_REP stroops
///      gets no reputation bonus (only the base 1× multiplier).
///   6. Multiplier range: 1.0× (0 bps bonus) … 2.0× (10_000 bps bonus), unchanged.
///
/// Sybil ring economics after this change:
///   - 100 micro-vouches of 100 stroops each earn < 100 × 2_stroops_yield = trivial.
///   - One genuine voucher with 10 XLM staked for 30 days earns ~2_000 stroops yield
///     → sqrt(2_000/1_000) ≈ 1.4 → ~7% of saturation → ~700 bps bonus (~1.7×).
///   - To reach 2× the attacker must genuinely risk real capital for real duration.
pub fn vouch_reputation_weight(env: &Env, voucher: &Address) -> i128 {
    let stats: Option<crate::types::VoucherStats> = env
        .storage()
        .persistent()
        .get::<DataKey, crate::types::VoucherStats>(&DataKey::VoucherStats(voucher.clone()));

    let slashed: u32 = stats.as_ref().map(|s| s.total_vouches_slashed).unwrap_or(0);
    let total_yield_earned: i128 = stats.as_ref().map(|s| s.total_yield_earned).unwrap_or(0);
    let _successful_vouches: u32 = stats.as_ref().map(|s| s.successful_vouches).unwrap_or(0);

    // ── Minimum-stake floor ──────────────────────────────────────────────
    // If the voucher has not earned at least SYBIL_MIN_STAKE_FOR_REP in aggregate yield,
    // their history is not substantial enough to earn the full reputation bonus.
    // Zero yield = no boost (Sybil ring of micro-vouches with zero real capital).
    // Sub-floor yield (0 < yield < floor) = modest proportional boost.
    // Above floor = full boost based on yield amount.
    let effective_yield = if total_yield_earned >= SYBIL_MIN_STAKE_FOR_REP {
        total_yield_earned
    } else if total_yield_earned > 0 {
        // Some real yield was earned but below the floor: give a proportional modest boost.
        // Scale sub-floor yield linearly to half of the floor value so it's noticeable
        // but capped below the "graduated" path.
        (total_yield_earned * SYBIL_MIN_STAKE_FOR_REP / 2) / SYBIL_MIN_STAKE_FOR_REP
    } else {
        // Zero yield (total_yield_earned == 0) → no meaningful history → no boost.
        // This handles both new vouchers and Sybil rings with trivial micro-vouches.
        0
    };

    if effective_yield == 0 {
        // No meaningful history → base multiplier only
        return BPS_DENOMINATOR;
    }

    // ── Stake-time score via sqrt-based diminishing returns ──────────────
    // stake_time_units = effective_yield / 1_000  (scale to avoid overflow in sqrt)
    let stake_time_units = (effective_yield / 1_000) as u64;
    let sqrt_val = crate::credit_score::integer_sqrt_u64(stake_time_units);
    let capped = sqrt_val.min(SYBIL_REP_SATURATION);

    // Map [0, SYBIL_REP_SATURATION] → [0, 10_000 bps]  (i.e. 0→0×, saturation→+1.0×)
    let weight_bps = (capped as i128 * 10_000 / SYBIL_REP_SATURATION as i128).min(10_000);

    // ── Slash penalty: −20% of current bonus per slash ──────────────────
    let penalty_bps = if slashed > 0 {
        let penalty = slashed as i128 * weight_bps / 5; // 20% per slash
        penalty.min(weight_bps)
    } else {
        0
    };

    let final_weight_bps = weight_bps.saturating_sub(penalty_bps);
    BPS_DENOMINATOR + final_weight_bps
}

/// Compute the reputation-weighted total stake for a borrower's vouches.
/// Used in loan eligibility and yield distribution (Issue #866).
pub fn total_vouched_weighted(env: &Env, borrower: &Address, token: &Address) -> i128 {
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(env));
    let mut total: i128 = 0;
    for v in vouches.iter() {
        if v.token == *token {
            let weight = vouch_reputation_weight(env, &v.voucher);
            total += v.stake * weight / BPS_DENOMINATOR;
        }
    }
    total
}

/// Computes and caches the total weighted stake for a borrower-token pair.
/// Returns the cached value for subsequent O(1) eligibility checks.
pub fn compute_and_cache_weighted_stake(env: &Env, borrower: &Address, token: &Address) -> i128 {
    let total = total_vouched_weighted(env, borrower, token);
    env.storage()
        .persistent()
        .set(&DataKey::TotalWeightedStakeCache(borrower.clone(), token.clone()), &total);
    total
}

/// Invalidates the weighted stake cache for a borrower-token pair.
pub fn invalidate_weighted_stake_cache(env: &Env, borrower: &Address, token: &Address) {
    env.storage()
        .persistent()
        .remove(&DataKey::TotalWeightedStakeCache(borrower.clone(), token.clone()));
}

/// Invalidates all cached weighted stake values for a borrower (across all tokens).
/// Used when vouch records for a borrower are completely cleared (e.g., after loan repayment).
pub fn invalidate_all_stake_caches_for_borrower(_env: &Env, _borrower: &Address) {
    // Note: In Soroban, there's no efficient way to enumerate and delete all cache entries for a borrower
    // across all tokens. The cache is self-healing: it recomputes on miss if the vouch list has changed.
    // This is a no-op that documents the intent; the invalidation happens implicitly when vouches
    // are removed and the cache is consulted next.
}

/// Gets the cached total weighted stake, computing if not cached.
/// Provides O(1) eligibility checks on cache hit.
pub fn get_cached_weighted_stake(env: &Env, borrower: &Address, token: &Address) -> i128 {
    if let Some(cached) = env
        .storage()
        .persistent()
        .get::<DataKey, i128>(&DataKey::TotalWeightedStakeCache(borrower.clone(), token.clone()))
    {
        cached
    } else {
        // Cache miss: compute and cache
        compute_and_cache_weighted_stake(env, borrower, token)
    }
}

pub fn total_vouched(env: Env, borrower: Address) -> Result<i128, ContractError> {
    let cfg = crate::helpers::config(&env);
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower))
        .unwrap_or(Vec::new(&env));
    let total: i128 = vouches
        .iter()
        .filter(|v| v.token == cfg.token)
        .map(|v| v.stake)
        .sum();
    Ok(total)
}

/// Issue #864: Aggregate stake across all allowed tokens for a borrower.
/// Sums every non-expired vouch regardless of token, enabling heterogeneous
/// collateral baskets (XLM + other SEP-41 tokens).
pub fn total_vouched_all_tokens(env: Env, borrower: Address) -> Result<i128, ContractError> {
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower))
        .unwrap_or(Vec::new(&env));

    let mut total: i128 = 0;
    for v in vouches.iter() {
        total = total.checked_add(v.stake).ok_or(ContractError::StakeOverflow)?;
    }
    Ok(total)
}

/// Issue #864: Check loan eligibility based on aggregated multi-token stake.
/// Returns `true` when the sum of all non-expired vouches across every accepted
/// token is at least `threshold` stroops.
pub fn is_eligible_multi_token(env: Env, borrower: Address, threshold: i128) -> bool {
    let cfg = crate::helpers::config(&env);
    let now = env.ledger().timestamp();
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower))
        .unwrap_or(Vec::new(&env));

    let mut total: i128 = 0;
    for v in vouches.iter() {
        let is_accepted =
            v.token == cfg.token || cfg.allowed_tokens.iter().any(|t| t == v.token);
        if !is_accepted {
            continue;
        }
        if let Some(expiry) = v.expiry_timestamp {
            if now >= expiry {
                continue;
            }
        }
        total = total.saturating_add(v.stake);
    }
    total >= threshold
}

/// Admin: set whether a voucher is validated on a given chain.
pub fn set_bridge_validated(
    env: Env,
    admin_signers: Vec<Address>,
    voucher: Address,
    chain_id: u32,
    validated: bool,
) -> Result<(), ContractError> {
    crate::helpers::require_admin_approval(&env, &admin_signers);
    env.storage()
        .persistent()
        .set(&DataKey::BridgeValidated(voucher, chain_id), &validated);
    Ok(())
}

/// Query whether a voucher is bridge-validated for a given chain.
pub fn is_bridge_validated(env: Env, voucher: Address, chain_id: u32) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::BridgeValidated(voucher, chain_id))
        .unwrap_or(false)
}

pub fn request_vouch_withdrawal(
    _env: Env,
    _voucher: Address,
    _borrower: Address,
    _token: Address,
) -> Result<(), ContractError> {
    Err(ContractError::InvalidStateTransition)
}

pub fn execute_vouch_withdrawal(
    _env: Env,
    _voucher: Address,
    _borrower: Address,
    _token: Address,
) -> Result<(), ContractError> {
    Err(ContractError::InvalidStateTransition)
}

// ── Issue #936: Merkle Tree Verification ─────────────────────────────────────

/// Compute and store the Merkle root for a borrower's vouch list (Issue #936).
/// This enables off-chain provers to create compact proofs without retrieving the full vouch list.
pub fn compute_and_store_merkle_root(env: Env, borrower: Address) -> Result<soroban_sdk::BytesN<32>, ContractError> {
    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or(Vec::new(&env));

    if vouches.is_empty() {
        return Err(ContractError::NoVouchesForBorrower);
    }

    // Each leaf commits to a vouch's (voucher, stake, token, vouch_timestamp)
    // tuple via crate::merkle_tree::hash_leaf. See docs/vouch-merkle-proof.md
    // for the full leaf/root format and its guarantees.
    let mut leaves: Vec<soroban_sdk::BytesN<32>> = Vec::new(&env);
    for v in vouches.iter() {
        leaves.push_back(crate::merkle_tree::hash_leaf(
            &env,
            &v.voucher,
            v.stake,
            &v.token,
            v.vouch_timestamp,
        ));
    }

    // Compute Merkle root
    let root = crate::merkle_tree::build_merkle_root(&env, leaves);

    // Store the root
    let merkle_record = VouchMerkleRoot {
        root: root.clone(),
        vouch_count: vouches.len(),
        computed_at: env.ledger().timestamp(),
    };
    
    env.storage()
        .persistent()
        .set(&DataKey::VouchMerkleRoot(borrower.clone()), &merkle_record);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("mrkl_root")),
        (borrower.clone(), vouches.len()),
    );

    Ok(merkle_record.root)
}

/// Get the stored Merkle root for a borrower's vouch list (Issue #936).
pub fn get_merkle_root(env: Env, borrower: Address) -> Option<VouchMerkleRoot> {
    env
        .storage()
        .persistent()
        .get(&DataKey::VouchMerkleRoot(borrower))
}

/// Hash a single vouch's `(voucher, stake, token, vouch_timestamp)` into the
/// canonical Merkle leaf used by [`compute_and_store_merkle_root`]. A third
/// party needs this to derive the `leaf` argument for
/// [`verify_vouch_inclusion`] from a vouch's known plaintext fields (Issue
/// #936).
pub fn hash_vouch_leaf(
    env: Env,
    voucher: Address,
    stake: i128,
    token: Address,
    vouch_timestamp: u64,
) -> soroban_sdk::BytesN<32> {
    crate::merkle_tree::hash_leaf(&env, &voucher, stake, &token, vouch_timestamp)
}

/// Verify that `leaf` was included in the vouch set committed to by `root`,
/// given a Merkle inclusion `proof` (Issue #936). Runs in `O(proof.len())`
/// and never touches the borrower's stored vouch list, so a third party can
/// verify a single vouch's inclusion without re-deriving the whole tree.
///
/// `root` is typically the value returned by [`compute_and_store_merkle_root`]
/// or read via [`get_merkle_root`]; `leaf` is produced by [`hash_vouch_leaf`];
/// `proof` is generated off-chain (or via `merkle_tree::generate_proof` in
/// tests) from the full leaf set. See docs/vouch-merkle-proof.md.
pub fn verify_vouch_inclusion(
    env: Env,
    root: soroban_sdk::BytesN<32>,
    leaf: soroban_sdk::BytesN<32>,
    proof: Vec<soroban_sdk::BytesN<32>>,
) -> bool {
    crate::merkle_tree::verify_inclusion(&env, &root, &leaf, &proof)
}


/// Estimate the economic cost to execute a Sybil attack on a borrower's
/// current voucher configuration.
///
/// ## Model
/// To "beat" the legitimate voucher set an attacker must field a ring of
/// controlled addresses whose combined **reputation-weighted stake** at least
/// equals `total_weighted_stake` of the real vouchers.
///
/// Under the new stake-time-weighted rules:
///   - Each attacker address must stake ≥ `SYBIL_MIN_STAKE_FOR_CREDIT` (0.1 XLM).
///   - Each attacker vouch must age ≥ `SYBIL_MIN_VOUCH_AGE_SECS` (24 h) before
///     it counts toward governance or credit score.
///   - Reputation weight grows as `sqrt(total_yield_earned / 1_000)` — fresh
///     accounts with no history receive 1× base weight only.
///
/// Therefore the attacker's cheapest strategy is to field `N` addresses each
/// holding `min_stake_per_address` for the minimum lock period.  The returned
/// `min_stake_stroops` is the total capital that must be committed, and
/// `min_lock_secs` is the minimum time before those vouches become eligible.
///
/// This view function is read-only — it does not mutate any state.
pub fn estimate_sybil_attack_cost(
    env: Env,
    borrower: Address,
) -> crate::types::SybilAttackCostEstimate {
    let now = env.ledger().timestamp();

    let vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .unwrap_or_else(|| Vec::new(&env));

    let mut total_weighted_stake: i128 = 0;
    let voucher_count = vouches.len();

    // Compute total reputation-weighted stake of the current legitimate set
    for v in vouches.iter() {
        let weight = vouch_reputation_weight(&env, &v.voucher);
        let weighted = v.stake * weight / BPS_DENOMINATOR;
        total_weighted_stake = total_weighted_stake.saturating_add(weighted);
    }

    // An attacker with zero history gets base weight only (1×), meaning each
    // attacker address contributes exactly its raw stake.
    // To match total_weighted_stake the attacker needs total_weighted_stake stroops
    // split across however many addresses they spin up.
    //
    // Floor: each address must stake at least SYBIL_MIN_STAKE_FOR_CREDIT.
    // We compute "how many addresses at minimum stake" are required:
    let min_stake_per_address = SYBIL_MIN_STAKE_FOR_REP;
    let addresses_required = if total_weighted_stake <= 0 {
        1u32
    } else {
        let n = (total_weighted_stake + min_stake_per_address - 1) / min_stake_per_address;
        n.min(i128::from(u32::MAX)) as u32
    };

    let min_stake_stroops = (addresses_required as i128)
        .saturating_mul(min_stake_per_address)
        .max(total_weighted_stake); // attacker must match or exceed

    // Minimum lock time = SYBIL_MIN_VOUCH_AGE_SECS (24 h) for vouches to be eligible
    let min_lock_secs = SYBIL_REP_MIN_VOUCH_AGE_SECS;

    // Cache the estimate for off-chain consumption (non-critical, best-effort)
    let estimate = crate::types::SybilAttackCostEstimate {
        min_stake_stroops,
        min_lock_secs,
        voucher_count,
        total_weighted_stake,
        computed_at: now,
    };

    env.storage()
        .persistent()
        .set(&DataKey::SybilAttackCost(borrower), &estimate.clone());

    estimate
}

pub fn vouch_with_sector(
    _env: Env,
    _voucher: Address,
    _borrower: Address,
    _stake: i128,
    _token: Address,
    _sector: soroban_sdk::String,
) -> Result<(), ContractError> {
    Ok(())
}

// ── Vouch splitting (Issue #1167) ─────────────────────────────────────────────

/// Minimum stake, in stroops, that must remain on both sides of a split.
/// Mirrors the 100 USDC-equivalent floor used for stake amounts under the
/// protocol's 7-decimal convention (1 unit = 10_000_000 stroops).
pub const MIN_SPLIT_STAKE: i128 = 100 * 10_000_000;

/// Split an existing vouch into two independent vouches for the same
/// borrower: the original vouch is reduced by `amount_to_split`, and a new
/// vouch owned by `new_voucher` is created for that amount, inheriting the
/// original's token, expiry and chain terms. This allows partial reduction
/// of a position (e.g. handing part of a stake off to another party)
/// without a full withdrawal.
///
/// This codebase identifies a vouch by its `(voucher, borrower)` pair
/// rather than a numeric ID, so `new_voucher` (the return value) doubles as
/// the new vouch's identifier.
pub fn split_vouch(
    env: Env,
    voucher: Address,
    borrower: Address,
    new_voucher: Address,
    amount_to_split: i128,
) -> Result<Address, ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;
    require_positive_amount(&env, amount_to_split)?;

    if voucher == new_voucher {
        return Err(ContractError::InvalidAmount);
    }

    let mut vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    let original = vouches.get(idx).unwrap();

    if amount_to_split >= original.stake {
        return Err(ContractError::InsufficientFunds);
    }

    let remaining = original
        .stake
        .checked_sub(amount_to_split)
        .ok_or(ContractError::ArithmeticError)?;

    if remaining < MIN_SPLIT_STAKE || amount_to_split < MIN_SPLIT_STAKE {
        return Err(ContractError::SplitBelowMinimum);
    }

    if has_active_loan(&env, &borrower) {
        return Err(ContractError::ActiveLoanExists);
    }

    for v in vouches.iter() {
        if v.voucher == new_voucher && v.token == original.token {
            return Err(ContractError::DuplicateVouch);
        }
    }

    let now = env.ledger().timestamp();

    let mut updated_original = original.clone();
    updated_original.stake = remaining;
    vouches.set(idx, updated_original);

    let new_record = VouchRecord {
        voucher: new_voucher.clone(),
        stake: amount_to_split,
        vouch_timestamp: now,
        token: original.token.clone(),
        expiry_timestamp: original.expiry_timestamp,
        delegate: None,
        chain_id: original.chain_id,
    };
    vouches.push_back(new_record);

    env.storage()
        .persistent()
        .set(&DataKey::Vouches(borrower.clone()), &vouches);

    invalidate_weighted_stake_cache(&env, &borrower, &original.token);

    let mut new_history: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(new_voucher.clone()))
        .unwrap_or(Vec::new(&env));
    if !new_history.iter().any(|b| b == borrower) {
        new_history.push_back(borrower.clone());
        env.storage()
            .persistent()
            .set(&DataKey::VoucherHistory(new_voucher.clone()), &new_history);
    }

    let mut split_history: Vec<VouchSplitRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::VouchSplitHistory(borrower.clone()))
        .unwrap_or(Vec::new(&env));
    split_history.push_back(VouchSplitRecord {
        parent_voucher: voucher.clone(),
        child_voucher: new_voucher.clone(),
        borrower: borrower.clone(),
        amount: amount_to_split,
        split_at: now,
    });
    env.storage()
        .persistent()
        .set(&DataKey::VouchSplitHistory(borrower.clone()), &split_history);

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("split")),
        (voucher, new_voucher.clone(), borrower, amount_to_split),
    );

    Ok(new_voucher)
}

/// Full split genealogy (every split ever performed) for a borrower's vouches.
pub fn get_vouch_split_history(env: Env, borrower: Address) -> Vec<VouchSplitRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::VouchSplitHistory(borrower))
        .unwrap_or(Vec::new(&env))
}

// ── Vouch rotation incentive program (Issue #1165) ────────────────────────────

/// Minimum time, in seconds, a voucher must wait between rotations.
pub const ROTATION_COOLDOWN_SECS: u64 = 14 * 24 * 60 * 60; // 14 days

/// Minimum time, in seconds, between rotations to qualify as "quarterly"
/// and earn the rotation bonus.
pub const ROTATION_BONUS_INTERVAL_SECS: u64 = 90 * 24 * 60 * 60; // ~1 quarter

/// Basis-point yield bonus awarded for rotating on (or after) a quarterly
/// cadence. Stored per-voucher (`DataKey::RotationBonusBps`) for the yield
/// distribution layer to apply.
pub const ROTATION_BONUS_BPS: u32 = 500; // 5%

/// A vouch is considered stagnant once this many days pass without a
/// rotation — surfaced by `get_stagnant_vouches` as a recommendation.
pub const STAGNANT_VOUCH_THRESHOLD_DAYS: u64 = 180;

/// Move a voucher's entire stake for `old_borrower` to `new_borrower`.
/// Rotation never incurs the protocol fee that would apply to an equivalent
/// withdraw-and-re-vouch flow (there is none to waive at the vouch layer —
/// this function simply never assesses one). Enforces a 14-day cooling off
/// period between rotations, and records a 5% yield bonus
/// (`DataKey::RotationBonusBps`) when the voucher rotates on a quarterly
/// cadence or better.
pub fn rotate_to_new_borrower(
    env: Env,
    voucher: Address,
    old_borrower: Address,
    new_borrower: Address,
) -> Result<(), ContractError> {
    voucher.require_auth();
    require_not_thawing(&env)?;

    if old_borrower == new_borrower {
        return Err(ContractError::InvalidAmount);
    }

    let now = env.ledger().timestamp();

    let last_rotation: Option<u64> = env
        .storage()
        .persistent()
        .get(&DataKey::LastRotationTimestamp(voucher.clone()));

    if let Some(last) = last_rotation {
        if now < last + ROTATION_COOLDOWN_SECS {
            return Err(ContractError::RotationCooldownActive);
        }
    }

    let mut old_vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(old_borrower.clone()))
        .ok_or(ContractError::NoVouchesForBorrower)?;

    let idx = old_vouches
        .iter()
        .position(|v| v.voucher == voucher)
        .ok_or(ContractError::VoucherNotFound)? as u32;

    if has_active_loan(&env, &old_borrower) {
        return Err(ContractError::ActiveLoanExists);
    }

    let vouch_rec = old_vouches.get(idx).unwrap();
    let token = vouch_rec.token.clone();

    let mut new_borrower_vouches: Vec<VouchRecord> = env
        .storage()
        .persistent()
        .get(&DataKey::Vouches(new_borrower.clone()))
        .unwrap_or(Vec::new(&env));

    if new_borrower_vouches
        .iter()
        .any(|v| v.voucher == voucher && v.token == token)
    {
        return Err(ContractError::DuplicateVouch);
    }

    // Move the record: remove from old_borrower, append (with a fresh
    // timestamp) under new_borrower.
    old_vouches.remove(idx);
    env.storage()
        .persistent()
        .set(&DataKey::Vouches(old_borrower.clone()), &old_vouches);
    invalidate_weighted_stake_cache(&env, &old_borrower, &token);

    let mut rotated = vouch_rec.clone();
    rotated.vouch_timestamp = now;
    new_borrower_vouches.push_back(rotated);
    env.storage().persistent().set(
        &DataKey::Vouches(new_borrower.clone()),
        &new_borrower_vouches,
    );
    invalidate_weighted_stake_cache(&env, &new_borrower, &token);

    // VoucherHistory bookkeeping: drop old_borrower if this was the
    // voucher's only vouch for them, add new_borrower.
    let mut history: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(voucher.clone()))
        .unwrap_or(Vec::new(&env));
    if !old_vouches.iter().any(|v| v.voucher == voucher) {
        if let Some(p) = history.iter().position(|b| b == old_borrower) {
            history.remove(p as u32);
        }
    }
    if !history.iter().any(|b| b == new_borrower) {
        history.push_back(new_borrower.clone());
    }
    env.storage()
        .persistent()
        .set(&DataKey::VoucherHistory(voucher.clone()), &history);

    // Rotation frequency + quarterly bonus tracking.
    let bonus_bps: u32 = match last_rotation {
        Some(last) if now.saturating_sub(last) >= ROTATION_BONUS_INTERVAL_SECS => {
            ROTATION_BONUS_BPS
        }
        None => ROTATION_BONUS_BPS,
        _ => 0,
    };
    env.storage()
        .persistent()
        .set(&DataKey::RotationBonusBps(voucher.clone()), &bonus_bps);
    env.storage()
        .persistent()
        .set(&DataKey::LastRotationTimestamp(voucher.clone()), &now);

    let count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::RotationCount(voucher.clone()))
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::RotationCount(voucher.clone()), &(count + 1));

    env.events().publish(
        (symbol_short!("vouch"), symbol_short!("rotate")),
        (
            voucher,
            old_borrower,
            new_borrower,
            vouch_rec.stake,
            bonus_bps,
        ),
    );

    Ok(())
}

/// Basis-point yield bonus currently earned by `voucher` from quarterly
/// rotation, for the yield-distribution layer to apply.
pub fn get_rotation_bonus_bps(env: Env, voucher: Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::RotationBonusBps(voucher))
        .unwrap_or(0)
}

/// Total number of rotations `voucher` has performed.
pub fn get_rotation_count(env: Env, voucher: Address) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::RotationCount(voucher))
        .unwrap_or(0)
}

/// Scan `voucher`'s current vouches and flag the ones that haven't rotated
/// in at least `STAGNANT_VOUCH_THRESHOLD_DAYS` days (measured from the
/// voucher's last rotation, or from each vouch's creation timestamp if the
/// voucher has never rotated).
pub fn get_stagnant_vouches(env: Env, voucher: Address) -> Vec<StagnantVouch> {
    let mut out: Vec<StagnantVouch> = Vec::new(&env);
    let now = env.ledger().timestamp();
    let borrowers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(voucher.clone()))
        .unwrap_or(Vec::new(&env));

    let last_rotation: Option<u64> = env
        .storage()
        .persistent()
        .get(&DataKey::LastRotationTimestamp(voucher.clone()));

    for borrower in borrowers.iter() {
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));
        if let Some(v) = vouches.iter().find(|v| v.voucher == voucher) {
            let baseline = last_rotation.unwrap_or(v.vouch_timestamp);
            let days_since = now.saturating_sub(baseline) / SECS_PER_DAY;
            if days_since >= STAGNANT_VOUCH_THRESHOLD_DAYS {
                out.push_back(StagnantVouch {
                    voucher: voucher.clone(),
                    borrower: borrower.clone(),
                    stake: v.stake,
                    days_since_rotation: days_since,
                });
            }
        }
    }

    out
}

// ── Vouch portfolio risk dashboard (Issue #1164) ──────────────────────────────

/// Estimated loss if `default_rate_bps` of a voucher's total stake defaults.
/// A simple, conservative approximation used purely for dashboard estimates
/// (not an on-chain guarantee of loss).
fn estimate_portfolio_loss(total_stake: i128, default_rate_bps: i128) -> i128 {
    total_stake.saturating_mul(default_rate_bps) / BPS_DENOMINATOR
}

/// Concentration and risk report for everything `voucher` currently backs:
/// per-borrower exposure, token ("sector") exposure, chain ("region")
/// exposure, an HHI-style concentration score, estimated losses under a
/// few default-rate scenarios, and rebalancing recommendations. Appends a
/// `PortfolioSnapshot` to the voucher's history on every call so
/// `PortfolioRiskReport::history` shows how the portfolio evolved.
pub fn get_portfolio_risk(env: Env, voucher: Address) -> PortfolioRiskReport {
    let now = env.ledger().timestamp();
    let borrowers: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherHistory(voucher.clone()))
        .unwrap_or(Vec::new(&env));

    let mut borrower_breakdown: Vec<BorrowerExposure> = Vec::new(&env);
    let mut token_addrs: Vec<Address> = Vec::new(&env);
    let mut token_stakes: Vec<i128> = Vec::new(&env);
    let mut chain_ids: Vec<Option<u32>> = Vec::new(&env);
    let mut chain_stakes: Vec<i128> = Vec::new(&env);
    let mut total_stake: i128 = 0;

    for borrower in borrowers.iter() {
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));
        let v = match vouches.iter().find(|v| v.voucher == voucher) {
            Some(v) => v,
            None => continue,
        };

        total_stake = total_stake.saturating_add(v.stake);
        borrower_breakdown.push_back(BorrowerExposure {
            borrower: borrower.clone(),
            stake: v.stake,
            pct_bps: 0, // filled in below once total_stake is known
        });

        match token_addrs.iter().position(|t| t == v.token) {
            Some(i) => {
                let updated = token_stakes.get(i).unwrap().saturating_add(v.stake);
                token_stakes.set(i, updated);
            }
            None => {
                token_addrs.push_back(v.token.clone());
                token_stakes.push_back(v.stake);
            }
        }

        match chain_ids.iter().position(|c| c == v.chain_id) {
            Some(i) => {
                let updated = chain_stakes.get(i).unwrap().saturating_add(v.stake);
                chain_stakes.set(i, updated);
            }
            None => {
                chain_ids.push_back(v.chain_id);
                chain_stakes.push_back(v.stake);
            }
        }
    }

    let borrower_count = borrower_breakdown.len();

    let mut concentration_hhi_bps: i128 = 0;
    let mut final_borrowers: Vec<BorrowerExposure> = Vec::new(&env);
    for b in borrower_breakdown.iter() {
        let pct_bps: u32 = if total_stake > 0 {
            (b.stake.saturating_mul(BPS_DENOMINATOR) / total_stake) as u32
        } else {
            0
        };
        concentration_hhi_bps += (pct_bps as i128) * (pct_bps as i128) / BPS_DENOMINATOR;
        final_borrowers.push_back(BorrowerExposure {
            borrower: b.borrower.clone(),
            stake: b.stake,
            pct_bps,
        });
    }

    let mut token_breakdown: Vec<TokenExposure> = Vec::new(&env);
    for i in 0..token_addrs.len() {
        let stake = token_stakes.get(i).unwrap();
        let pct_bps: u32 = if total_stake > 0 {
            (stake.saturating_mul(BPS_DENOMINATOR) / total_stake) as u32
        } else {
            0
        };
        token_breakdown.push_back(TokenExposure {
            token: token_addrs.get(i).unwrap(),
            stake,
            pct_bps,
        });
    }

    let mut chain_breakdown: Vec<ChainExposure> = Vec::new(&env);
    for i in 0..chain_ids.len() {
        let stake = chain_stakes.get(i).unwrap();
        let pct_bps: u32 = if total_stake > 0 {
            (stake.saturating_mul(BPS_DENOMINATOR) / total_stake) as u32
        } else {
            0
        };
        chain_breakdown.push_back(ChainExposure {
            chain_id: chain_ids.get(i).unwrap(),
            stake,
            pct_bps,
        });
    }

    let estimated_loss_1pct = estimate_portfolio_loss(total_stake, 100);
    let estimated_loss_5pct = estimate_portfolio_loss(total_stake, 500);
    let estimated_loss_10pct = estimate_portfolio_loss(total_stake, 1000);

    let mut recommendations: Vec<String> = Vec::new(&env);
    if let Some(top) = final_borrowers.iter().max_by_key(|b| b.pct_bps) {
        if top.pct_bps > 3000 {
            recommendations.push_back(String::from_str(
                &env,
                "Single-borrower concentration exceeds 30%; diversify across more borrowers.",
            ));
        }
    }
    if concentration_hhi_bps > 2500 {
        recommendations.push_back(String::from_str(
            &env,
            "Portfolio concentration (HHI) is high; spread stake across additional borrowers.",
        ));
    }
    if borrower_count > 0 && borrower_count < 3 {
        recommendations.push_back(String::from_str(
            &env,
            "Fewer than 3 borrowers backed; broaden the portfolio to limit default impact.",
        ));
    }

    let mut history: Vec<PortfolioSnapshot> = env
        .storage()
        .persistent()
        .get(&DataKey::VoucherPortfolioHistory(voucher.clone()))
        .unwrap_or(Vec::new(&env));
    history.push_back(PortfolioSnapshot {
        timestamp: now,
        total_stake,
        borrower_count,
    });
    env.storage().persistent().set(
        &DataKey::VoucherPortfolioHistory(voucher.clone()),
        &history,
    );

    PortfolioRiskReport {
        voucher,
        total_stake,
        borrower_count,
        borrower_breakdown: final_borrowers,
        token_breakdown,
        chain_breakdown,
        concentration_hhi_bps: concentration_hhi_bps as u32,
        estimated_loss_1pct,
        estimated_loss_5pct,
        estimated_loss_10pct,
        recommendations,
        history,
    }
}

/// Historical evolution of `voucher`'s portfolio, one snapshot per
/// `get_portfolio_risk` call.
pub fn get_portfolio_risk_history(env: Env, voucher: Address) -> Vec<PortfolioSnapshot> {
    env.storage()
        .persistent()
        .get(&DataKey::VoucherPortfolioHistory(voucher))
        .unwrap_or(Vec::new(&env))
}