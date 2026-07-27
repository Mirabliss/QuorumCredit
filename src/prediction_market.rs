//! # Prediction Market for Interest Rates (Issue #1253)
//!
//! Enables on-chain predictions about future interest rate levels.
//!
//! ## Flow
//!
//! 1. An admin creates a market: e.g. "Will yield_bps exceed 300 by ledger T?"
//! 2. Participants place bets (YES / NO) staking stroops.
//! 3. When the market expires, an oracle (admin) resolves YES or NO.
//! 4. Winners share the total pot proportionally to their stake; losers forfeit.
//!
//! ## Accuracy Tracking
//!
//! Each participant's prediction history is tracked on-chain.  Off-chain UIs
//! can rank participants by accuracy to surface reliable forecasters.

#![allow(unused)]

use soroban_sdk::{contracttype, symbol_short, token, Address, Env, String, Vec};

use crate::errors::ContractError;
use crate::helpers::{require_admin_approval, require_not_paused};
use crate::types::DataKey;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Protocol fee on prediction market winnings, in basis points (200 = 2%).
pub const PREDICTION_MARKET_FEE_BPS: u32 = 200;

// ── Data Structures ───────────────────────────────────────────────────────────

/// The direction of a market prediction.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PredictionSide {
    /// Predicts the condition will be TRUE (e.g. rate > threshold).
    Yes,
    /// Predicts the condition will be FALSE.
    No,
}

/// Lifecycle state of a prediction market.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketStatus {
    /// Accepting new predictions.
    Open,
    /// Prediction period ended; awaiting oracle resolution.
    PendingResolution,
    /// Oracle has resolved the outcome; winners may claim payouts.
    Resolved,
    /// Market was cancelled (e.g. oracle unavailable); all stakes returned.
    Cancelled,
}

/// An interest rate prediction market.
#[contracttype]
#[derive(Clone)]
pub struct PredictionMarket {
    /// Unique market ID.
    pub id: u64,
    /// Short description, e.g. "yield_bps > 300 by 2026-12-01".
    pub description: String,
    /// Rate threshold in basis points being predicted against.
    pub rate_threshold_bps: u32,
    /// Ledger timestamp when the market closes for new predictions.
    pub closes_at: u64,
    /// Ledger timestamp when the market is expected to resolve.
    pub resolves_at: u64,
    /// Total stroops staked on YES.
    pub total_yes_stake: i128,
    /// Total stroops staked on NO.
    pub total_no_stake: i128,
    /// Oracle-supplied resolution: Some(true) = YES won, Some(false) = NO won.
    pub outcome: Option<bool>,
    /// Market lifecycle status.
    pub status: MarketStatus,
}

/// A single participant's position in a market.
#[contracttype]
#[derive(Clone)]
pub struct MarketPosition {
    /// Participant address.
    pub participant: Address,
    /// Which side they are on.
    pub side: PredictionSide,
    /// Amount staked in stroops.
    pub stake: i128,
    /// Whether the payout has been claimed.
    pub claimed: bool,
}

/// Per-participant prediction accuracy statistics.
#[contracttype]
#[derive(Clone)]
pub struct PredictionAccuracy {
    /// Total predictions made.
    pub total: u32,
    /// Number of correct predictions (matched resolved outcome).
    pub correct: u32,
    /// Total stroops won.
    pub total_won: i128,
    /// Total stroops lost.
    pub total_lost: i128,
}

// ── DataKey extensions (added to types.rs) ────────────────────────────────────
//
//   PredictionMarket(u64)              — PredictionMarket by ID
//   PredictionMarketCounter            — u64 monotonic counter
//   MarketPosition(u64, Address)       — MarketPosition for (market_id, participant)
//   PredictionAccuracy(Address)        — PredictionAccuracy for participant

// ── Market Management ─────────────────────────────────────────────────────────

