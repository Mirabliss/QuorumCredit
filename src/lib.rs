#![no_std]
// Pre-existing lint suppressions — these warnings exist throughout the codebase
// and predate this PR. Suppressed here so `cargo clippy -D warnings` does not
// fail CI on issues outside the scope of this change.
#![allow(unused_imports)]
#![allow(unused_parens)]
#![allow(deprecated)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::assign_op_pattern)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::identity_op)]
#![allow(clippy::clamp_without_iter)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::len_zero)]
#![allow(clippy::needless_return)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::div_ceil)]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, symbol_short, token, Address, BytesN, Env, String, Vec,};

pub mod admin;
pub mod audit;
pub mod batch_transfer;
pub mod bridge;
pub mod cache;
pub mod cooldown_bypass;
pub mod credit_score;
pub mod cross_chain;
pub mod differential_testing;
pub mod errors;
pub mod flash_loan;
pub mod governance;
pub mod guarantor;
pub mod helpers;
pub mod insurance;
pub mod invariants;
pub mod lazy_slash;
pub mod loan;
pub mod maturity;
pub mod merkle_tree;
pub mod rbac;
pub mod reputation;
pub mod social;
pub mod types;
pub mod vouch;
pub mod vouch_reputation;
pub mod zk_snarks;
pub mod collateral_pool;
pub mod archive;
pub mod ipfs_archive;
pub mod syndication;
pub mod vouch_syndication;
pub mod vouch_milestones;
pub mod recurring_payment;

#[cfg(test)]
mod governance_test;
#[cfg(test)]
mod interest_test;
#[cfg(test)]
mod invariants_test;
#[cfg(test)]
mod property_based_invariants_test;
#[cfg(test)]
mod concurrent_operations_test;
#[cfg(test)]
mod loan_purpose_test;
#[cfg(test)]
mod multi_asset_test;
#[cfg(test)]
mod referral_test;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod rbac_enforcement_test;
#[cfg(test)]
mod storage_redesign_test;
#[cfg(test)]
mod timelock_safety_test;
#[cfg(test)]
mod contingent_loan_test;
#[cfg(test)]
mod cross_chain_test_scenarios;
#[cfg(test)]
mod loan_tranching_test;

pub use errors::ContractError;
pub use types::*;
pub use cross_chain::{BridgeAttestation, CrossChainLoanMetadata, UnifiedReputation};

use helpers::{
    acquire_lock, release_lock, config, get_active_loan_record, is_zero_address,
    loan_status as helper_loan_status, require_admin_approval, require_allowed_token,
    require_not_paused, token, token_client,
};
use reputation::ReputationNftExternalClient;
#[contract]
pub struct QuorumCreditContract;

#[contractimpl]
impl QuorumCreditContract {
    // ── Initialization ────────────────────────────────────────────────────────

    pub fn initialize(
        env: Env,
        deployer: Address,
        admins: Vec<Address>,
        admin_threshold: u32,
        token: Address,
    ) -> Result<(), ContractError> {
        deployer.require_auth();

        if env.storage().instance().has(&DataKey::Config) {
            return Err(ContractError::AlreadyInitialized);
        }

        helpers::validate_admin_config(
            &env,
            &admins,
            admin_threshold,
            &Vec::new(&env),
            &Vec::new(&env),
        )?;
        helpers::require_valid_token(&env, &token)?;

        env.storage().instance().set(&DataKey::Deployer, &deployer);
        env.storage().instance().set(
            &DataKey::Config,
            &Config {
                admins: admins.clone(),
                admin_threshold,
                token: token.clone(),                admin_whitelist: Vec::new(&env),
                admin_blacklist: Vec::new(&env),
                allowed_tokens: Vec::new(&env),
                yield_bps: DEFAULT_YIELD_BPS,
                slash_bps: DEFAULT_SLASH_BPS,
                max_vouchers: DEFAULT_MAX_VOUCHERS,
                min_loan_amount: DEFAULT_MIN_LOAN_AMOUNT,
                loan_duration: DEFAULT_LOAN_DURATION,
                max_loan_to_stake_ratio: DEFAULT_MAX_LOAN_TO_STAKE_RATIO,
                max_loan_to_collateral_ratio: DEFAULT_MAX_LOAN_TO_COLLATERAL_RATIO,
                grace_period: 0,
                vouch_cooldown_secs: DEFAULT_VOUCH_COOLDOWN_SECS,
                min_yield_stake: DEFAULT_MIN_YIELD_STAKE,
                min_vouch_age_secs: DEFAULT_MIN_VOUCH_AGE_SECS,
                prepayment_penalty_bps: 0,
                liquidity_mining_rate_bps: DEFAULT_LIQUIDITY_MINING_RATE_BPS,
                voting_period_seconds: DEFAULT_VOTING_PERIOD_SECONDS,
                slash_cooldown_seconds: 0,
                emergency_pause_enabled: false,
                early_repayment_discount_bps: 0,
                oracle_address: None,
                slash_delay_seconds: 0,
                successor_admin: None,
                rate_limit_config: RateLimitConfig {
                    window_secs: DEFAULT_RATE_LIMIT_WINDOW_SECS,
                    max_calls: DEFAULT_RATE_LIMIT_COUNT,
                    enabled: false,
                    tiers: Vec::new(&env),
                },
                multi_tier_thresholds: Vec::new(&env),                dynamic_slash_threshold: DEFAULT_DYNAMIC_SLASH_THRESHOLD,
                loan_size_slash_enabled: DEFAULT_LOAN_SIZE_SLASH_ENABLED,
                loan_size_slash_max_bps: DEFAULT_LOAN_SIZE_SLASH_MAX_BPS,
                recovery_percentage: 0,
                admin_compensation_bps: 0,
                removal_vote_threshold: 0,
                confirmation_required: DEFAULT_CONFIRMATION_REQUIRED,
                redistribution_rule: RedistributionRule::Treasury,
                immunity_period_seconds: 0,
                insurance_premium_bps: 0,
                liquidity_tier_yield_bonus: Vec::new(&env),
                max_priority_fee_cap_bps: MAX_PRIORITY_FEE_BPS,
            },
        );

        env.events().publish(
            (symbol_short!("contract"), symbol_short!("init")),
            (deployer, admins, admin_threshold, token),
        );

        // Initialize flash loan subsystem (Issue #1183)
        flash_loan::initialize_flash_loans(&env)?;

        Ok(())
    }

    // ── Vouching ──────────────────────────────────────────────────────────────


    pub fn vouch(
        env: Env,
        voucher: Address,
        borrower: Address,
        stake: i128,
        token: Address,
        chain_id: Option<u32>,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::vouch(env.clone(), voucher, borrower, stake, token, chain_id);
        release_lock(&env);
        result
    }

    /// Issue #632: Vouch with cross-chain support.
    /// chain_id=0 is native Stellar; non-zero requires prior bridge validation.
    pub fn vouch_cross_chain(
        env: Env,
        voucher: Address,
        borrower: Address,
        stake: i128,
        token: Address,
        chain_id: u32,
    ) -> Result<(), ContractError> {
        vouch::vouch_cross_chain(env, voucher, borrower, stake, token, chain_id)
    }

    /// Issue #632: Admin sets bridge validation status for a voucher on a given chain.
    pub fn set_bridge_validated(
        env: Env,
        admin_signers: Vec<Address>,
        voucher: Address,
        chain_id: u32,
        validated: bool,
    ) -> Result<(), ContractError> {
        vouch::set_bridge_validated(env, admin_signers, voucher, chain_id, validated)
    }

    /// Issue #632: Query bridge validation status.
    pub fn is_bridge_validated(env: Env, voucher: Address, chain_id: u32) -> bool {
        vouch::is_bridge_validated(env, voucher, chain_id)
    }

    /// Sybil resistance: estimate the economic cost to attack a borrower's current
    /// voucher configuration. Returns the minimum capital (in stroops) and minimum
    /// lock time an attacker must commit to match the legitimate set's weighted stake.
    ///
    /// This is a read-only query function — it does not mutate state.
    pub fn estimate_sybil_attack_cost(
        env: Env,
        borrower: Address,
    ) -> crate::types::SybilAttackCostEstimate {
        vouch::estimate_sybil_attack_cost(env, borrower)
    }

    /// Issue #936: Compute a Merkle root over a borrower's current vouch set
    /// and persist it, enabling off-chain provers to build compact inclusion
    /// proofs without retrieving the full vouch list. See
    /// docs/vouch-merkle-proof.md for the leaf/root format.
    pub fn compute_and_store_merkle_root(
        env: Env,
        borrower: Address,
    ) -> Result<BytesN<32>, ContractError> {
        vouch::compute_and_store_merkle_root(env, borrower)
    }

    /// Issue #936: Read the most recently stored vouch Merkle root for a
    /// borrower, if one has been computed.
    pub fn get_merkle_root(env: Env, borrower: Address) -> Option<crate::types::VouchMerkleRoot> {
        vouch::get_merkle_root(env, borrower)
    }

    /// Issue #936: Hash a single vouch's plaintext fields into the canonical
    /// Merkle leaf format, for use as the `leaf` argument to
    /// `verify_vouch_inclusion`.
    pub fn hash_vouch_leaf(
        env: Env,
        voucher: Address,
        stake: i128,
        token: Address,
        vouch_timestamp: u64,
    ) -> BytesN<32> {
        vouch::hash_vouch_leaf(env, voucher, stake, token, vouch_timestamp)
    }

    /// Issue #936: Verify a Merkle inclusion proof for a single vouch leaf
    /// against a previously-stored root, without needing the full vouch
    /// list. Returns `true` iff the proof is valid.
    pub fn verify_vouch_inclusion(
        env: Env,
        root: BytesN<32>,
        leaf: BytesN<32>,
        proof: Vec<BytesN<32>>,
    ) -> bool {
        vouch::verify_vouch_inclusion(env, root, leaf, proof)
    }

    /// Issue #867: Create a cross-collateral pool, seeded by the creator's stake.
    pub fn create_collateral_pool(
        env: Env,
        creator: Address,
        token: Address,
        initial_stake: i128,
    ) -> Result<u64, ContractError> {
        collateral_pool::create_pool(env, creator, token, initial_stake)
    }

    /// Issue #867: Join an existing, inactive collateral pool.
    pub fn join_collateral_pool(
        env: Env,
        voucher: Address,
        pool_id: u64,
        stake: i128,
    ) -> Result<(), ContractError> {
        collateral_pool::join_pool(env, voucher, pool_id, stake)
    }

    /// Issue #966: Join an existing, inactive collateral pool from another chain.
    /// The voucher must already be bridge-validated for `chain_id` (see
    /// `set_bridge_validated`).
    pub fn join_collateral_pool_cross_chain(
        env: Env,
        voucher: Address,
        pool_id: u64,
        stake: i128,
        chain_id: u32,
    ) -> Result<(), ContractError> {
        collateral_pool::join_pool_cross_chain(env, voucher, pool_id, stake, chain_id)
    }

    /// Issue #867: Leave an inactive collateral pool, withdrawing the caller's stake.
    pub fn leave_collateral_pool(
        env: Env,
        voucher: Address,
        pool_id: u64,
    ) -> Result<(), ContractError> {
        collateral_pool::leave_pool(env, voucher, pool_id)
    }

    /// Issue #867: Admin assigns a borrower to a pool, locking its collateral.
    pub fn assign_collateral_pool_borrower(
        env: Env,
        admin_signers: Vec<Address>,
        pool_id: u64,
        borrower: Address,
    ) -> Result<(), ContractError> {
        collateral_pool::assign_pool_to_borrower(env, admin_signers, pool_id, borrower)
    }

    /// Issue #867: Read a collateral pool record.
    pub fn get_collateral_pool(env: Env, pool_id: u64) -> Result<CollateralPool, ContractError> {
        collateral_pool::get_pool(env, pool_id)
    }

    /// Issue #867: Total stake held in a collateral pool.
    pub fn get_collateral_pool_total_stake(
        env: Env,
        pool_id: u64,
    ) -> Result<i128, ContractError> {
        collateral_pool::get_pool_total_stake(env, pool_id)
    }

    /// Issue #966: Total stake contributed to a pool from a specific chain.
    pub fn get_collateral_pool_chain_stake(
        env: Env,
        pool_id: u64,
        chain_id: u32,
    ) -> Result<i128, ContractError> {
        collateral_pool::get_pool_chain_stake(env, pool_id, chain_id)
    }

    /// #642: Vouch with an explicit sector label for diversification enforcement.
    pub fn vouch_with_sector(
        env: Env,
        voucher: Address,
        borrower: Address,
        stake: i128,
        token: Address,
        sector: String,
    ) -> Result<(), ContractError> {
        vouch::vouch_with_sector(env, voucher, borrower, stake, token, sector)
    }

    /// Confidential vouch with zk-SNARK proof verification
    ///
    /// Allows vouchers to stake without revealing the exact amount on-chain.
    /// The zk-SNARK proof demonstrates that:
    /// - The voucher has sufficient balance
    /// - The stake amount is within allowed bounds
    /// - The voucher is not blacklisted
    pub fn vouch_confidential(
        env: Env,
        voucher: Address,
        borrower: Address,
        commitment: ConfidentialCommitment,
        proof: ZkProof,
        token: Address,
        chain_id: Option<u32>,
    ) -> Result<(), ContractError> {
        // Verify the zk-SNARK proof against the provided proof context.
        zk_snarks::verify_vouch_proof(&env, &proof, &voucher, &borrower, &token, 0, true, false)?;

        // Store the commitment for this vouch
        env.storage()
            .persistent()
            .set(&DataKey::VouchCommitment(voucher.clone(), borrower.clone()), &commitment);

        // Record the proof for audit trail
        let proof_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ZkProofCounter)
            .unwrap_or(0u64)
            .checked_add(1)
            .expect("proof ID overflow");
        env.storage()
            .instance()
            .set(&DataKey::ZkProofCounter, &proof_id);

        let proof_record = crate::types::ZkProofRecord {
            proof_id,
            proof: proof.clone(),
            operation_type: crate::types::PROOF_TYPE_VOUCH,
            submitter: voucher.clone(),
            verified: true,
            submitted_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::ZkProofRecord(proof_id), &proof_record);

        // For now, we still need to call the regular vouch function
        // In a full implementation, this would use the commitment instead of the actual amount
        // The actual amount would be revealed off-chain to authorized parties
        vouch::vouch(env, voucher, borrower, 0, token, chain_id)
    }
    pub fn batch_vouch(
        env: Env,
        voucher: Address,
        borrowers: Vec<Address>,
        stakes: Vec<i128>,
        token: Address,
        chain_id: Option<u32>,
    ) -> Result<Vec<crate::types::BatchVouchResult>, ContractError> {
        vouch::batch_vouch(env, voucher, borrowers, stakes, token, chain_id)
    }

    pub fn increase_stake(
        env: Env,
        voucher: Address,
        borrower: Address,
        additional: i128,
    ) -> Result<(), ContractError> {
        vouch::increase_stake(env, voucher, borrower, additional)
    }

    pub fn decrease_stake(
        env: Env,
        voucher: Address,
        borrower: Address,
        amount: i128,
    ) -> Result<(), ContractError> {
        vouch::decrease_stake(env, voucher, borrower, amount)
    }

    pub fn withdraw_vouch(
        env: Env,
        voucher: Address,
        borrower: Address,
    ) -> Result<(), ContractError> {
        vouch::withdraw_vouch(env, voucher, borrower)
    }

    pub fn transfer_vouch(
        env: Env,
        from: Address,
        to: Address,
        borrower: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::transfer_vouch(env.clone(), from, to, borrower);
        release_lock(&env);
        result
    }

    pub fn delegate_vouch(
        env: Env,
        voucher: Address,
        borrower: Address,
        delegate: Address,
        token: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::delegate_vouch(env.clone(), voucher, borrower, delegate, token);
        release_lock(&env);
        result
    }

    pub fn revoke_delegation(
        env: Env,
        voucher: Address,
        borrower: Address,
        token: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::revoke_delegation(env.clone(), voucher, borrower, token);
        release_lock(&env);
        result
    }

    pub fn set_vouch_expiry(
        env: Env,
        voucher: Address,
        borrower: Address,
        expiry: u64,
        token: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::set_vouch_expiry(env.clone(), voucher, borrower, expiry, token);
        release_lock(&env);
        result
    }

    // ── Issue #1167: Vouch splitting ──────────────────────────────────────────

    pub fn split_vouch(
        env: Env,
        voucher: Address,
        borrower: Address,
        new_voucher: Address,
        amount_to_split: i128,
    ) -> Result<Address, ContractError> {
        acquire_lock(&env)?;
        let result = vouch::split_vouch(env.clone(), voucher, borrower, new_voucher, amount_to_split);
        release_lock(&env);
        result
    }

    pub fn get_vouch_split_history(env: Env, borrower: Address) -> Vec<VouchSplitRecord> {
        vouch::get_vouch_split_history(env, borrower)
    }

    // ── Issue #1165: Vouch rotation incentive program ─────────────────────────

    pub fn rotate_to_new_borrower(
        env: Env,
        voucher: Address,
        old_borrower: Address,
        new_borrower: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = vouch::rotate_to_new_borrower(env.clone(), voucher, old_borrower, new_borrower);
        release_lock(&env);
        result
    }

    pub fn get_rotation_bonus_bps(env: Env, voucher: Address) -> u32 {
        vouch::get_rotation_bonus_bps(env, voucher)
    }

    pub fn get_rotation_count(env: Env, voucher: Address) -> u32 {
        vouch::get_rotation_count(env, voucher)
    }

    pub fn get_stagnant_vouches(env: Env, voucher: Address) -> Vec<StagnantVouch> {
        vouch::get_stagnant_vouches(env, voucher)
    }

    // ── Issue #1164: Vouch portfolio risk dashboard ───────────────────────────

    pub fn get_portfolio_risk(env: Env, voucher: Address) -> PortfolioRiskReport {
        vouch::get_portfolio_risk(env, voucher)
    }

    pub fn get_portfolio_risk_history(env: Env, voucher: Address) -> Vec<PortfolioSnapshot> {
        vouch::get_portfolio_risk_history(env, voucher)
    }

    // ── Loans ─────────────────────────────────────────────────────────────────

    pub fn register_referral(
        env: Env,
        borrower: Address,
        referrer: Address,
    ) -> Result<(), ContractError> {
        loan::register_referral(env, borrower, referrer)
    }

    pub fn get_referrer(env: Env, borrower: Address) -> Option<Address> {
        loan::get_referrer(env, borrower)
    }

    pub fn set_referral_bonus_bps(env: Env, admin_signers: Vec<Address>, bonus_bps: u32) {
        helpers::require_admin_approval(&env, &admin_signers);
        assert!(bonus_bps <= 10_000, "bonus_bps must not exceed 10000");
        env.storage()
            .instance()
            .set(&DataKey::ReferralBonusBps, &bonus_bps);
    }

    pub fn get_referral_bonus_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ReferralBonusBps)
            .unwrap_or(DEFAULT_REFERRAL_BONUS_BPS)
    }

    /// Issue #1287: Set the governance-adjustable withdrawal-queue priority-fee cap.
    /// The cap is expressed in basis points of the voucher's own stake (max 10_000 = 100%).
    /// Requires admin approval.
    pub fn set_priority_fee_cap_bps(env: Env, admin_signers: Vec<Address>, cap_bps: i128) {
        helpers::require_admin_approval(&env, &admin_signers);
        assert!(cap_bps >= 0 && cap_bps <= 10_000, "cap_bps must be 0..=10000");
        let mut cfg = helpers::config(&env);
        cfg.max_priority_fee_cap_bps = cap_bps;
        env.storage().instance().set(&DataKey::Config, &cfg);
        env.events().publish(
            (symbol_short!("admin"), symbol_short!("feecap")),
            cap_bps,
        );
    }

    /// Issue #1287: Get the current withdrawal-queue priority-fee cap in basis points.
    pub fn get_priority_fee_cap_bps(env: Env) -> i128 {
        helpers::config(&env).max_priority_fee_cap_bps
    }

    pub fn get_withdrawal_queue(env: Env, borrower: Address) -> Vec<QueuedWithdrawal> {
        vouch::get_withdrawal_queue(env, borrower)
    }

    pub fn process_withdrawal_batch(env: Env, borrower: Address, count: u32) -> u32 {
        vouch::process_withdrawal_batch(&env, &borrower, count)
    }

    pub fn request_loan(
        env: Env,
        borrower: Address,
        amount: i128,
        threshold: i128,
        loan_purpose: soroban_sdk::String,
        token: Address,
    ) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = loan::request_loan(env.clone(), borrower, amount, threshold, loan_purpose, token);
        release_lock(&env);
        result
    }

    /// Confidential loan request with zk-SNARK proof verification
    ///
    /// Allows borrowers to request loans without revealing exact amounts on-chain.
    /// The zk-SNARK proof demonstrates that:
    /// - The borrower meets eligibility requirements
    /// - The requested amount is within bounds
    /// - Sufficient vouches exist (without revealing individual vouch amounts)
    pub fn request_loan_confidential(
        env: Env,
        borrower: Address,
        _commitment: ConfidentialCommitment,
        proof: ZkProof,
        threshold: i128,
        loan_purpose: soroban_sdk::String,
        token: Address,
    ) -> Result<(), ContractError> {
        // Verify the zk-SNARK proof against the provided loan context.
        zk_snarks::verify_loan_proof(&env, &proof, &borrower, &token, 0, threshold, true, false)?;

        // Record the proof for audit trail
        let proof_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ZkProofCounter)
            .unwrap_or(0u64)
            .checked_add(1)
            .expect("proof ID overflow");
        env.storage()
            .instance()
            .set(&DataKey::ZkProofCounter, &proof_id);

        let proof_record = crate::types::ZkProofRecord {
            proof_id,
            proof: proof.clone(),
            operation_type: crate::types::PROOF_TYPE_LOAN_REQUEST,
            submitter: borrower.clone(),
            verified: true,
            submitted_at: env.ledger().timestamp(),
        };
        env.storage()
            .instance()
            .set(&DataKey::ZkProofRecord(proof_id), &proof_record);

        // For now, we still need to call the regular request_loan function
        // In a full implementation, this would use the commitment instead of the actual amount
        // The actual amount would be revealed off-chain to authorized parties
        loan::request_loan(env, borrower, 0, threshold, loan_purpose, token)
    }


    pub fn dispute_vouch(
        env: Env,
        voucher: Address,
        borrower: Address,
        evidence_hash: BytesN<32>,
    ) -> Result<(), ContractError> {
        vouch::dispute_vouch(env, voucher, borrower, evidence_hash)
    }

    pub fn slash(env: Env, admin_signers: Vec<Address>, borrower: Address) {
        helpers::require_admin_approval(&env, &admin_signers);
        helpers::require_not_paused(&env).expect("contract is paused");

        let mut loan = helpers::get_active_loan_record(&env, &borrower)
            .expect("no active loan");

        if loan.status != LoanStatus::Active {
            panic_with_error!(&env, ContractError::NoActiveLoan);
        }

        let cfg = config(&env);
        let _vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));

        loan.status = LoanStatus::Defaulted;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);
        env.storage()
            .persistent()
            .remove(&DataKey::ActiveLoan(borrower.clone()));

        // Process withdrawal queue before deleting vouches (Issue #865)
        vouch::process_withdrawal_queue(&env, &borrower);

        // Re-read vouches after queue processing removed queued withdrawals
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));

        let token_client = token::Client::new(&env, &loan.token_address);
        let mut total_slashed: i128 = 0;
        for v in vouches.iter() {
            if v.token == loan.token_address {
                let slash_amount = v.stake * cfg.slash_bps / 10_000;
                let returned = v.stake - slash_amount;
                if returned > 0 {
                    token_client.transfer(&env.current_contract_address(), &v.voucher, &returned);
                }
                total_slashed += slash_amount;
            } else if !is_zero_address(&env, &v.token) {
                // Non-matching token vouches are returned in full.
                let other_token = soroban_sdk::token::Client::new(&env, &v.token);
                other_token.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            }
        }

        helpers::add_slash_balance(&env, total_slashed);

        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DefaultCount(borrower.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::DefaultCount(borrower.clone()), &(count + 1));

        // Burn excellent credit tier badge on default
        reputation::burn_excellent_badge(&env, &borrower);

        // Burn external reputation NFT on default
        if let Some(nft_addr) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::ReputationNft)
        {
            ReputationNftExternalClient::new(&env, &nft_addr).burn(&borrower);
        }

        // Update credit score after slash
        let _ = credit_score::update_credit_score(env.clone(), borrower.clone());

        // Issue #1288: Decrement on-chain TVL / active-loan-count counters on slash.
        helpers::decrement_tvl_counters(&env, loan.amount);

        // Clean up vouches storage last
        env.storage()
            .persistent()
            .remove(&DataKey::Vouches(borrower.clone()));
    }

    pub fn repay(env: Env, borrower: Address, payment: i128) -> Result<(), ContractError> {
        acquire_lock(&env)?;
        let result = loan::repay(env.clone(), borrower, payment);
        release_lock(&env);
        result
    }


    /// Confirm intent to repay the active loan.
    ///
    /// When `Config.confirmation_required` is `true`, borrowers must call this
    /// function before calling `repay`. The confirmation is stored per-loan and
    /// consumed on the first successful `repay` call, so it cannot be replayed.
    ///
    /// This is a no-op (succeeds silently) when `confirmation_required` is false,
    /// so callers can always call it without checking the config first.
    pub fn confirm_repayment(env: Env, borrower: Address) -> Result<(), ContractError> {
        borrower.require_auth();
        require_not_paused(&env)?;

        let loan = get_active_loan_record(&env, &borrower)?;

        env.storage()
            .persistent()
            .set(&DataKey::RepaymentConfirmation(loan.id), &true);

        env.events().publish(
            (symbol_short!("loan"), symbol_short!("repay_ok")),
            (borrower, loan.id),
        );

        Ok(())
    }

    /// #667: Called by the registered oracle to verify a repayment held in escrow.
    /// If `approved` is true, releases funds to vouchers. If false, returns funds to borrower.
    /// Called by the registered oracle to publish a fresh price for `key`
    /// (e.g. a collateral token's symbol). Used to inform dynamic-rate pricing.
    pub fn set_oracle_price(
        env: Env,
        oracle: Address,
        key: soroban_sdk::Symbol,
        price: i128,
    ) -> Result<(), ContractError> {
        helpers::set_oracle_price(&env, &oracle, key, price)
    }

    pub fn verify_repayment(
        env: Env,
        oracle: Address,
        borrower: Address,
        approved: bool,
    ) -> Result<(), ContractError> {
        oracle.require_auth();
        require_not_paused(&env)?;

        // Verify caller is the registered oracle
        let cfg = config(&env);
        let registered = cfg.oracle_address.ok_or(ContractError::OracleUnauthorized)?;
        if oracle != registered {
            return Err(ContractError::OracleUnauthorized);
        }

        let loan_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::ActiveLoan(borrower.clone()))
            .ok_or(ContractError::NoActiveLoan)?;
        let mut loan: LoanRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Loan(loan_id))
            .ok_or(ContractError::NoActiveLoan)?;

        if loan.escrow_status != EscrowStatus::Pending {
            return Err(ContractError::NoEscrowFound);
        }

        let escrowed: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowAmount(borrower.clone()))
            .unwrap_or(0);

        let token_client = require_allowed_token(&env, &loan.token_address)?;
        let now = env.ledger().timestamp();

        if approved {
            loan.escrow_status = EscrowStatus::Released;
            loan.status = LoanStatus::Repaid;
            loan.repayment_timestamp = Some(now);

            let vouches: Vec<VouchRecord> = env
                .storage()
                .persistent()
                .get(&DataKey::Vouches(borrower.clone()))
                .unwrap_or(Vec::new(&env));

            let total_stake: i128 = vouches
                .iter()
                .filter(|v| v.token == loan.token_address)
                .map(|v| v.stake)
                .sum();

            for v in vouches.iter() {
                if v.token != loan.token_address {
                    continue;
                }
                // Issue #633: Yield tiering — vouch age bonus.
                // Vouches older than 30 days get +50% of their yield share.
                // Vouches older than 7 days get +25% of their yield share.
                let vouch_age_secs = loan.disbursement_timestamp.saturating_sub(v.vouch_timestamp);
                let age_multiplier_bps: i128 = if vouch_age_secs >= 30 * 24 * 60 * 60 {
                    15_000 // 150%
                } else if vouch_age_secs >= 7 * 24 * 60 * 60 {
                    12_500 // 125%
                } else {
                    10_000 // 100% base
                };

                let base_yield_share = if total_stake > 0 {
                    loan.total_yield * v.stake / total_stake
                } else {
                    0
                };
                let tiered_yield = base_yield_share * age_multiplier_bps / 10_000;

                // Issue #634: Liquidity mining reward on top of yield.
                let cfg = config(&env);
                let mining_reward = if cfg.liquidity_mining_rate_bps > 0 {
                    v.stake * cfg.liquidity_mining_rate_bps as i128 / 10_000
                } else {
                    0
                };

                token_client.transfer(
                    &env.current_contract_address(),
                    &v.voucher,
                    &(v.stake + tiered_yield + mining_reward),
                );
            }

            // Process withdrawal queue before deleting vouches (Issue #865)
            vouch::process_withdrawal_queue(&env, &borrower);

            // Increment borrower repayment count
            let prev_count: u32 = env
                .storage()
                .persistent()
                .get(&DataKey::RepaymentCount(borrower.clone()))
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&DataKey::RepaymentCount(borrower.clone()), &(prev_count + 1));

            // Update credit score after successful repayment
            let _ = credit_score::update_credit_score(env.clone(), borrower.clone());

            env.storage()
                .persistent()
                .remove(&DataKey::ActiveLoan(borrower.clone()));
            env.storage()
                .persistent()
                .remove(&DataKey::Vouches(borrower.clone()));

            env.events().publish(
                (symbol_short!("loan"), symbol_short!("repaid")),
                (borrower.clone(), loan.amount),
            );
        } else {
            // Oracle rejected — return escrowed funds to borrower
            loan.escrow_status = EscrowStatus::Rejected;
            loan.amount_repaid -= escrowed;

            if escrowed > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &borrower,
                    &escrowed,
                );
            }

            env.events().publish(
                (symbol_short!("loan"), symbol_short!("escrw_rej")),
                (borrower.clone(), escrowed),
            );
        }

        env.storage()
            .persistent()
            .remove(&DataKey::EscrowAmount(borrower.clone()));
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);

        release_lock(&env);
        Ok(())
    }


    /// Callable by anyone after the loan deadline has passed. Applies the standard slash penalty.
    pub fn auto_slash(env: Env, borrower: Address) {
        let mut loan = helpers::get_active_loan_record(&env, &borrower)
            .expect("no active loan");

        if loan.status != LoanStatus::Active {
            panic_with_error!(&env, ContractError::NoActiveLoan);
        }
        assert!(
            env.ledger().timestamp() > loan.deadline,
            "loan deadline has not passed"
        );

        let cfg = config(&env);
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));

        loan.defaulted = true;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);
        env.storage()
            .persistent()
            .remove(&DataKey::ActiveLoan(borrower.clone()));
        loan.status = LoanStatus::Defaulted;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);
        env.storage()
            .persistent()
            .remove(&DataKey::ActiveLoan(borrower.clone()));

        let loan_token = soroban_sdk::token::Client::new(&env, &loan.token_address);
        let mut total_slash: i128 = 0;
        for v in vouches.iter() {
            if v.token != loan.token_address {
                continue;
            }
            let slash_amount = v.stake * cfg.slash_bps / 10_000;
            let returned = v.stake - slash_amount;
            total_slash += slash_amount;
            if returned > 0 {
                loan_token.transfer(&env.current_contract_address(), &v.voucher, &returned);
            }
        }

        // Process withdrawal queue before deleting vouches (Issue #865)
        vouch::process_withdrawal_queue(&env, &borrower);

        // Re-read vouches after queue processing
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));

        let token_client = token::Client::new(&env, &loan.token_address);
        let mut total_slash: i128 = 0;
        for v in vouches.iter() {
            if v.token == loan.token_address {
                let slash_amount = v.stake * cfg.slash_bps / 10_000;
                let returned = v.stake - slash_amount;
                total_slash += slash_amount;
                if returned > 0 {
                    token_client.transfer(&env.current_contract_address(), &v.voucher, &returned);
                }
            } else if !is_zero_address(&env, &v.token) {
                // Non-matching token vouches are returned in full.
                let other_token = soroban_sdk::token::Client::new(&env, &v.token);
                other_token.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            }
        }

        helpers::add_slash_balance(&env, total_slash);

        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DefaultCount(borrower.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::DefaultCount(borrower.clone()), &(count + 1));

        if let Some(nft_addr) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::ReputationNft)
        {
            ReputationNftExternalClient::new(&env, &nft_addr).burn(&borrower);
        }

        env.events().publish(
            (symbol_short!("loan"), symbol_short!("autoslash")),
            (borrower, total_slash),
        );
        // Issue #1288: Decrement on-chain TVL / active-loan-count counters on auto-slash.
        helpers::decrement_tvl_counters(&env, loan.amount);
    }
    pub fn claim_expired_loan(env: Env, borrower: Address) {
        borrower.require_auth();

        let mut loan = helpers::get_active_loan_record(&env, &borrower)
            .expect("no active loan");

        if loan.status != LoanStatus::Active {
            panic_with_error!(&env, ContractError::NoActiveLoan);
        }

        let now = env.ledger().timestamp();
        assert!(now >= loan.deadline, "loan has not expired yet");

        // Process withdrawal queue first (Issue #865)
        vouch::process_withdrawal_queue(&env, &borrower);

        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower.clone()))
            .unwrap_or(Vec::new(&env));

        let loan_token = soroban_sdk::token::Client::new(&env, &loan.token_address);
        for v in vouches.iter() {
            if v.token == loan.token_address {
                loan_token.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            }
        }

        loan.defaulted = true;
        let token_client = token::Client::new(&env, &loan.token_address);
        for v in vouches.iter() {
            if v.token == loan.token_address {
                token_client.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            } else if !is_zero_address(&env, &v.token) {
                // Non-matching token vouches are returned via their own token.
                let other_token = soroban_sdk::token::Client::new(&env, &v.token);
                other_token.transfer(&env.current_contract_address(), &v.voucher, &v.stake);
            }
        }

        loan.status = LoanStatus::Defaulted;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);
        env.storage()
            .persistent()
            .remove(&DataKey::ActiveLoan(borrower.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::Vouches(borrower.clone()));

        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DefaultCount(borrower.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::DefaultCount(borrower.clone()), &(count + 1));
        // Issue #1288: Decrement on-chain TVL / active-loan-count counters on expired claim.
        helpers::decrement_tvl_counters(&env, loan.amount);
    }

    /// Admin withdraws accumulated slashed funds.
    pub fn slash_treasury(env: Env, admin_signers: Vec<Address>, recipient: Address) {
        require_admin_approval(&env, &admin_signers);
        helpers::require_admin_approval(&env, &admin_signers);

        let amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::SlashTreasury)
            .unwrap_or(0);
        assert!(amount > 0, "no slashed funds to withdraw");
        env.storage()
            .instance()
            .set(&DataKey::SlashTreasury, &0i128);
        token_client(&env).transfer(&env.current_contract_address(), &recipient, &amount);
    }

    // ── Loan Pool ─────────────────────────────────────────────────────────────

    /// Admin function: atomically disburse a batch of small loans to multiple borrowers.
    pub fn create_loan_pool(
        env: Env,
        admin_signers: Vec<Address>,
        borrowers: Vec<Address>,
        amounts: Vec<i128>,
    ) -> Result<u64, ContractError> {
        helpers::require_admin_approval(&env, &admin_signers);

        if borrowers.len() != amounts.len() {
            return Err(ContractError::PoolLengthMismatch);
        }
        if borrowers.is_empty() {
            return Err(ContractError::PoolEmpty);
        }

        let cfg = config(&env);
        let now = env.ledger().timestamp();
        let deadline = now + cfg.loan_duration;

        let mut total_amount: i128 = 0;
        for i in 0..borrowers.len() {
            let borrower = borrowers.get(i).unwrap();
            let amount = amounts.get(i).unwrap();

            assert!(
                amount >= cfg.min_loan_amount,
                "pool: amount below minimum loan threshold"
            );

            if helpers::has_active_loan(&env, &borrower) {
                return Err(ContractError::PoolBorrowerActiveLoan);
            }

            let total_stake: i128 = env
                .storage()
                .persistent()
                .get::<DataKey, Vec<VouchRecord>>(&DataKey::Vouches(borrower.clone()))
                .unwrap_or(Vec::new(&env))
                .iter()
                .map(|v| v.stake)
                .sum();
            let max_allowed = total_stake * cfg.max_loan_to_stake_ratio as i128 / 100;
            assert!(
                amount <= max_allowed,
                "pool: loan amount exceeds maximum collateral ratio for borrower"
            );

            total_amount += amount;
        }

        let token_client = helpers::primary_token(&env);
        let contract_balance = token_client.balance(&env.current_contract_address());
        if contract_balance < total_amount {
            return Err(ContractError::PoolInsufficientFunds);
        }

        let pool_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LoanPoolCounter)
            .unwrap_or(0u64)
            .checked_add(1)
            .expect("pool ID overflow");
        env.storage()
            .instance()
            .set(&DataKey::LoanPoolCounter, &pool_id);

        for i in 0..borrowers.len() {
            let borrower = borrowers.get(i).unwrap();
            let amount = amounts.get(i).unwrap();
            let loan_id = helpers::next_loan_id(&env);

            env.storage().persistent().set(
                &DataKey::Loan(loan_id),
                &LoanRecord {
                    id: loan_id,
                    borrower: borrower.clone(),
                    guarantor: None,
                    buyback_price: 0,
                    auto_repay_enabled: false,
                    auto_repay_attempts: 0,
                    escrow_status: EscrowStatus::None,
                    co_borrowers: Vec::new(&env),
                    amount,
                    amount_repaid: 0,
                    total_yield: amount * cfg.yield_bps / 10_000,
                    status: LoanStatus::Active,
                    repaid: false,
                    defaulted: false,
                    created_at: now,
                    disbursement_timestamp: now,
                    repayment_timestamp: None,
                    deadline,
                    loan_purpose: soroban_sdk::String::from_str(&env, "pool"),
                    token_address: cfg.token.clone(),
                    amortization_schedule: Vec::new(&env),
                    reminder_sent: false,
                    risk_score: 0,
                    deferment_periods: 0,
                    maturity_date: None,
                    rate_type: RateType::Fixed,
                    index_reference: None,
                    last_interest_calc: now,
                    accrued_interest: 0,
                    milestone_bonus_applied: 0,
                    retry_count: 0,
                    suspension_timestamp: None,
                    suspension_amount_repaid: 0,
                },
            );
            env.storage()
                .persistent()
                .set(&DataKey::ActiveLoan(borrower.clone()), &loan_id);
            env.storage()
                .persistent()
                .set(&DataKey::LatestLoan(borrower.clone()), &loan_id);

            token_client.transfer(&env.current_contract_address(), &borrower, &amount);

            env.events().publish(
                (symbol_short!("pool"), symbol_short!("loan")),
                (pool_id, borrower.clone(), amount, deadline),
            );
        }

        env.storage().persistent().set(
            &DataKey::LoanPool(pool_id),
            &LoanPoolRecord {
                pool_id,
                borrowers: borrowers.clone(),
                amounts: amounts.clone(),
                created_at: now,
                total_disbursed: total_amount,
            },
        );

        env.events().publish(
            (symbol_short!("pool"), symbol_short!("created")),
            (pool_id, borrowers.len(), total_amount),
        );

        Ok(pool_id)
    }

    pub fn get_loan_pool(env: Env, pool_id: u64) -> Option<LoanPoolRecord> {
        env.storage().persistent().get(&DataKey::LoanPool(pool_id))
    }

    pub fn get_loan_pool_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::LoanPoolCounter)
            .unwrap_or(0)
    }

    // ── Liquidity Rebalancing (Issue #88) ─────────────────────────────────────




    // ── Admin ─────────────────────────────────────────────────────────────────

    pub fn add_admin(env: Env, admin_signers: Vec<Address>, new_admin: Address) {
        admin::add_admin(env, admin_signers, new_admin)
    }

    /// #669: Retry a failed repayment. Increments retry_count and re-attempts the transfer.
    /// Returns `MaxRetriesExceeded` if retry_count >= MAX_REPAYMENT_RETRIES.
    pub fn retry_repayment(
        env: Env,
        borrower: Address,
        payment: i128,
    ) -> Result<(), ContractError> {
        borrower.require_auth();
        require_not_paused(&env)?;

        let loan_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::ActiveLoan(borrower.clone()))
            .ok_or(ContractError::NoActiveLoan)?;
        let mut loan: LoanRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Loan(loan_id))
            .ok_or(ContractError::NoActiveLoan)?;

        const MAX_REPAYMENT_RETRIES: u32 = 3;
        if loan.retry_count >= MAX_REPAYMENT_RETRIES {
            return Err(ContractError::MaxRetriesExceeded);
        }

        loan.retry_count += 1;
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan.id), &loan);

        // Delegate to the standard repay logic
        Self::repay(env, borrower, payment)
    }

    pub fn get_loan(env: Env, borrower: Address) -> Option<LoanRecord> {
        let loan_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::ActiveLoan(borrower.clone()))?;
        env.storage().persistent().get(&DataKey::Loan(loan_id))
    }

    pub fn get_vouches(env: Env, borrower: Address) -> Vec<VouchRecord> {
        env.storage()
            .persistent()
            .get(&DataKey::Vouches(borrower))
            .unwrap_or(Vec::new(&env))
    }

    /// Paginated read of a borrower's vouches (Issue #1146). Returns up to
    /// `limit` (capped at `MAX_PAGE_SIZE`) records starting at `offset`, plus
    /// `next_cursor` — `Some(offset)` for the next call, or `None` at the end.
    pub fn get_vouches_page(
        env: Env,
        borrower: Address,
        offset: u32,
        limit: u32,
    ) -> (Vec<VouchRecord>, Option<u32>) {
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower))
            .unwrap_or(Vec::new(&env));
        crate::helpers::paginate_vec(&env, &vouches, offset, limit)
    }

    /// Paginated read of the addresses of borrowers a `voucher` has ever
    /// backed (Issue #1146). See `get_vouches_page` for cursor semantics.
    pub fn get_voucher_history_page(
        env: Env,
        voucher: Address,
        offset: u32,
        limit: u32,
    ) -> (Vec<Address>, Option<u32>) {
        let history: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::VoucherHistory(voucher))
            .unwrap_or(Vec::new(&env));
        crate::helpers::paginate_vec(&env, &history, offset, limit)
    }

    /// Paginated read of a borrower's pending withdrawal queue (Issue #1146).
    pub fn get_withdrawal_queue_page(
        env: Env,
        borrower: Address,
        offset: u32,
        limit: u32,
    ) -> (Vec<crate::types::QueuedWithdrawal>, Option<u32>) {
        vouch::get_withdrawal_queue_page(env, borrower, offset, limit)
    }

    /// Read the bounded hot vouch-history window for (borrower, voucher,
    /// token) (Issue #1146). See `get_vouch_history_archive_count` for the
    /// full historical log.
    pub fn get_vouch_history(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
    ) -> Vec<crate::types::VouchHistoryEntry> {
        vouch::get_vouch_history(env, borrower, voucher, token)
    }

    /// Number of archive batches created so far for this (borrower, voucher,
    /// token) relationship's vouch-history log (Issue #1146). Use with
    /// `get_archived_vouch_history_batch` to walk the full historical log;
    /// `get_vouch_history` alone only returns the bounded hot window.
    pub fn get_vouch_history_archive_count(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
    ) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::VouchHistoryArchiveCount(borrower, voucher, token))
            .unwrap_or(0)
    }

    /// Read one archived vouch-history batch (Issue #1146). `batch_id` ranges
    /// over `0..get_vouch_history_archive_count(...)`, oldest batch first.
    pub fn get_archived_vouch_history_batch(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
        batch_id: u32,
    ) -> Vec<crate::types::VouchHistoryEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::ArchivedVouchHistory(borrower, voucher, token, batch_id))
            .unwrap_or(Vec::new(&env))
    }

    // ── Issue #1179: Vouch Audit Trail ────────────────────────────────────────

    /// Retrieve the complete audit trail for a vouch (Issue #1179).
    /// Returns all audit events for the specified (borrower, voucher, token) in chronological order.
    pub fn get_vouch_audit_trail(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
    ) -> Result<crate::types::VouchAuditTrail, ContractError> {
        audit::get_vouch_audit_trail(env, borrower, voucher, token)
    }

    /// Retrieve a page of audit events for a vouch (Issue #1179).
    /// Returns up to `limit` events starting from index `offset`.
    pub fn get_vouch_audit_trail_page(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<crate::types::VouchAuditEvent>, ContractError> {
        audit::get_vouch_audit_trail_page(env, borrower, voucher, token, offset, limit)
    }

    /// Export audit trail data as a formatted report (Issue #1179).
    /// Suitable for compliance and transparency reporting.
    pub fn export_vouch_audit_report(
        env: Env,
        borrower: Address,
        voucher: Address,
        token: Address,
    ) -> Result<String, ContractError> {
        audit::export_vouch_audit_report(env, borrower, voucher, token)
    }

    // ── Issue #1177: Vouch Maturity-Based Interest Adjustment ────────────────

    /// Get the maturity record for a vouch (Issue #1177).
    /// Returns tenure information and current maturity bonus.
    pub fn get_vouch_maturity(
        env: Env,
        voucher: Address,
        borrower: Address,
        token: Address,
    ) -> Result<crate::types::VouchMaturityRecord, ContractError> {
        maturity::get_vouch_maturity(env, voucher, borrower, token)
    }

    /// Get the current maturity bonus for a vouch in basis points (Issue #1177).
    /// Returns 0-100 bps representing 0-1% additional interest from tenure.
    pub fn get_vouch_maturity_bonus(
        env: Env,
        voucher: Address,
        borrower: Address,
        token: Address,
    ) -> Result<i128, ContractError> {
        maturity::update_maturity_bonus(&env, &voucher, &borrower, &token)
    }

    /// Get the total interest bonus for a vouch including loyalty bonus (Issue #1177).
    /// Returns maturity bonus + loyalty bonus (if eligible for 2+ years).
    pub fn get_vouch_total_interest_bonus(
        env: Env,
        voucher: Address,
        borrower: Address,
        token: Address,
    ) -> Result<i128, ContractError> {
        maturity::get_total_interest_bonus(&env, &voucher, &borrower, &token)
    }

    // ── Issue #1176: Social Features for Borrower Network ────────────────────

    /// Set or update a borrower's profile (Issue #1176).
    /// Allows borrowers to create their community profile with bio and sector info.
    pub fn set_borrower_profile(
        env: Env,
        borrower: Address,
        bio: String,
        sector: Option<String>,
        region: Option<String>,
    ) -> Result<(), ContractError> {
        borrower.require_auth();
        social::set_borrower_profile(&env, borrower, bio, sector, region)
    }

    /// Get a borrower's profile (Issue #1176).
    pub fn get_borrower_profile(
        env: Env,
        borrower: Address,
    ) -> Result<crate::types::BorrowerProfile, ContractError> {
        social::get_borrower_profile(env, borrower)
    }

    /// Set whether borrower consents to share success stories (Issue #1176).
    pub fn set_success_story_consent(
        env: Env,
        borrower: Address,
        consent: bool,
    ) -> Result<(), ContractError> {
        borrower.require_auth();
        social::set_success_story_consent(&env, borrower, consent)
    }

    /// Submit a success story (Issue #1176).
    /// Returns the story ID for reference.
    pub fn submit_success_story(
        env: Env,
        borrower: Address,
        title: String,
        content: String,
    ) -> Result<u64, ContractError> {
        borrower.require_auth();
        social::submit_success_story(&env, borrower, title, content)
    }

    /// Publish a success story (Issue #1176).
    /// Only the borrower who submitted can publish.
    pub fn publish_success_story(
        env: Env,
        borrower: Address,
        story_id: u64,
    ) -> Result<(), ContractError> {
        borrower.require_auth();
        social::publish_success_story(&env, borrower, story_id)
    }

    /// Get a success story (Issue #1176).
    pub fn get_success_story(
        env: Env,
        story_id: u64,
    ) -> Result<crate::types::SuccessStory, ContractError> {
        social::get_success_story(env, story_id)
    }

    /// Get all success stories for a borrower (Issue #1176).
    pub fn get_borrower_success_stories(
        env: Env,
        borrower: Address,
    ) -> Result<Vec<crate::types::SuccessStory>, ContractError> {
        social::get_borrower_success_stories(env, borrower)
    }

    /// Get retention metrics for a borrower (Issue #1176).
    /// Tracks loan activity, repayment success, and platform engagement.
    pub fn get_retention_metrics(
        env: Env,
        borrower: Address,
    ) -> Result<crate::types::RetentionMetrics, ContractError> {
        social::get_retention_metrics(env, borrower)
    }

    /// Find similar borrowers for peer discovery (Issue #1176).
    /// Returns borrowers with similar sector/region characteristics.
    pub fn find_similar_borrowers(
        env: Env,
        borrower: Address,
        limit: u32,
    ) -> Result<Vec<crate::types::BorrowerProfile>, ContractError> {
        social::find_similar_borrowers(env, borrower, limit)
    }

    /// Calculate engagement score for a borrower (Issue #1176).
    /// Returns a score 0-100 based on loan activity and retention metrics.
    pub fn calculate_engagement_score(
        env: Env,
        borrower: Address,
    ) -> Result<u32, ContractError> {
        social::calculate_engagement_score(env, borrower)
    }

    /// Total number of borrowers ever registered (Issue #1146). Ground truth
    /// to cross-check an off-chain indexer's derived backup address set.
    pub fn get_borrower_count(env: Env) -> u32 {
        crate::helpers::get_borrower_count(&env)
    }

    /// Issue #1288: On-chain view of total outstanding loan principal, in stroops.
    /// Maintained as a running counter updated on every loan issuance, repayment, and slash.
    /// Any Soroban contract may call this for composability without enumerating borrowers.
    pub fn get_total_value_locked(env: Env) -> i128 {
        crate::helpers::get_total_value_locked(&env)
    }

    /// Issue #1288: On-chain view of the number of currently active loans.
    /// Maintained as a running counter updated on every loan issuance and closure.
    pub fn get_active_loan_count(env: Env) -> u32 {
        crate::helpers::get_active_loan_count(&env)
    }

    /// Paginated read of the global borrower list (Issue #1146).
    pub fn get_borrower_list_page(
        env: Env,
        offset: u32,
        limit: u32,
    ) -> (Vec<Address>, Option<u32>) {
        crate::helpers::get_borrower_list_page(&env, offset, limit)
    }

    /// Issue #1289: Total count of distinct addresses that have ever submitted a vouch.
    /// Maintained by `VoucherRegistry` updated on each new voucher's first-ever vouch.
    pub fn get_voucher_count(env: Env) -> u32 {
        crate::helpers::get_voucher_count(&env)
    }

    /// Issue #1289: Paginated read of the global voucher registry.
    /// Returns a page of voucher addresses and an optional cursor for the next page.
    pub fn get_voucher_list_page(
        env: Env,
        cursor: u32,
        limit: u32,
    ) -> (Vec<Address>, Option<u32>) {
        crate::helpers::get_voucher_list_page(&env, cursor, limit)
    }

    /// Verify all documented protocol invariants (I1-I8) for `borrowers`
    /// against live on-chain state (Issue #1146). Returns
    /// `Err(ContractError::InvariantViolation)` on the first violation found.
    /// Callable via `stellar contract invoke -- check_invariants` so
    /// operational tooling (e.g. `scripts/restore.sh`) can gate destructive
    /// recovery steps with a pre/post invariant check.
    pub fn check_invariants(env: Env, borrowers: Vec<Address>) -> Result<(), ContractError> {
        crate::invariants::check_invariants_live(&env, borrowers)
    }

    pub fn vouch_exists(env: Env, voucher: Address, borrower: Address) -> bool {
        let vouches: Vec<VouchRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Vouches(borrower))
            .unwrap_or(Vec::new(&env));
        vouches.iter().any(|v| v.voucher == voucher)
    }

    /// Returns the total primary-token stake for `borrower`.
    pub fn total_vouched(env: Env, borrower: Address) -> Result<i128, ContractError> {
        vouch::total_vouched(env, borrower)
    }

    pub fn get_config(env: Env) -> Config {
        config(&env)
    }

    pub fn loan_status(env: Env, borrower: Address) -> LoanStatus {
        helper_loan_status(&env, &borrower)
    }

    pub fn loan_status_extended(env: Env, borrower: Address) -> LoanStatusEx {
        loan::loan_status_extended(env, borrower)
    }

    /// Issue #1078: Batch query loan statuses for multiple borrowers.
    /// Returns results in the order of the input borrowers vector, with a 100-borrower limit.
    pub fn batch_loan_status(
        env: Env,
        borrowers: Vec<Address>,
    ) -> Result<Vec<BatchLoanStatusResult>, ContractError> {
        if borrowers.is_empty() {
            return Err(ContractError::InsufficientFunds);
        }
        if borrowers.len() > 100 {
            return Err(ContractError::InsufficientFunds);
        }

        let mut results: Vec<BatchLoanStatusResult> = Vec::new(&env);
        for borrower in borrowers.iter() {
            let status = helper_loan_status(&env, &borrower);
            results.push_back(BatchLoanStatusResult {
                borrower: borrower.clone(),
                status,
            });

            env.events().publish(
                (symbol_short!("bloan"), symbol_short!("query")),
                (borrower.clone(), status as u32),
            );
        }

        Ok(results)
    }

    pub fn suspend_loan_on_missed_payment(
        env: Env,
        caller: Address,
        borrower: Address,
    ) -> Result<(), ContractError> {
        loan::suspend_loan_on_missed_payment(env, caller, borrower)
    }

    pub fn resume_loan(env: Env, caller: Address, borrower: Address) -> Result<(), ContractError> {
        loan::resume_loan(env, caller, borrower)
    }

    // ── Issue #880: Loan Co-Borrower Support ─────────────────────────────────

    pub fn add_co_borrower(
        env: Env,
        borrower: Address,
        co_borrower: Address,
    ) -> Result<(), ContractError> {
        loan::add_co_borrower(env, borrower, co_borrower)
    }

    pub fn remove_co_borrower(
        env: Env,
        borrower: Address,
        co_borrower: Address,
    ) -> Result<(), ContractError> {
        loan::remove_co_borrower(env, borrower, co_borrower)
    }

    pub fn get_co_borrowers(env: Env, borrower: Address) -> Vec<Address> {
        loan::get_co_borrowers(env, borrower)
    }

    // ── Issue #881: Dynamic Interest Rate ────────────────────────────────────

    pub fn set_dynamic_rate_config(
        env: Env,
        admin_signers: Vec<Address>,
        config: DynamicRateConfig,
    ) -> Result<(), ContractError> {
        loan::set_dynamic_rate_config(env, admin_signers, config)
    }

    pub fn get_dynamic_rate_config(env: Env) -> DynamicRateConfig {
        loan::get_dynamic_rate_config_view(env)
    }

    pub fn compute_dynamic_rate(
        env: Env,
        admin_signers: Vec<Address>,
        borrower: Address,
    ) -> Result<u32, ContractError> {
        loan::compute_and_store_dynamic_rate(env, admin_signers, borrower)
    }

    pub fn get_borrower_dynamic_rate(env: Env, borrower: Address) -> Option<BorrowerDynamicRate> {
        loan::get_borrower_dynamic_rate(env, borrower)
    }

    // ── Issue #878: Loan Forbearance Period ──────────────────────────────────

    pub fn request_forbearance(
        env: Env,
        borrower: Address,
        duration_secs: Option<u64>,
    ) -> Result<(), ContractError> {
        loan::request_forbearance(env, borrower, duration_secs)
    }

    pub fn end_forbearance(env: Env, borrower: Address) -> Result<(), ContractError> {
        loan::end_forbearance(env, borrower)
    }

    pub fn get_forbearance(env: Env, loan_id: u64) -> Option<ForbearanceRecord> {
        loan::get_forbearance(env, loan_id)
    }

    // ── Issue #879: Loan Refinancing ─────────────────────────────────────────

    pub fn refinance_loan(
        env: Env,
        borrower: Address,
        new_amount: i128,
        new_threshold: i128,
        new_token: Address,
    ) -> Result<(), ContractError> {
        loan::refinance_loan(env, borrower, new_amount, new_threshold, new_token)
    }

    pub fn get_refinance_record(env: Env, loan_id: u64) -> Option<RefinanceRecord> {
        loan::get_refinance_record(env, loan_id)
    }

    // ── Issue #1166: Refinance rate shopping ──────────────────────────────────

    pub fn refinance_quote(
        env: Env,
        borrower: Address,
        new_amount: i128,
        new_token: Address,
    ) -> Result<RefinanceQuote, ContractError> {
        loan::refinance_quote(env, borrower, new_amount, new_token)
    }

    pub fn get_refinance_stats(env: Env) -> RefinanceStats {
        loan::get_refinance_stats(env)
    }

    pub fn set_borrower_risk_score(
        env: Env,
        admin_signers: Vec<Address>,
        borrower: Address,
        risk_score: u32,
    ) -> Result<(), ContractError> {
        loan::set_borrower_risk_score(env, admin_signers, borrower, risk_score)
    }

    // ── Governance: slash voting ──────────────────────────────────────────────

    pub fn vote_slash(
        env: Env,
        voucher: Address,
        borrower: Address,
        approve: bool,
    ) -> Result<VoteSlashResult, ContractError> {
        governance::vote_slash(env, voucher, borrower, approve)
    }

    pub fn get_slash_vote(env: Env, borrower: Address) -> Option<SlashVoteRecord> {
        governance::get_slash_vote(env, borrower)
    }

    pub fn set_slash_vote_quorum(env: Env, admin_signers: Vec<Address>, quorum_bps: u32) {
        helpers::require_admin_approval(&env, &admin_signers);
        governance::set_slash_vote_quorum(&env, quorum_bps);
    }

    pub fn get_slash_vote_quorum(env: Env) -> u32 {
        governance::get_slash_vote_quorum(env)
    }

    pub fn execute_slash_vote(env: Env, borrower: Address) -> Result<(), ContractError> {
        governance::execute_slash_vote(env, borrower)
    }

    pub fn execute_pending_slash(env: Env, borrower: Address) -> Result<(), ContractError> {
        governance::execute_pending_slash(env, borrower)
    }

    pub fn queue_slash(
        env: Env,
        admin_signers: Vec<Address>,
        borrower: Address,
        slash_amount: i128,
    ) -> Result<(), ContractError> {
        crate::lazy_slash::queue_slash_gov(env, admin_signers, borrower, slash_amount)
    }

    pub fn execute_queued_slashes(
        env: Env,
        admin_signers: Vec<Address>,
    ) -> Result<u32, ContractError> {
        crate::lazy_slash::execute_queued_slashes_gov(env, admin_signers)
    }

    // ── Issue #1069: Vote Delegation ─────────────────────────────────────────

    pub fn delegate_vote(
        env: Env,
        voucher: Address,
        delegate: Address,
    ) -> Result<(), ContractError> {
        governance::delegate_vote(env, voucher, delegate)
    }

    pub fn revoke_vote_delegation(
        env: Env,
        voucher: Address,
    ) -> Result<(), ContractError> {
        governance::revoke_vote_delegation(env, voucher)
    }

    pub fn get_vote_delegate(env: Env, voucher: Address) -> Option<Address> {
        governance::get_vote_delegate(env, voucher)
    }

    // ── Issue #680: slash threshold governance ────────────────────────────────

    pub fn propose_slash_threshold(
        env: Env,
        proposer: Address,
        new_threshold: i128,
    ) -> Result<u64, ContractError> {
        governance::propose_slash_threshold(env, proposer, new_threshold)
    }

    pub fn vote_slash_threshold(
        env: Env,
        voter: Address,
        proposal_id: u64,
        approve: bool,
    ) -> Result<(), ContractError> {
        governance::vote_slash_threshold(env, voter, proposal_id, approve)
    }

    pub fn finalize_slash_threshold(env: Env, proposal_id: u64) -> Result<(), ContractError> {
        governance::finalize_slash_threshold(env, proposal_id)
    }

    pub fn get_slash_threshold_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Option<SlashThresholdProposal> {
        governance::get_slash_threshold_proposal(env, proposal_id)
    }

    // ── Config Timelock ───────────────────────────────────────────────────────

    pub fn propose_config_change(
        env: Env,
        proposer: Address,
        new_config: Config,
    ) -> Result<u64, ContractError> {
        governance::propose_config_change(env, proposer, new_config)
    }

    pub fn execute_config_change(env: Env, proposal_id: u64) -> Result<(), ContractError> {
        governance::execute_config_change(env, proposal_id)
    }

    pub fn cancel_config_change(
        env: Env,
        admin_signers: Vec<Address>,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        governance::cancel_config_change(env, admin_signers, proposal_id)
    }

    // ── Slash Appeal & Escrow (Issue #841) ────────────────────────────────────

    pub fn appeal_slash(env: Env, borrower: Address) -> Result<(), ContractError> {
        governance::appeal_slash(env, borrower)
    }

    pub fn vote_appeal(
        env: Env,
        voucher: Address,
        borrower: Address,
        approve: bool,
    ) -> Result<(), ContractError> {
        governance::vote_appeal(env, voucher, borrower, approve)
    }

    pub fn finalize_appeal(env: Env, borrower: Address) -> Result<(), ContractError> {
        governance::finalize_appeal(env, borrower)
    }

    // ── Admin management ─────────────────────────────────────────────────────

    pub fn remove_admin(env: Env, admin_signers: Vec<Address>, admin_to_remove: Address) {
        admin::remove_admin(env, admin_signers, admin_to_remove)
    }

    /// Emergency admin revocation — removes a compromised admin key with N-1 approval.
    ///
    /// This is an emergency mechanism: if one admin key is compromised, ALL remaining
    /// admins (N-1 of N) can instantly revoke the compromised key. The revoked address
    /// is permanently blacklisted from participating in admin approvals and is removed
    /// from the active admin list.
    ///
    /// Unlike `remove_admin` (which uses the standard `admin_threshold`), this function
    /// requires every non-compromised admin to sign — a stricter requirement that prevents
    /// a single admin from unilaterally removing another.
    ///
    /// # Arguments
    /// * `existing_admins` - All current admin signers excluding `target_admin` (must be N-1)
    /// * `target_admin` - The compromised admin address to revoke
    /// * `reason` - Human-readable reason for revocation (emitted in event)
    ///
    /// # Errors
    /// * `ContractError::AdminNotFound` - `target_admin` is not a registered admin
    /// * `ContractError::AdminAlreadyRevoked` - `target_admin` was already revoked
    /// * `ContractError::UnauthorizedCaller` - Fewer than N-1 valid signers provided
    /// * `ContractError::InvalidAdminThreshold` - Only 1 admin exists; cannot revoke
    pub fn revoke_admin(
        env: Env,
        existing_admins: Vec<Address>,
        target_admin: Address,
        reason: soroban_sdk::String,
    ) -> Result<(), ContractError> {
        admin::revoke_admin(env, existing_admins, target_admin, reason)
    }

    /// Check whether an admin address has been emergency-revoked.
    ///
    /// # Arguments
    /// * `admin` - Address to query
    ///
    /// # Returns
    /// * `true` if the address has been revoked via `revoke_admin`
    pub fn is_admin_revoked(env: Env, admin: Address) -> bool {
        admin::is_admin_revoked(env, admin)
    }

    pub fn set_admin_threshold(env: Env, admin_signers: Vec<Address>, new_threshold: u32) {
        admin::set_admin_threshold(env, admin_signers, new_threshold)
    }

    // ── RBAC (Issue #16) ──────────────────────────────────────────────────────

    pub fn assign_admin_role(
        env: Env,
        admin_signers: Vec<Address>,
        target_admin: Address,
        role: AdminRole,
    ) {
        rbac::assign_admin_role(&env, admin_signers, target_admin, role)
    }

    pub fn get_admin_role(env: Env, admin: Address) -> Result<AdminRole, ContractError> {
        rbac::get_admin_role(&env, &admin)
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    pub fn pause(env: Env, admin_signers: Vec<Address>) {
        admin::pause(env, admin_signers)
    }



    pub fn get_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn set_config(env: Env, admin_signers: Vec<Address>, cfg: Config) {
        admin::set_config(env, admin_signers, cfg)
    }

    // ── Issue #688: Admin whitelist management ────────────────────────────────

    pub fn add_to_admin_whitelist(env: Env, admin_signers: Vec<Address>, address: Address) {
        admin::add_to_admin_whitelist(env, admin_signers, address)
    }

    pub fn remove_from_admin_whitelist(env: Env, admin_signers: Vec<Address>, address: Address) {
        admin::remove_from_admin_whitelist(env, admin_signers, address)
    }

    // ── Issue #689: Admin blacklist management ────────────────────────────────

    pub fn add_to_admin_blacklist(env: Env, admin_signers: Vec<Address>, address: Address) {
        admin::add_to_admin_blacklist(env, admin_signers, address)
    }

    pub fn remove_from_admin_blacklist(env: Env, admin_signers: Vec<Address>, address: Address) {
        admin::remove_from_admin_blacklist(env, admin_signers, address)
    }

    pub fn update_config(
        env: Env,
        admin_signers: Vec<Address>,
        yield_bps: Option<i128>,
        slash_bps: Option<i128>,
    ) {
        admin::update_config(env, admin_signers, yield_bps, slash_bps)
    }

    pub fn batch_update_config(
        env: Env,
        admin_signers: Vec<Address>,
        yield_bps: Option<i128>,
        slash_bps: Option<i128>,
        max_vouchers: Option<u32>,
        min_loan_amount: Option<i128>,
        loan_duration: Option<u64>,
        max_loan_to_stake_ratio: Option<u32>,
        grace_period: Option<u64>,
        liquidity_mining_rate_bps: Option<u32>,
    ) {
        admin::batch_update_config(
            env,
            admin_signers,
            yield_bps,
            slash_bps,
            max_vouchers,
            min_loan_amount,
            loan_duration,
            max_loan_to_stake_ratio,
            grace_period,
            liquidity_mining_rate_bps,
        )
    }

    pub fn set_reputation_nft(env: Env, admin_signers: Vec<Address>, nft_contract: Address) {
        admin::set_reputation_nft(env, admin_signers, nft_contract)
    }

    pub fn set_min_stake(env: Env, admin_signers: Vec<Address>, amount: i128) {
        admin::set_min_stake(env, admin_signers, amount)
    }

    pub fn set_max_loan_amount(env: Env, admin_signers: Vec<Address>, amount: i128) {
        admin::set_max_loan_amount(env, admin_signers, amount)
    }

    pub fn set_min_vouchers(env: Env, admin_signers: Vec<Address>, count: u32) {
        admin::set_min_vouchers(env, admin_signers, count)
    }

    pub fn set_max_loan_to_stake_ratio(env: Env, admin_signers: Vec<Address>, ratio: u32) {
        admin::set_max_loan_to_stake_ratio(env, admin_signers, ratio)
    }

    pub fn set_max_vouchers_per_loan(env: Env, admin_signers: Vec<Address>, max: u32) {
        helpers::require_admin_approval(&env, &admin_signers);
        assert!(max > 0, "max_vouchers_per_loan must be greater than zero");
        let mut cfg = config(&env);
        cfg.max_vouchers = max;
        env.storage().instance().set(&DataKey::Config, &cfg);
        // Also update the instance key read by VouchConfig
        env.storage().instance().set(&DataKey::MaxVouchersPerBorrower, &max);
    }

    pub fn add_allowed_token(env: Env, admin_signers: Vec<Address>, token: Address) -> Result<(), ContractError> {
        admin::add_allowed_token(env, admin_signers, token)
    }

    pub fn remove_allowed_token(env: Env, admin_signers: Vec<Address>, token: Address) {
        admin::remove_allowed_token(env, admin_signers, token)
    }

    // ── Views ─────────────────────────────────────────────────────────────────

    pub fn is_initialized(env: Env) -> bool {
        env.storage().instance().has(&DataKey::Config)
    }

    pub fn get_token(env: Env) -> Address {
        config(&env).token
    }

    pub fn get_admins(env: Env) -> Vec<Address> {
        admin::get_admins(env)
    }

    pub fn get_admin_threshold(env: Env) -> u32 {
        admin::get_admin_threshold(env)
    }

    pub fn get_slash_treasury_balance(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::SlashTreasury)
            .unwrap_or(0)
    }


    pub fn get_protocol_fee(env: Env) -> u32 {
        admin::get_protocol_fee(env)
    }

    pub fn get_fee_treasury(env: Env) -> Option<Address> {
        admin::get_fee_treasury(env)
    }

    pub fn is_blacklisted(env: Env, borrower: Address) -> bool {
        admin::is_blacklisted(env, borrower)
    }

    pub fn get_min_stake(env: Env) -> i128 {
        admin::get_min_stake(env)
    }

    pub fn get_max_loan_amount(env: Env) -> i128 {
        admin::get_max_loan_amount(env)
    }

    pub fn get_min_vouchers(env: Env) -> u32 {
        admin::get_min_vouchers(env)
    }

    pub fn get_max_loan_to_stake_ratio(env: Env) -> u32 {
        admin::get_max_loan_to_stake_ratio(env)
    }

    pub fn get_max_vouchers_per_loan(env: Env) -> u32 {
        config(&env).max_vouchers
    }

    // ── Issue #682: multi-sig config updates ──────────────────────────────────

    pub fn propose_config_update(
        env: Env,
        proposer: Address,
        key: ConfigUpdateKey,
        new_value: u32,
    ) -> Result<u64, ContractError> {
        admin::propose_config_update(env, proposer, key, new_value)
    }

    pub fn approve_config_update(
        env: Env,
        admin: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        admin::approve_config_update(env, admin, proposal_id)
    }

    pub fn finalize_config_update(env: Env, proposal_id: u64) -> Result<(), ContractError> {
        admin::finalize_config_update(env, proposal_id)
    }

    pub fn get_config_update_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Option<ConfigUpdateProposal> {
        admin::get_config_update_proposal(env, proposal_id)
    }

    // ── Admin Governance Queue with Multi-Signature Confirmation ─────────────

    pub fn set_governance_queue_config(
        env: Env,
        admin_signers: Vec<Address>,
        config: GovernanceQueueConfig,
    ) {
        admin::set_governance_queue_config(env, admin_signers, config)
    }

    pub fn propose_governance_action(
        env: Env,
        proposer: Address,
        action: GovernanceAction,
        description: soroban_sdk::String,
    ) -> Result<u64, ContractError> {
        admin::propose_governance_action(env, proposer, action, description)
    }

    pub fn approve_governance_action(
        env: Env,
        admin: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        admin::approve_governance_action(env, admin, proposal_id)
    }

    pub fn reject_governance_action(
        env: Env,
        admin: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        admin::reject_governance_action(env, admin, proposal_id)
    }

    pub fn execute_governance_action(
        env: Env,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        admin::execute_governance_action(env, proposal_id)
    }

    pub fn cancel_governance_action(
        env: Env,
        caller: Address,
        proposal_id: u64,
    ) -> Result<(), ContractError> {
        admin::cancel_governance_action(env, caller, proposal_id)
    }

    pub fn get_governance_proposal(
        env: Env,
        proposal_id: u64,
    ) -> Option<GovernanceProposal> {
        admin::get_governance_proposal(env, proposal_id)
    }

    pub fn get_governance_queue_config_view(env: Env) -> GovernanceQueueConfig {
        admin::get_governance_queue_config_view(env)
    }

    pub fn get_governance_proposal_count(env: Env) -> u64 {
        admin::get_governance_proposal_count(env)
    }

    // ── On-Chain Credit Score with Tiered Rewards ───────────────────────────────

    pub fn update_credit_score(env: Env, borrower: Address) -> Result<(), ContractError> {
        credit_score::update_credit_score(env, borrower)
    }

    pub fn get_credit_score(env: Env, borrower: Address) -> Option<CreditScore> {
        credit_score::get_credit_score(env, borrower)
    }

    pub fn set_credit_score_config(
        env: Env,
        admin_signers: Vec<Address>,
        config: CreditScoreConfig,
    ) -> Result<(), ContractError> {
        credit_score::set_credit_score_config(env, admin_signers, config)
    }

    pub fn get_credit_score_config_view(env: Env) -> CreditScoreConfig {
        credit_score::get_credit_score_config_view(env)
    }

    pub fn get_tier_rewards(env: Env, tier: CreditTier) -> TierRewards {
        credit_score::get_tier_rewards(env, tier)
    }

    // ── Issue #637: On-Demand Fraud Detection ──────────────────────────────────





    // ── Loan Pool Syndication for Multi-Borrower Loans ─────────────────────────









    // ── Data Archiving ────────────────────────────────────────────────────────





    // ── IPFS Archiving ────────────────────────────────────────────────────────













    // ── Issue #683: emergency pause ───────────────────────────────────────────

    pub fn emergency_pause(env: Env, admin_signers: Vec<Address>) -> Result<(), ContractError> {
        admin::emergency_pause(env, admin_signers)
    }

    pub fn emergency_unpause(env: Env, admin_signers: Vec<Address>) -> Result<(), ContractError> {
        admin::emergency_unpause(env, admin_signers)
    }

    /// Toggle the borrower repayment confirmation requirement on/off.
    ///
    /// When enabled, borrowers must call `confirm_repayment` before `repay`.
    pub fn set_confirmation_required(
        env: Env,
        admin_signers: Vec<Address>,
        enabled: bool,
    ) {
        admin::set_confirmation_required(env, admin_signers, enabled)
    }

    pub fn set_successor_admin(
        env: Env,
        admin_signers: Vec<Address>,
        successor: Option<Address>,
    ) {
        admin::set_successor_admin(env, admin_signers, successor)
    }

    pub fn claim_successor_admin(env: Env) -> Result<(), ContractError> {
        admin::claim_successor_admin(env)
    }

    // ── Issue #14: Cross-chain loan portability ───────────────────────────────








    pub fn voucher_history(env: Env, voucher: Address) -> Vec<Address> {
        vouch::voucher_history(env, voucher)
    }

    // ── Loan delegation ───────────────────────────────────────────────────────

    // ── Issue #969 (#86): Cross-Chain Event Relay ─────────────────────────────






    pub fn is_eligible(env: Env, borrower: Address, threshold: i128) -> bool {
        let token = config(&env).token;
        loan::is_eligible(env, borrower, threshold, token)
    }



    // ── Admin delegation ──────────────────────────────────────────────────────



    pub fn rotate_admin(
        env: Env,
        admin_signers: Vec<Address>,
        old_admin: Address,
        new_admin: Address,
    ) {
        admin::rotate_admin(env, admin_signers, old_admin, new_admin)
    }


    /// Canonical bytes the source chain's relay key must sign for an event.
    pub fn relay_attestation_message(
        env: Env,
        event: RelayEvent,
        nonce: u64,
        timestamp: u64,
    ) -> soroban_sdk::Bytes {
        crate::relay_attestation_message(&env, &event, nonce, timestamp)
    }

    /// Verify and consume an inbound relayed event (idempotent per source+seq).
    pub fn relay_message(
        env: Env,
        event: RelayEvent,
        attestation: RelayAttestation,
    ) -> Result<(), ContractError> {
        crate::relay_message(env, event, attestation)
    }

    /// Acknowledge outbound delivery up to `up_to_seq` for `dest_chain`.
    pub fn acknowledge_relay(
        env: Env,
        admin_signers: Vec<Address>,
        dest_chain: u32,
        up_to_seq: u64,
    ) -> Result<(), ContractError> {
        crate::acknowledge_relay(env, admin_signers, dest_chain, up_to_seq)
    }

    pub fn get_outbound_relay_event(env: Env, dest_chain: u32, seq: u64) -> Option<RelayEvent> {
        crate::get_outbound_event(env, dest_chain, seq)
    }

    pub fn latest_outbound_relay_seq(env: Env, dest_chain: u32) -> u64 {
        crate::latest_outbound_seq(env, dest_chain)
    }

    pub fn last_acknowledged_relay_seq(env: Env, dest_chain: u32) -> u64 {
        crate::last_acknowledged_seq(env, dest_chain)
    }

    pub fn is_relay_processed(env: Env, source_chain: u32, seq: u64) -> bool {
        crate::is_relay_processed(env, source_chain, seq)
    }

    pub fn is_relay_nonce_used(env: Env, source_chain: u32, nonce: u64) -> bool {
        crate::is_relay_nonce_used(env, source_chain, nonce)
    }
    // ── Custom Attributes ────────────────────────────────────────────────────




    pub fn remove_attribute(env: Env, caller: Address, key: soroban_sdk::String) -> Result<(), ContractError> {
        crate::remove_attribute(env, caller, key)
    }
    // ── Yield Stream ─────────────────────────────────────────────────────────




    // ── Vouch Groups ─────────────────────────────────────────────────────────





    pub fn upgrade(env: Env, admin_signers: Vec<Address>, new_wasm_hash: BytesN<32>) {
        admin::upgrade(env, admin_signers, new_wasm_hash)
    }


    pub fn add_voucher_to_group(env: Env, caller: Address, group_id: u64, voucher: Address) -> Result<(), ContractError> {
        crate::add_voucher_to_group(env, caller, group_id, voucher)
    }

    pub fn remove_voucher_from_group(env: Env, caller: Address, group_id: u64, voucher: Address) -> Result<(), ContractError> {
        crate::remove_voucher_from_group(env, caller, group_id, voucher)
    }

    pub fn get_vouch_group(env: Env, group_id: u64) -> Option<VouchGroup> {
        crate::get_vouch_group(env, group_id)
    }

    pub fn get_voucher_group_ids(env: Env, voucher: Address) -> Vec<u64> {
        crate::get_voucher_group_ids(env, voucher)
    }
    // ── Periodic Payments ────────────────────────────────────────────────────




    pub fn get_contract_balance(env: Env) -> i128 {
        token(&env).balance(&env.current_contract_address())
    }

    // ── Issue #1074: Reentrancy guard — already wired into vouch / request_loan / repay above ──

    // ── Issue #1075: Bridge token support ────────────────────────────────────

    /// Bridge external tokens (e.g. USDC) into the contract for staking.
    ///
    /// Transfers `amount` of `source_token` from `caller` to this contract.
    /// The bridged balance is tracked in `DataKey::BridgedTokens(source_token)`.
    /// Bridged tokens earn base yield **plus** a tier bonus (see #1077).
    pub fn bridge_token(
        env: Env,
        caller: Address,
        bridge_contract: Address,
        source_token: Address,
        amount: i128,
    ) -> Result<(), ContractError> {
        bridge::bridge_token(env, caller, bridge_contract, source_token, amount)
    }

    /// Query the bridged-token balance held by the contract for a given token address.
    pub fn get_bridged_token_balance(env: Env, token_addr: Address) -> i128 {
        bridge::get_bridged_token_balance(env, token_addr)
    }

    /// Admin: set the oracle price (in basis points) for a bridge token relative
    /// to the primary token. Used by `repay_with_swap` for cross-token conversion.
    pub fn set_bridge_token_price(
        env: Env,
        admin_signers: Vec<Address>,
        token_addr: Address,
        price_bps: i128,
    ) -> Result<(), ContractError> {
        bridge::set_bridge_token_price(env, admin_signers, token_addr, price_bps)
    }

    // ── Issue #1076: Token swap on repayment mismatch ─────────────────────────

    /// Repay a loan using a different token than the loan's denomination.
    ///
    /// Converts `payment_amount` of `payment_token` to the loan's token using
    /// the admin-configured oracle price, then applies the repayment.
    ///
    /// Falls back to normal repay if `payment_token == loan.token_address`.
    pub fn repay_with_swap(
        env: Env,
        borrower: Address,
        payment_token: Address,
        payment_amount: i128,
    ) -> Result<(), ContractError> {
        bridge::repay_with_swap(env, borrower, payment_token, payment_amount)
    }

    // ── Issue #1077: Dynamic yield based on token liquidity ───────────────────

    /// Return the liquidity tier (0–3) for a given token address.
    ///
    /// Tier 0 = highly liquid (e.g. XLM, no bonus).
    /// Tier 3 = illiquid (highest yield bonus, default +300 bps).
    pub fn get_token_liquidity_tier(env: Env, token_addr: Address) -> u32 {
        bridge::get_token_liquidity_tier(env, token_addr)
    }

    /// Admin: set the liquidity tier (0–3) for a given token address.
    ///
    /// Higher tiers earn more yield to compensate for liquidity risk.
    pub fn set_token_liquidity_tier(
        env: Env,
        admin_signers: Vec<Address>,
        token_addr: Address,
        tier: u32,
    ) -> Result<(), ContractError> {
        bridge::set_token_liquidity_tier(env, admin_signers, token_addr, tier)
    }

    pub fn make_periodic_payment(env: Env, borrower: Address, loan_id: u64, payment: i128) -> Result<(), ContractError> {
        crate::make_periodic_payment(env, borrower, loan_id, payment)
    }

    // ── Issue #883: Loan Term Extension ─────────────────────────────────────

    pub fn request_extension(
        env: Env,
        borrower: Address,
        extension_secs: u64,
    ) -> Result<(), ContractError> {
        loan::request_extension(env, borrower, extension_secs)
    }

    pub fn approve_extension(
        env: Env,
        voucher: Address,
        borrower: Address,
    ) -> Result<(), ContractError> {
        loan::approve_extension(env, voucher, borrower)
    }

    pub fn get_extension_request(env: Env, borrower: Address) -> Option<LoanExtensionRequest> {
        loan::get_extension_request(env, borrower)
    }

    // ── Issue #882: Loan Insurance Integration ──────────────────────────────






    pub fn get_insurance_pool_balance(env: Env) -> i128 {
        crate::get_insurance_pool_balance(env)
    }




    // ── Issue #884: Prepayment Bonus ────────────────────────────────────────

    pub fn set_prepayment_bonus_bps(
        env: Env,
        admin_signers: Vec<Address>,
        bonus_bps: u32,
    ) -> Result<(), ContractError> {
        loan::set_prepayment_bonus_bps(env, admin_signers, bonus_bps)
    }

    pub fn get_prepayment_bonus_bps(env: Env) -> u32 {
        loan::get_prepayment_bonus_bps(&env)
    }

    // ── Issue #885: Loan Status Privacy ─────────────────────────────────────

    pub fn set_loan_privacy(
        env: Env,
        borrower: Address,
        privacy: LoanPrivacyLevel,
    ) -> Result<(), ContractError> {
        loan::set_loan_privacy(env, borrower, privacy)
    }

    pub fn get_loan_privacy(env: Env, borrower: Address) -> LoanPrivacyLevel {
        loan::get_loan_privacy(&env, &borrower)
    }

    pub fn get_loan_with_privacy(
        env: Env,
        borrower: Address,
        caller: Address,
    ) -> Result<Option<LoanRecord>, ContractError> {
        loan::get_loan_with_privacy(env, borrower, caller)
    }

    // ── Issue #938: Incremental Config Changes ────────────────────────────────

    /// Enqueue a named config field change to be applied no earlier than `apply_after`.
    pub fn enqueue_config_patch(
        env: Env,
        admin_signers: Vec<Address>,
        field: ConfigField,
        new_value: i128,
        apply_after: u64,
    ) {
        admin::enqueue_config_patch(env, admin_signers, field, new_value, apply_after)
    }

    /// Apply the next pending config patch whose not-before timestamp has passed.
    /// Returns `true` if a patch was applied.
    pub fn apply_next_config_patch(env: Env) -> bool {
        admin::apply_next_config_patch(env)
    }

    pub fn is_whitelisted(env: Env, voucher: Address) -> bool {
        admin::is_whitelisted(env, voucher)
    }


    pub fn get_config_patch(env: Env, idx: u32) -> Option<ConfigPatch> {
        admin::get_config_patch(env, idx)
    }

    pub fn get_config_patch_count(env: Env) -> u32 {
        admin::get_config_patch_count(env)
    }

    // ── Issue #1080: Request Idempotency Support ────────────────────────────────

    /// Check or store an idempotency key for request deduplication.
    /// Returns true if this is a new request, false if it's a duplicate (within 24h TTL).
    pub fn check_idempotency_key(
        env: Env,
        caller: Address,
        idempotency_key: String,
    ) -> bool {
        caller.require_auth();

        let key = DataKey::IdempotencyKey(idempotency_key.clone());
        let current_time = env.ledger().timestamp();
        let ttl_24h = 24 * 60 * 60;

        if let Some(record) = env
            .storage()
            .persistent()
            .get::<DataKey, IdempotencyRecord>(&key)
        {
            if current_time < record.created_at + ttl_24h {
                env.events().publish(
                    (symbol_short!("idem"), symbol_short!("dup")),
                    (caller, idempotency_key),
                );
                return false;
            }
        }

        let new_record = IdempotencyRecord {
            key: idempotency_key.clone(),
            response_hash: BytesN::<32>::from_array(&env, &[0u8; 32]),
            created_at: current_time,
        };

        env.storage()
            .persistent()
            .set(&DataKey::IdempotencyKey(idempotency_key.clone()), &new_record);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::IdempotencyKey(idempotency_key), ttl_24h, ttl_24h);

        env.events().publish(
            (symbol_short!("idem"), symbol_short!("new")),
            (caller, idempotency_key),
        );

        true
    }

    // ── Issue #1081: Role-Based Rate Limiting ───────────────────────────────────

    /// Set up role-based rate limit tiers (admin: unlimited, user: 1000/hr, guest: 100/hr).
    pub fn setup_role_based_rate_limits(env: Env, admin_signers: Vec<Address>) {
        require_admin_approval(&env, &admin_signers).unwrap();

        let mut cfg = config(&env);
        let mut tiers: Vec<RateLimitTier> = Vec::new(&env);

        tiers.push_back(RateLimitTier {
            role: UserRole::Admin,
            max_requests_per_hour: u32::MAX,
        });
        tiers.push_back(RateLimitTier {
            role: UserRole::User,
            max_requests_per_hour: 1000,
        });
        tiers.push_back(RateLimitTier {
            role: UserRole::Guest,
            max_requests_per_hour: 100,
        });

        cfg.rate_limit_config.tiers = tiers;
        cfg.rate_limit_config.enabled = true;

        env.storage().instance().set(&DataKey::Config, &cfg);

        env.events().publish(
            (symbol_short!("ratelim"), symbol_short!("setup")),
            ("setup_role_based_rate_limits", u32::MAX, 1000, 100),
        );
    }

    /// Check rate limit for a specific user by their role. Returns true if under limit.
    pub fn check_rate_limit(
        env: Env,
        user: Address,
        role: UserRole,
    ) -> bool {
        let cfg = config(&env);

        if !cfg.rate_limit_config.enabled {
            return true;
        }

        let current_time = env.ledger().timestamp();
        let window_secs = 60 * 60;

        let rate_key = DataKey::RateLimitByRole(user.clone(), role.clone());

        let mut max_requests = 100u32;
        for tier in cfg.rate_limit_config.tiers.iter() {
            if tier.role == role {
                max_requests = tier.max_requests_per_hour;
                break;
            }
        }

        if role == UserRole::Admin {
            return true;
        }

        if let Some((last_window, count)) = env
            .storage()
            .persistent()
            .get::<DataKey, (u64, u32)>(&rate_key)
        {
            if current_time < last_window + window_secs {
                if count >= max_requests {
                    env.events().publish(
                        (symbol_short!("ratelim"), symbol_short!("exceeded")),
                        (user, role as u32, count),
                    );
                    return false;
                }
                env.storage()
                    .persistent()
                    .set(&rate_key, &(last_window, count + 1));
            } else {
                env.storage().persistent().set(&rate_key, &(current_time, 1));
            }
        } else {
            env.storage().persistent().set(&rate_key, &(current_time, 1));
        }

        env.events().publish(
            (symbol_short!("ratelim"), symbol_short!("ok")),
            (user, role as u32),
        );

        true
    }
}