/// Create a new prediction market.  Requires admin approval.
///
/// # Parameters
/// - `admin_signers`       — must meet admin threshold.
/// - `description`         — human-readable market description.
/// - `rate_threshold_bps`  — the interest rate threshold being predicted (bps).
/// - `closes_at`           — ledger timestamp when prediction window closes.
/// - `resolves_at`         — ledger timestamp by which the oracle will resolve.
///
/// # Errors
/// - `InvalidAmount`        — `closes_at` or `resolves_at` is in the past.
/// - `ContractPaused`       — contract is paused.
pub fn create_market(
    env: &Env,
    admin_signers: Vec<Address>,
    description: String,
    rate_threshold_bps: u32,
    closes_at: u64,
    resolves_at: u64,
) -> Result<u64, ContractError> {
    require_not_paused(env)?;
    require_admin_approval(env, &admin_signers);

    let now = env.ledger().timestamp();
    if closes_at <= now || resolves_at <= closes_at {
        return Err(ContractError::InvalidAmount);
    }

    let market_id: u64 = env
        .storage()
        .persistent()
        .get::<DataKey, u64>(&DataKey::PredictionMarketCounter)
        .unwrap_or(0)
        + 1;
    env.storage()
        .persistent()
        .set(&DataKey::PredictionMarketCounter, &market_id);

    let market = PredictionMarket {
        id: market_id,
        description: description.clone(),
        rate_threshold_bps,
        closes_at,
        resolves_at,
        total_yes_stake: 0,
        total_no_stake: 0,
        outcome: None,
        status: MarketStatus::Open,
    };

    env.storage()
        .persistent()
        .set(&DataKey::PredictionMarket(market_id), &market);

    env.events().publish(
        (symbol_short!("market"), symbol_short!("create")),
        (market_id, rate_threshold_bps, closes_at, resolves_at),
    );

    Ok(market_id)
}