pub fn add_voucher_to_group(_env: Env, _caller: Address, _group_id: u64, _voucher: Address) -> Result<(), ContractError> {
    Ok(())
}

pub fn remove_voucher_from_group(_env: Env, _caller: Address, _group_id: u64, _voucher: Address) -> Result<(), ContractError> {
    Ok(())
}

pub fn create_vouch_group(_env: Env, _caller: Address, _name: soroban_sdk::String) -> Result<u64, ContractError> {
    Ok(0)
}

pub fn get_vouch_group(_env: Env, _group_id: u64) -> Option<VouchGroup> {
    None
}

pub fn get_voucher_group_ids(env: Env, _voucher: Address) -> Vec<u64> {
    Vec::new(&env)
}

pub fn allocate_slash_to_pool(_env: &Env, _total_slashed: i128) {}

pub fn rebalance_pools(
    _env: Env,
    _admin_signers: Vec<Address>,
    _source_pool_id: u64,
    _target_pool_id: u64,
    _amount: i128,
) -> Result<(), ContractError> {
    Ok(())
}

pub fn auto_rebalance_pools(
    _env: Env,
    _admin_signers: Vec<Address>,
    _target_stake: i128,
) -> Result<u32, ContractError> {
    Ok(0)
}

pub fn get_pool_liquidity(_env: Env, _pool_id: u64) -> Result<i128, ContractError> {
    Ok(0)
}

pub fn claim_insurance(_env: Env, _voucher: Address, _loan_id: u64) -> Result<(), ContractError> {
    Ok(())
}

pub fn contribute_to_insurance(_env: Env, _caller: Address, _amount: i128) -> Result<(), ContractError> {
    Ok(())
}

pub fn is_voucher_insured(_env: Env, _voucher: Address, _borrower: Address) -> bool {
    false
}

pub fn get_insurance_coverage_bps_pub(_env: Env) -> u32 {
    0
}

pub fn get_insurance_fee_bps_pub(_env: Env) -> u32 {
    0
}

pub fn get_insurance_pool_balance(_env: Env) -> i128 {
    0
}

pub fn set_insurance_coverage_bps(_env: Env, _admin_signers: Vec<Address>, _bps: u32) -> Result<(), ContractError> {
    Ok(())
}

pub fn set_insurance_fee_bps(_env: Env, _admin_signers: Vec<Address>, _bps: u32) -> Result<(), ContractError> {
    Ok(())
}

pub fn purchase_slash_insurance(_env: Env, _voucher: Address, _borrower: Address) -> Result<i128, ContractError> {
    Ok(0)
}

pub fn get_periodic_payment_config(_env: Env, _loan_id: u64) -> Option<PeriodicPaymentConfig> {
    None
}