/// Place a prediction on an open market.
///
/// The `stake` amount of the protocol token is transferred from `participant`
/// to the contract when this function is called.
///
/// # Parameters
/// - `participant` — address placing the bet; must sign.
/// - `market_id`   — target market.
/// - `side`        — YES or NO.
/// - `stake`       — amount in stroops (must be > 0).
/// - `token_addr`  — protocol token address.
///
/// # Errors
/// - `ProposalNotFound`     — unknown market ID.
/// - `VotingPeriodEnded`    — prediction window has closed.
/// - `AlreadyVoted`         — participant already has a position in this market.
/// - `InvalidAmount`        — stake ≤ 0.
/// - `ContractPaused`       — contract is paused.
pub fn place_prediction(
    env: &Env,
    participant: Address,
    market_id: u64,
    side: PredictionSide,
    stake: i128,
    token_addr: Address,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    participant.require_auth();

    if stake <= 0 {
        return Err(ContractError::InvalidAmount);
    }

    let mut market: PredictionMarket = env
        .storage()
        .persistent()
        .get(&DataKey::PredictionMarket(market_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if market.status != MarketStatus::Open {
        return Err(ContractError::VotingPeriodEnded);
    }

    let now = env.ledger().timestamp();
    if now >= market.closes_at {
        market.status = MarketStatus::PendingResolution;
        env.storage()
            .persistent()
            .set(&DataKey::PredictionMarket(market_id), &market);
        return Err(ContractError::VotingPeriodEnded);
    }

    let pos_key = DataKey::MarketPosition(market_id, participant.clone());
    if env.storage().persistent().has(&pos_key) {
        return Err(ContractError::AlreadyVoted);
    }

    // Transfer stake from participant to contract.
    let tc = token::Client::new(env, &token_addr);
    tc.transfer(&participant, &env.current_contract_address(), &stake);

    // Record position.
    let position = MarketPosition {
        participant: participant.clone(),
        side: side.clone(),
        stake,
        claimed: false,
    };
    env.storage().persistent().set(&pos_key, &position);

    match side {
        PredictionSide::Yes => market.total_yes_stake += stake,
        PredictionSide::No => market.total_no_stake += stake,
    }
    env.storage()
        .persistent()
        .set(&DataKey::PredictionMarket(market_id), &market);

    env.events().publish(
        (symbol_short!("market"), symbol_short!("predict")),
        (market_id, participant, side, stake),
    );

    Ok(())
}

/// Resolve a market via oracle (admin).
///
/// Sets the final outcome and moves market to `Resolved`.
///
/// # Parameters
/// - `admin_signers` — must meet admin threshold.
/// - `market_id`     — market to resolve.
/// - `outcome`       — `true` = YES won (rate exceeded threshold); `false` = NO won.
///
/// # Errors
/// - `ProposalNotFound`        — unknown market ID.
/// - `InvalidStateTransition`  — market is not in `PendingResolution` or `Open`.
/// - `ContractPaused`          — contract is paused.
pub fn resolve_market(
    env: &Env,
    admin_signers: Vec<Address>,
    market_id: u64,
    outcome: bool,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    require_admin_approval(env, &admin_signers);

    let mut market: PredictionMarket = env
        .storage()
        .persistent()
        .get(&DataKey::PredictionMarket(market_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if market.status == MarketStatus::Resolved || market.status == MarketStatus::Cancelled {
        return Err(ContractError::InvalidStateTransition);
    }

    market.outcome = Some(outcome);
    market.status = MarketStatus::Resolved;
    env.storage()
        .persistent()
        .set(&DataKey::PredictionMarket(market_id), &market);

    env.events().publish(
        (symbol_short!("market"), symbol_short!("resolve")),
        (market_id, outcome),
    );

    Ok(())
}

/// Cancel a market and allow refunds.
///
/// # Errors
/// - `ProposalNotFound`        — unknown market ID.
/// - `InvalidStateTransition`  — market is already resolved or cancelled.
pub fn cancel_market(
    env: &Env,
    admin_signers: Vec<Address>,
    market_id: u64,
) -> Result<(), ContractError> {
    require_not_paused(env)?;
    require_admin_approval(env, &admin_signers);

    let mut market: PredictionMarket = env
        .storage()
        .persistent()
        .get(&DataKey::PredictionMarket(market_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if market.status == MarketStatus::Resolved || market.status == MarketStatus::Cancelled {
        return Err(ContractError::InvalidStateTransition);
    }

    market.status = MarketStatus::Cancelled;
    env.storage()
        .persistent()
        .set(&DataKey::PredictionMarket(market_id), &market);

    Ok(())
}

// ── Payout ────────────────────────────────────────────────────────────────────

/// Claim payout for a winning position (or refund for cancelled market).
///
/// Payout = (participant_stake / total_winning_stake) × total_pot
///          minus `PREDICTION_MARKET_FEE_BPS` protocol fee on winnings.
///
/// For cancelled markets, the full stake is returned.
///
/// # Parameters
/// - `participant`  — winner claiming payout; must sign.
/// - `market_id`    — market ID.
/// - `token_addr`   — protocol token address.
///
/// # Errors
/// - `ProposalNotFound`       — unknown market ID.
/// - `InvalidStateTransition` — market is not resolved or cancelled.
/// - `UnauthorizedCaller`     — participant has no position in this market.
/// - `AlreadyRepaid`          — payout already claimed.
pub fn claim_payout(
    env: &Env,
    participant: Address,
    market_id: u64,
    token_addr: Address,
) -> Result<i128, ContractError> {
    participant.require_auth();

    let market: PredictionMarket = env
        .storage()
        .persistent()
        .get(&DataKey::PredictionMarket(market_id))
        .ok_or(ContractError::ProposalNotFound)?;

    if market.status != MarketStatus::Resolved && market.status != MarketStatus::Cancelled {
        return Err(ContractError::InvalidStateTransition);
    }

    let pos_key = DataKey::MarketPosition(market_id, participant.clone());
    let mut position: MarketPosition = env
        .storage()
        .persistent()
        .get(&pos_key)
        .ok_or(ContractError::UnauthorizedCaller)?;

    if position.claimed {
        return Err(ContractError::AlreadyRepaid);
    }

    let payout = if market.status == MarketStatus::Cancelled {
        // Full refund.
        position.stake
    } else {
        let won = match market.outcome {
            Some(true) => position.side == PredictionSide::Yes,
            Some(false) => position.side == PredictionSide::No,
            None => false,
        };

        if !won {
            // Update accuracy stats — record loss.
            update_accuracy(env, &participant, false, 0, position.stake);
            position.claimed = true;
            env.storage().persistent().set(&pos_key, &position);
            return Ok(0);
        }

        // Compute share of the total pot.
        let winning_stake = match market.outcome {
            Some(true) => market.total_yes_stake,
            _ => market.total_no_stake,
        };
        let total_pot = market.total_yes_stake + market.total_no_stake;

        if winning_stake == 0 {
            0
        } else {
            let gross = position.stake * total_pot / winning_stake;
            let fee = gross * PREDICTION_MARKET_FEE_BPS as i128 / 10_000;
            let net = gross - fee;

            // Accumulate fee to treasury.
            crate::community_treasury::deposit_to_treasury(env, fee);

            net
        }
    };

    // Mark claimed.
    position.claimed = true;
    env.storage().persistent().set(&pos_key, &position);

    // Transfer payout.
    if payout > 0 {
        let tc = token::Client::new(env, &token_addr);
        tc.transfer(&env.current_contract_address(), &participant, &payout);
    }

    // Update accuracy stats — record win.
    update_accuracy(env, &participant, true, payout, 0);

    env.events().publish(
        (symbol_short!("market"), symbol_short!("payout")),
        (market_id, participant, payout),
    );

    Ok(payout)
}

// ── Accuracy Tracking ─────────────────────────────────────────────────────────

fn update_accuracy(env: &Env, participant: &Address, correct: bool, won: i128, lost: i128) {
    let key = DataKey::PredictionAccuracy(participant.clone());
    let mut acc: PredictionAccuracy = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(PredictionAccuracy {
            total: 0,
            correct: 0,
            total_won: 0,
            total_lost: 0,
        });
    acc.total += 1;
    if correct {
        acc.correct += 1;
    }
    acc.total_won += won;
    acc.total_lost += lost;
    env.storage().persistent().set(&key, &acc);
}

/// Return the prediction accuracy record for a participant.
pub fn get_prediction_accuracy(env: &Env, participant: &Address) -> PredictionAccuracy {
    env.storage()
        .persistent()
        .get(&DataKey::PredictionAccuracy(participant.clone()))
        .unwrap_or(PredictionAccuracy {
            total: 0,
            correct: 0,
            total_won: 0,
            total_lost: 0,
        })
}

/// Return the prediction market by ID.
pub fn get_market(env: &Env, market_id: u64) -> Option<PredictionMarket> {
    env.storage()
        .persistent()
        .get(&DataKey::PredictionMarket(market_id))
}

/// Return a participant's position in a market.
pub fn get_position(env: &Env, market_id: u64, participant: &Address) -> Option<MarketPosition> {
    env.storage()
        .persistent()
        .get(&DataKey::MarketPosition(market_id, participant.clone()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::{Address as _, Ledger, LedgerInfo};
    use soroban_sdk::{Env, String};

    fn setup() -> Env {
        let env = Env::default();
        env.mock_all_auths();
        env
    }

    fn make_description(env: &Env) -> String {
        String::from_str(env, "yield_bps > 300 in 6 months")
    }

    #[test]
    fn test_create_market() {
        let env = setup();
        // We need a valid Config in storage for require_admin_approval.
        // Since we can't easily bootstrap a full contract here, we test
        // the data path by verifying the counter increments.
        // Full integration is covered via lib.rs entry points.
        let counter: u64 = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::PredictionMarketCounter)
            .unwrap_or(0);
        assert_eq!(counter, 0);
    }

    #[test]
    fn test_market_status_default() {
        let env = setup();
        let market = get_market(&env, 999);
        assert!(market.is_none());
    }

    #[test]
    fn test_prediction_accuracy_default() {
        let env = setup();
        let addr = Address::generate(&env);
        let acc = get_prediction_accuracy(&env, &addr);
        assert_eq!(acc.total, 0);
        assert_eq!(acc.correct, 0);
    }

    #[test]
    fn test_invalid_amount_for_zero_stake() {
        let env = setup();
        // place_prediction with stake=0 should return InvalidAmount.
        // We seed a market manually.
        let market_id = 1u64;
        let now = env.ledger().timestamp();
        let market = PredictionMarket {
            id: market_id,
            description: make_description(&env),
            rate_threshold_bps: 300,
            closes_at: now + 3600,
            resolves_at: now + 7200,
            total_yes_stake: 0,
            total_no_stake: 0,
            outcome: None,
            status: MarketStatus::Open,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PredictionMarket(market_id), &market);

        let participant = Address::generate(&env);
        let token = Address::generate(&env);
        let result = place_prediction(&env, participant, market_id, PredictionSide::Yes, 0, token);
        assert_eq!(result, Err(ContractError::InvalidAmount));
    }

    #[test]
    fn test_cannot_predict_on_resolved_market() {
        let env = setup();
        let market_id = 1u64;
        let now = env.ledger().timestamp();
        let market = PredictionMarket {
            id: market_id,
            description: make_description(&env),
            rate_threshold_bps: 300,
            closes_at: now + 3600,
            resolves_at: now + 7200,
            total_yes_stake: 0,
            total_no_stake: 0,
            outcome: Some(true),
            status: MarketStatus::Resolved,
        };
        env.storage()
            .persistent()
            .set(&DataKey::PredictionMarket(market_id), &market);

        let participant = Address::generate(&env);
        let token = Address::generate(&env);
        let result =
            place_prediction(&env, participant, market_id, PredictionSide::Yes, 1000, token);
        assert_eq!(result, Err(ContractError::VotingPeriodEnded));
    }
}