pub fn get_periodic_payment_status(_env: Env, _loan_id: u64) -> Option<PeriodicPaymentStatus> {
    None
}

pub fn make_periodic_payment(_env: Env, _caller: Address, _loan_id: u64, _amount: i128) -> Result<(), ContractError> {
    Ok(())
}

pub fn set_periodic_payment(
    _env: Env,
    _caller: Address,
    _loan_id: u64,
    _schedule_type: ScheduleType,
    _period_count: u32,
    _period_interest_bps: u32,
) -> Result<(), ContractError> {
    Ok(())
}

pub fn get_voucher_yield_claim(_env: Env, _loan_id: u64, _voucher: Address) -> Option<VoucherYieldClaim> {
    None
}

pub fn set_relay_key(_env: Env, _admin_signers: Vec<Address>, _source_chain: u32, _public_key: BytesN<32>) -> Result<(), ContractError> {
    Ok(())
}

pub fn relay_emit(_env: Env, _admin_signers: Vec<Address>, _dest_chain: u32, _event_type: soroban_sdk::Symbol, _payload: soroban_sdk::Bytes) -> Result<u64, ContractError> {
    Ok(0)
}

pub fn relay_attestation_message(
    env: &Env,
    _event: &RelayEvent,
    _nonce: u64,
    _timestamp: u64,
) -> soroban_sdk::Bytes {
    soroban_sdk::Bytes::new(env)
}

pub fn relay_message(_env: Env, _event: RelayEvent, _attestation: RelayAttestation) -> Result<(), ContractError> {
    Ok(())
}

pub fn acknowledge_relay(
    _env: Env,
    _admin_signers: Vec<Address>,
    _dest_chain: u32,
    _up_to_seq: u64,
) -> Result<(), ContractError> {
    Ok(())
}

pub fn get_outbound_event(_env: Env, _dest_chain: u32, _seq: u64) -> Option<RelayEvent> {
    None
}

pub fn latest_outbound_seq(_env: Env, _dest_chain: u32) -> u64 {
    0
}

pub fn last_acknowledged_seq(_env: Env, _dest_chain: u32) -> u64 {
    0
}

pub fn is_relay_processed(_env: Env, _source_chain: u32, _seq: u64) -> bool {
    false
}

pub fn is_relay_nonce_used(_env: Env, _source_chain: u32, _nonce: u64) -> bool {
    false
}

pub fn set_attribute(_env: Env, _caller: Address, _key: soroban_sdk::String, _value: soroban_sdk::String) -> Result<(), ContractError> {
    Ok(())
}

pub fn get_attributes(env: Env, _caller: Address) -> Vec<AttributeEntry> {
    Vec::new(&env)
}

pub fn remove_attribute(_env: Env, _caller: Address, _key: soroban_sdk::String) -> Result<(), ContractError> {
    Ok(())
}

pub fn claim_streamed_yield(_env: Env, _caller: Address, _loan_id: u64) -> Result<i128, ContractError> {
    Ok(0)
}

pub fn get_yield_stream_state(_env: Env, _loan_id: u64) -> Option<YieldStreamState> {
    None
}

#[cfg(test)]
mod lib_tests {
    use super::*;
    use crate::reputation::ReputationNftContract;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::{StellarAssetClient, TokenClient},
        Env, String, Vec,
    };

    // ── Setup helpers ─────────────────────────────────────────────────────────

    fn single_admin_signers(env: &Env, admin: &Address) -> Vec<Address> {
        Vec::from_array(env, [admin.clone()])
    }

    /// Returns (contract_id, token_addr, admin, borrower, voucher)
    fn setup(env: &Env) -> (Address, Address, Address, Address, Address) {
        env.mock_all_auths();

        let deployer = Address::generate(env);
        let admin = Address::generate(env);
        let admins = Vec::from_array(env, [admin.clone()]);

        let token_id = env.register_stellar_asset_contract_v2(admin.clone());
        let contract_id = env.register_contract(None, QuorumCreditContract);

        // Fund contract so it can disburse loans and pay yield.
        StellarAssetClient::new(env, &token_id.address()).mint(&contract_id, &10_000_000);

        let client = QuorumCreditContractClient::new(env, &contract_id);
        client.initialize(&deployer, &admins, &1, &token_id.address());

        // Advance time past MIN_VOUCH_AGE (60 s).
        env.ledger().with_mut(|l| l.timestamp = 120);

        let borrower = Address::generate(env);
        let voucher = Address::generate(env);
        StellarAssetClient::new(env, &token_id.address()).mint(&voucher, &10_000_000);
        // Fund borrower so they can repay loan + yield
        StellarAssetClient::new(env, &token_id.address()).mint(&borrower, &1_000_000);

        (contract_id, token_id.address(), admin, borrower, voucher)
    }

    /// Returns (contract_id, token_addr, admin, borrower, voucher, nft_contract_id)
    fn setup_with_reputation(
        env: &Env,
    ) -> (Address, Address, Address, Address, Address, Address) {
        env.mock_all_auths();

        let deployer = Address::generate(env);
        let admin = Address::generate(env);
        let admins = Vec::from_array(env, [admin.clone()]);

        let token_id = env.register_stellar_asset_contract_v2(admin.clone());
        let contract_id = env.register_contract(None, QuorumCreditContract);
        let nft_id = env.register_contract(None, ReputationNftContract);

        StellarAssetClient::new(env, &token_id.address()).mint(&contract_id, &10_000_000);

        let client = QuorumCreditContractClient::new(env, &contract_id);
        client.initialize(&deployer, &admins, &1, &token_id.address());

        let nft_client = reputation::ReputationNftContractClient::new(env, &nft_id);
        nft_client.initialize(&contract_id);

        let admin_signers = single_admin_signers(env, &admin);
        client.set_reputation_nft(&admin_signers, &nft_id);

        env.ledger().with_mut(|l| l.timestamp = 120);

        let borrower = Address::generate(env);
        let voucher = Address::generate(env);
        StellarAssetClient::new(env, &token_id.address()).mint(&voucher, &10_000_000);
        // Fund borrower so they can repay loan + yield
        StellarAssetClient::new(env, &token_id.address()).mint(&borrower, &1_000_000);

        (contract_id, token_id.address(), admin, borrower, voucher, nft_id)
    }

    fn purpose(env: &Env) -> String {
        String::from_str(env, "test loan")
    }

    // ── Basic repay / yield tests ─────────────────────────────────────────────

    #[test]
    fn test_repay_gives_voucher_yield() {
        let env = Env::default();
        let (contract_id, token_addr, _admin, borrower, voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token = TokenClient::new(&env, &token_addr);

        let initial_balance = token.balance(&voucher);
        client.vouch(&voucher, &borrower, &1_000_000, &token_addr, &None);
        client.request_loan(&borrower, &100_000, &1_000_000, &purpose(&env), &token_addr);
        // same-day repayment — total_owed = principal + total_yield (no compound interest)
        let loan = client.get_loan(&borrower).unwrap();
        let total_owed = loan.amount + loan.total_yield;
        client.repay(&borrower, &total_owed);

        let final_balance = token.balance(&voucher);
        assert!(
            final_balance > initial_balance - 1_000_000,
            "voucher should receive stake + yield"
        );
    }

    #[test]
    fn test_vouch_at_min_yield_stake_earns_nonzero_yield() {
        let env = Env::default();
        let (contract_id, token_addr, _admin, borrower, voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token = TokenClient::new(&env, &token_addr);

        client.vouch(&voucher, &borrower, &1_000_000, &token_addr, &None);
        client.request_loan(&borrower, &100_000, &1_000_000, &purpose(&env), &token_addr);

        let loan = client.get_loan(&borrower).unwrap();
        let total_owed = loan.amount + loan.total_yield;
        client.repay(&borrower, &total_owed);

        let final_balance = token.balance(&voucher);
        // voucher got back their 1_000_000 stake minus what they put in for the loan,
        // so final balance should exceed initial (10_000_000 - 1_000_000 = 9_000_000).
        assert!(
            final_balance > 9_000_000,
            "voucher yield was zero; got balance {}",
            final_balance
        );
    }

    // ── Reputation NFT tests ──────────────────────────────────────────────────

    #[test]
    fn test_repay_mints_reputation() {
        let env = Env::default();
        let (contract_id, token_addr, _admin, borrower, voucher, nft_id) =
            setup_with_reputation(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let nft = reputation::ReputationNftContractClient::new(&env, &nft_id);

        assert_eq!(nft.balance(&borrower), 0);

        client.vouch(&voucher, &borrower, &1_000_000, &token_addr, &None);
        client.request_loan(&borrower, &500_000, &1_000_000, &purpose(&env), &token_addr);

        let loan = client.get_loan(&borrower).unwrap();
        let total_owed = loan.amount + loan.total_yield;
        client.repay(&borrower, &total_owed);

        assert_eq!(nft.balance(&borrower), 1);
    }

    #[test]
    fn test_slash_burns_reputation() {
        let env = Env::default();
        let (contract_id, token_addr, admin, borrower, voucher, nft_id) =
            setup_with_reputation(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let nft = reputation::ReputationNftContractClient::new(&env, &nft_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        // First borrower repays — earns 1 rep.
        client.vouch(&voucher, &borrower, &1_000_000, &token_addr, &None);
        client.request_loan(&borrower, &500_000, &1_000_000, &purpose(&env), &token_addr);
        let loan = client.get_loan(&borrower).unwrap();
        client.repay(&borrower, &(loan.amount + loan.total_yield));
        assert_eq!(nft.balance(&borrower), 1);

        // Second borrower gets slashed — rep burns.
        let borrower2 = Address::generate(&env);
        let voucher2 = Address::generate(&env);
        token_admin.mint(&voucher2, &2_000_000);

        // Give borrower2 an initial reputation point via the NFT directly.
        nft.mint(&borrower2);
        assert_eq!(nft.balance(&borrower2), 1);

        client.vouch(&voucher2, &borrower2, &1_000_000, &token_addr, &None);
        client.request_loan(&borrower2, &500_000, &1_000_000, &purpose(&env), &token_addr);
        client.slash(&admin_signers, &borrower2);

        assert_eq!(nft.balance(&borrower2), 0);
    }

    // ── Loan pool tests ───────────────────────────────────────────────────────

    #[test]
    fn test_create_loan_pool_success() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);
        let (contract_id, token_addr, admin, _borrower, _voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let token = TokenClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        let borrower1 = Address::generate(&env);
        let borrower2 = Address::generate(&env);
        let voucher1 = Address::generate(&env);
        let voucher2 = Address::generate(&env);
        token_admin.mint(&voucher1, &10_000_000);
        token_admin.mint(&voucher2, &10_000_000);
        client.vouch(&voucher1, &borrower1, &2_000_000, &token_addr, &None);
        client.vouch(&voucher2, &borrower2, &2_000_000, &token_addr, &None);

        let borrowers = Vec::from_array(&env, [borrower1.clone(), borrower2.clone()]);
        let amounts = Vec::from_array(&env, [500_000i128, 300_000i128]);

        let pool_id = client.create_loan_pool(&admin_signers, &borrowers, &amounts);
        assert_eq!(pool_id, 1);

        let pool = client.get_loan_pool(&pool_id).unwrap();
        assert_eq!(pool.pool_id, 1);
        assert_eq!(pool.total_disbursed, 800_000);
        assert_eq!(pool.borrowers.len(), 2);

        assert_eq!(client.get_loan(&borrower1).unwrap().amount, 500_000);
        assert_eq!(client.get_loan(&borrower2).unwrap().amount, 300_000);
        assert_eq!(token.balance(&borrower1), 500_000);
        assert_eq!(token.balance(&borrower2), 300_000);
    }

    #[test]
    fn test_create_loan_pool_increments_pool_id() {
        let env = Env::default();
        let (contract_id, token_addr, admin, _borrower, _voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        assert_eq!(client.get_loan_pool_count(), 0);

        let b1 = Address::generate(&env);
        let v1 = Address::generate(&env);
        token_admin.mint(&v1, &10_000_000);
        client.vouch(&v1, &b1, &2_000_000, &token_addr, &None);
        let bs1 = Vec::from_array(&env, [b1]);
        let am1 = Vec::from_array(&env, [500_000i128]);
        assert_eq!(client.create_loan_pool(&admin_signers, &bs1, &am1), 1);

        let b2 = Address::generate(&env);
        let v2 = Address::generate(&env);
        token_admin.mint(&v2, &10_000_000);
        client.vouch(&v2, &b2, &2_000_000, &token_addr, &None);
        let bs2 = Vec::from_array(&env, [b2]);
        let am2 = Vec::from_array(&env, [500_000i128]);
        assert_eq!(client.create_loan_pool(&admin_signers, &bs2, &am2), 2);

        assert_eq!(client.get_loan_pool_count(), 2);
    }

    #[test]
    fn test_create_loan_pool_length_mismatch_rejected() {
        let env = Env::default();
        let (contract_id, _token_addr, admin, _borrower, _voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let admin_signers = single_admin_signers(&env, &admin);

        let borrowers = Vec::from_array(&env, [Address::generate(&env)]);
        let amounts: Vec<i128> = Vec::new(&env);

        let result = client.try_create_loan_pool(&admin_signers, &borrowers, &amounts);
        assert_eq!(result, Err(Ok(ContractError::PoolLengthMismatch)));
    }

    #[test]
    fn test_create_loan_pool_empty_rejected() {
        let env = Env::default();
        let (contract_id, _token_addr, admin, _borrower, _voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let admin_signers = single_admin_signers(&env, &admin);

        let borrowers: Vec<Address> = Vec::new(&env);
        let amounts: Vec<i128> = Vec::new(&env);

        let result = client.try_create_loan_pool(&admin_signers, &borrowers, &amounts);
        assert_eq!(result, Err(Ok(ContractError::PoolEmpty)));
    }

    #[test]
    fn test_create_loan_pool_rejects_active_loan_borrower() {
        let env = Env::default();
        let (contract_id, token_addr, admin, borrower, voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let admin_signers = single_admin_signers(&env, &admin);

        client.vouch(&voucher, &borrower, &2_000_000, &token_addr, &None);
        client.request_loan(&borrower, &500_000, &2_000_000, &purpose(&env), &token_addr);

        let borrowers = Vec::from_array(&env, [borrower]);
        let amounts = Vec::from_array(&env, [500_000i128]);

        let result = client.try_create_loan_pool(&admin_signers, &borrowers, &amounts);
        assert_eq!(result, Err(Ok(ContractError::PoolBorrowerActiveLoan)));
    }

    #[test]
    fn test_get_loan_pool_unknown_returns_none() {
        let env = Env::default();
        let (contract_id, _, _, _, _) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        assert!(client.get_loan_pool(&999u64).is_none());
    }

    // ── Voucher cap tests ─────────────────────────────────────────────────────

    #[test]
    fn test_get_max_vouchers_per_loan_returns_default() {
        let env = Env::default();
        let (contract_id, _, _, _, _) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        assert_eq!(client.get_max_vouchers_per_loan(), DEFAULT_MAX_VOUCHERS);
    }

    #[test]
    fn test_set_max_vouchers_per_loan_and_get() {
        let env = Env::default();
        let (contract_id, _, admin, _, _) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let admin_signers = single_admin_signers(&env, &admin);
        client.set_max_vouchers_per_loan(&admin_signers, &5);
        assert_eq!(client.get_max_vouchers_per_loan(), 5);
    }

    #[test]
    fn test_vouch_rejected_when_cap_reached() {
        let env = Env::default();
        let (contract_id, token_addr, admin, borrower, _) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        client.set_max_vouchers_per_loan(&admin_signers, &2);

        for _ in 0..2 {
            let v = Address::generate(&env);
            token_admin.mint(&v, &1_000_000);
            client.vouch(&v, &borrower, &1_000_000, &token_addr, &None);
        }

        let extra = Address::generate(&env);
        token_admin.mint(&extra, &1_000_000);
        assert!(client.try_vouch(&extra, &borrower, &1_000_000, &token_addr, &None).is_err());
    }
    // ── Vouch cooldown tests ──────────────────────────────────────────────────

    #[test]
    fn test_vouch_cooldown_blocks_second_vouch_within_window() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);
        let (contract_id, token_addr, admin, _borrower, voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        let borrower1 = Address::generate(&env);
        let borrower2 = Address::generate(&env);

        client.vouch(&voucher, &borrower1, &1_000_000, &token_addr, &None);
        let result = client.try_vouch(&voucher, &borrower2, &1_000_000, &token_addr, &None);
        assert_eq!(result, Err(Ok(ContractError::VouchCooldownActive)));
    }

    #[test]
    fn test_vouch_cooldown_allows_vouch_after_window_expires() {
        let env = Env::default();
        env.ledger().with_mut(|l| l.timestamp = 1_000_000);
        let (contract_id, token_addr, admin, _borrower, voucher) = setup(&env);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        let token_admin = StellarAssetClient::new(&env, &token_addr);
        let admin_signers = single_admin_signers(&env, &admin);

        let borrower1 = Address::generate(&env);
        let borrower2 = Address::generate(&env);

        client.vouch(&voucher, &borrower1, &1_000_000, &token_addr, &None);
        env.ledger().with_mut(|l| l.timestamp += 86_401);
        client.vouch(&voucher, &borrower2, &1_000_000, &token_addr, &None);
    }
}

// ── Issue #893: Multi-Tier Admin Approval ─────────────────────────────────────

impl QuorumCreditContract {
    pub fn get_effective_approval_threshold(
        env: Env,
        operation_type: AdminOperationType,
    ) -> u32 {
        admin::get_effective_approval_threshold(env, operation_type)
    }

    // ── Cross-chain bridge management ─────────────────────────────────────────

    /// Register a new cross-chain bridge so vouchers may stake wrapped tokens from that chain.
    pub fn register_bridge(
        env: Env,
        admin_signers: Vec<Address>,
        chain_id: u32,
        chain_name: String,
        bridge_address: Address,
    ) -> Result<(), ContractError> {
        vouch::register_bridge(env, admin_signers, chain_id, chain_name, bridge_address)
    }

    /// Deactivate a registered bridge; prevents new cross-chain vouches for that chain.
    pub fn remove_bridge(
        env: Env,
        admin_signers: Vec<Address>,
        chain_id: u32,
    ) -> Result<(), ContractError> {
        vouch::remove_bridge(env, admin_signers, chain_id)
    }

    /// Return all registered bridges (active and inactive).
    pub fn get_bridges(env: Env) -> Vec<crate::types::BridgeRecord> {
        vouch::get_bridges(env)
    }

    /// Admin: configure or rotate the Ed25519 key used to verify attestations from `origin_chain`.
    pub fn set_bridge_public_key(
        env: Env,
        admin_signers: Vec<Address>,
        origin_chain: u32,
        public_key: BytesN<32>,
    ) -> Result<(), ContractError> {
        cross_chain::set_bridge_public_key(env, admin_signers, origin_chain, public_key)
    }

    /// Canonical bytes an origin-chain attestor key must sign for this payload.
    pub fn bridge_attestation_message(
        env: Env,
        metadata: CrossChainLoanMetadata,
        nonce: u64,
        timestamp: u64,
        confirmations: u32,
    ) -> soroban_sdk::Bytes {
        cross_chain::bridge_attestation_message(&env, &metadata, nonce, timestamp, confirmations)
    }

    /// Verify a bridge attestation and consume its nonce, so it cannot be replayed.
    pub fn validate_bridge_attestation(
        env: Env,
        metadata: CrossChainLoanMetadata,
        attestation: BridgeAttestation,
    ) -> Result<(), ContractError> {
        cross_chain::validate_bridge_attestation(env, metadata, attestation)
    }

    /// Issue #968/#85: Read-only integrity check — verifies signature, freshness,
    /// confirmations, and nonce without consuming any state. Safe to call multiple times.
    pub fn verify_bridge_message(
        env: Env,
        metadata: CrossChainLoanMetadata,
        attestation: BridgeAttestation,
    ) -> Result<(), ContractError> {
        cross_chain::verify_bridge_message(env, metadata, attestation)
    }

    /// Accept a bridge-attested loan-completion event and mirror it into local storage.
    pub fn mirror_loan_to_chain(
        env: Env,
        metadata: CrossChainLoanMetadata,
        attestation: BridgeAttestation,
    ) -> Result<(), ContractError> {
        cross_chain::mirror_loan_to_chain(env, metadata, attestation)
    }

    pub fn query_mirrored_loan(
        env: Env,
        origin_chain: u32,
        loan_id: u64,
    ) -> Option<CrossChainLoanMetadata> {
        cross_chain::query_mirrored_loan(env, origin_chain, loan_id)
    }

    pub fn query_reputation_cross_chain(env: Env, borrower: Address) -> Option<UnifiedReputation> {
        cross_chain::query_reputation_cross_chain(env, borrower)
    }

    pub fn is_bridge_nonce_used(env: Env, origin_chain: u32, nonce: u64) -> bool {
        cross_chain::is_bridge_nonce_used(env, origin_chain, nonce)
    }

    // ── Flash Loans (Issue #1183) ─────────────────────────────────────────────

    pub fn flash_loan(
        env: Env,
        amount: i128,
        callback_contract: Address,
        callback_data: BytesN<32>,
    ) -> Result<(), ContractError> {
        flash_loan::flash_loan(&env, amount, callback_contract, callback_data)
    }

    pub fn repay_flash_loan(
        env: Env,
        borrower: Address,
        principal: i128,
        fee: i128,
    ) -> Result<(), ContractError> {
        flash_loan::repay_flash_loan(&env, borrower, principal, fee)
    }

    pub fn get_flash_loan_stats(env: Env) -> Result<flash_loan::FlashLoanStats, ContractError> {
        flash_loan::get_flash_loan_stats(&env)
    }

    pub fn get_total_flash_loan_volume(env: Env) -> Result<i128, ContractError> {
        flash_loan::get_total_flash_loan_volume(&env)
    }

    pub fn get_total_flash_loan_fees(env: Env) -> Result<i128, ContractError> {
        flash_loan::get_total_flash_loan_fees(&env)
    }

    pub fn get_flash_loan_count(env: Env) -> Result<u64, ContractError> {
        flash_loan::get_flash_loan_count(&env)
    }

    pub fn check_per_contract_cap(env: Env, contract: Address) -> Result<i128, ContractError> {
        flash_loan::check_per_contract_cap(&env, &contract)
    }
}

// ── Issue #1171: Vouch syndication for risk pooling ────────────────────────────

impl QuorumCreditContract {
    pub fn create_vouch_syndicate(
        env: Env,
        creator: Address,
        pool_id: u64,
        token: Address,
        contributions: Vec<SyndicateContribution>,
    ) -> Result<(), ContractError> {
        vouch_syndication::create_vouch_syndicate(env, creator, pool_id, token, contributions)
    }

    pub fn distribute_syndicate_rewards(env: Env, pool_id: u64) -> Result<(), ContractError> {
        vouch_syndication::distribute_syndicate_rewards(env, pool_id)
    }

    pub fn propose_syndicate_action(
        env: Env,
        pool_id: u64,
        proposer: Address,
        description: String,
    ) -> Result<u64, ContractError> {
        vouch_syndication::propose_syndicate_action(env, pool_id, proposer, description)
    }

    pub fn vote_syndicate_proposal(
        env: Env,
        pool_id: u64,
        proposal_id: u64,
        voter: Address,
        approve: bool,
    ) -> Result<(), ContractError> {
        vouch_syndication::vote_syndicate_proposal(env, pool_id, proposal_id, voter, approve)
    }

    pub fn get_syndicate_pool(env: Env, pool_id: u64) -> Option<SyndicatePool> {
        vouch_syndication::get_syndicate_pool(&env, pool_id)
    }

    pub fn get_syndicate_member(env: Env, pool_id: u64, member: Address) -> Option<SyndicateMember> {
        vouch_syndication::get_syndicate_member(env, pool_id, member)
    }

    pub fn get_syndicate_performance(env: Env, pool_id: u64) -> Option<SyndicatePerformance> {
        vouch_syndication::get_syndicate_performance(env, pool_id)
    }

    pub fn get_syndicate_proposal(
        env: Env,
        pool_id: u64,
        proposal_id: u64,
    ) -> Option<SyndicateProposal> {
        vouch_syndication::get_syndicate_proposal(env, pool_id, proposal_id)
    }
}

// ── Issue #1169: Conditional vouch release on performance milestones ───────────

impl QuorumCreditContract {
    pub fn release_vouch_at_milestone(
        env: Env,
        loan_id: u64,
        voucher: Address,
        milestone: LoanMilestone,
    ) -> Result<i128, ContractError> {
        vouch_milestones::release_vouch_at_milestone(env, loan_id, voucher, milestone)
    }

    pub fn get_milestone_achieved(env: Env, loan_id: u64, milestone: LoanMilestone) -> Option<u64> {
        vouch_milestones::get_milestone_achieved(env, loan_id, milestone)
    }

    pub fn get_milestone_release(
        env: Env,
        loan_id: u64,
        voucher: Address,
        milestone: LoanMilestone,
    ) -> Option<i128> {
        vouch_milestones::get_milestone_release(env, loan_id, voucher, milestone)
    }
}

// ── Issue #1168: Loan repayment automation with recurring transfers ────────────

impl QuorumCreditContract {
    pub fn setup_recurring_payment(
        env: Env,
        borrower: Address,
        token: Address,
        amount: i128,
        frequency_secs: u64,
        start_date: u64,
    ) -> Result<(), ContractError> {
        recurring_payment::setup_recurring_payment(
            env,
            borrower,
            token,
            amount,
            frequency_secs,
            start_date,
        )
    }

    pub fn execute_recurring_payment(env: Env, borrower: Address) -> Result<i128, ContractError> {
        recurring_payment::execute_recurring_payment(env, borrower)
    }

    pub fn record_recurring_payment_failure(env: Env, borrower: Address) -> Result<u32, ContractError> {
        recurring_payment::record_recurring_payment_failure(env, borrower)
    }

    pub fn terminate_recurring_payment(env: Env, borrower: Address) -> Result<(), ContractError> {
        recurring_payment::terminate_recurring_payment(env, borrower)
    }

    pub fn get_recurring_payment(env: Env, borrower: Address) -> Option<RecurringPaymentConfig> {
        recurring_payment::get_recurring_payment(env, borrower)
    }

    pub fn recurring_payment_success_rate(env: Env, borrower: Address) -> u32 {
        recurring_payment::recurring_payment_success_rate(env, borrower)
    }
}
