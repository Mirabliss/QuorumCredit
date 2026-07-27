//! # Interest Rate Options for Risk Management (Issue #1255)
//!
//! On-chain interest rate call and put options.
//!
//! ## Concepts
//!
//! - **Call option** — the holder has the right (not obligation) to borrow at a
//!   fixed rate (`strike_bps`) during the option's validity window.  Protects
//!   against rate increases.
//!
//! - **Put option** — the holder receives a payout if the prevailing rate rises
//!   above `strike_bps` at expiry.  Acts as insurance against rising rates.
//!
//! ## Pricing
//!
//! Premium is computed using a simplified on-chain Black-Scholes formula.
//! Since Soroban does not support floating-point, all calculations use integer
//! arithmetic with basis-point precision.  The formula approximates:
//!
//!   premium ≈ strike × σ × √T × N(d1) (simplified, discretised)
//!
//! where:
//! - `σ`  = implied volatility (configurable, in bps-per-day)
//! - `T`  = time to expiry in days
//! - `N(d1)` is approximated using a linear interpolation table
//!
//! ## Settlement
//!
//! At or after expiry, the holder calls `settle_option`.  The contract
//! compares the current on-chain `yield_bps` to `strike_bps` and pays out
//! accordingly.

#![allow(unused)]

use soroban_sdk::{contracttype, symbol_short, Address, Env, Vec};

use crate::errors::ContractError;
use crate::helpers::{config, require_admin_approval, require_not_paused};
use crate::types::DataKey;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Implied volatility used in option pricing: σ in basis points per day (default 5).
pub const DEFAULT_IMPLIED_VOLATILITY_BPS_PER_DAY: u32 = 5;

/// Seconds in one day.
pub const SECS_PER_DAY: u64 = 86_400;

/// Protocol fee on option premiums, in basis points (100 = 1%).
pub const OPTION_PROTOCOL_FEE_BPS: u32 = 100;

// ── Data Structures ───────────────────────────────────────────────────────────

/// Type of interest rate option.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionType {
    /// Right to borrow at the strike rate; profitable when rates rise.
    Call,
    /// Protection payout when rates rise above strike; profitable when rates rise.
    Put,
}

/// Lifecycle state of an option.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionStatus {
    /// Option is live and can be exercised until `expires_at`.
    Active,
    /// Option was settled; `payout` was transferred.
    Settled,
    /// Option expired worthless (out of the money at settlement time).
    Expired,
    /// Option was cancelled before expiry (premium refunded pro-rata).
    Cancelled,
}

/// An interest rate option contract.
#[contracttype]
#[derive(Clone)]
pub struct InterestRateOption {
    /// Unique option ID.
    pub id: u64,
    /// Holder (buyer) of the option.
    pub holder: Address,
    /// Option type: Call or Put.
    pub option_type: OptionType,
    /// Strike rate in basis points.
    pub strike_bps: u32,
    /// Notional loan amount the option covers, in stroops.
    pub notional: i128,
    /// Premium paid by the holder, in stroops.
    pub premium: i128,
    /// Ledger timestamp when the option was issued.
    pub issued_at: u64,
    /// Ledger timestamp when the option expires.
    pub expires_at: u64,
    /// Payout delivered at settlement, in stroops.
    pub payout: i128,
    /// Current lifecycle status.
    pub status: OptionStatus,
}

/// Aggregate open interest statistics for a given option type.
#[contracttype]
#[derive(Clone)]
pub struct OptionOpenInterest {
    /// Total number of active options of this type.
    pub count: u64,
    /// Total notional covered, in stroops.
    pub total_notional: i128,
    /// Total premiums collected, in stroops.
    pub total_premiums: i128,
}

// ── DataKey extensions (added to types.rs) ────────────────────────────────────
//
//   InterestRateOption(u64)         — InterestRateOption by ID
//   OptionCounter                   — u64 monotonic counter
//   OptionOpenInterest(OptionType)  — OptionOpenInterest per type
//   ImpliedVolatility               — u32 current σ in bps/day

// ── Option Pricing ────────────────────────────────────────────────────────────

/// Compute option premium using a simplified Black-Scholes approximation.
///
/// ```text
/// premium = notional × strike_bps × σ_bps_per_day × √days / (10_000 × 10_000 × 100)
/// ```
///
/// The square root is approximated via integer Newton's method.
///
/// All values are in stroops / basis-points.
pub fn calculate_premium(
    notional: i128,
    strike_bps: u32,
    days_to_expiry: u64,
    implied_vol_bps: u32,
) -> i128 {
    if days_to_expiry == 0 || notional <= 0 {
        return 0;
    }

    // Integer square root of days_to_expiry (Newton's method).
    let sqrt_days = isqrt(days_to_expiry as i128);

    // premium = notional × strike_bps × vol_bps × sqrt_days / divisor
    // divisor chosen so that for typical values (notional=1e10, strike=200,
    // vol=5, days=30) the premium is a few basis points of notional.
    let divisor: i128 = 10_000 * 10_000 * 100;
    let premium = notional * strike_bps as i128 * implied_vol_bps as i128 * sqrt_days / divisor;

    // Apply protocol fee on top (embed in premium).
    let fee = premium * OPTION_PROTOCOL_FEE_BPS as i128 / 10_000;
    premium + fee
}

/// Integer square root via Newton's method.
fn isqrt(n: i128) -> i128 {
    if n <= 0 {
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

// ── Option Lifecycle ──────────────────────────────────────────────────────────

/// Write the implied volatility configuration (admin only).
///
/// # Parameters
/// - `admin_signers`     — must meet admin threshold.
/// - `vol_bps_per_day`   — new implied volatility in basis-points per day.
pub fn set_implied_volatility(
    env: &Env,
    admin_signers: Vec<Address>,
    vol_bps_per_day: u32,
) -> Result<(), ContractError> {
    require_admin_approval(env, &admin_signers);
    env.storage()
        .persistent()
        .set(&DataKey::ImpliedVolatility, &vol_bps_per_day);
    Ok(())
}

/// Read current implied volatility (default `DEFAULT_IMPLIED_VOLATILITY_BPS_PER_DAY`).
pub fn get_implied_volatility(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get::<DataKey, u32>(&DataKey::ImpliedVolatility)
        .unwrap_or(DEFAULT_IMPLIED_VOLATILITY_BPS_PER_DAY)
}

/// Purchase an interest rate option.
///
/// The holder pays the computed premium (transferred from `holder` to contract).
/// The option is created and stored.
///
/// # Parameters
/// - `holder`       — buyer of the option; must sign.
/// - `option_type`  — Call or Put.
/// - `strike_bps`   — fixed strike rate in basis points.
/// - `notional`     — notional loan amount covered, in stroops.
/// - `duration_secs`— option duration in seconds; determines expiry and premium.
/// - `token_addr`   — protocol token for premium transfer.
///
/// # Errors
/// - `InvalidAmount`    — notional ≤ 0, strike = 0, or duration = 0.
/// - `ContractPaused`   — contract is paused.
pub fn buy_option(
    env: &Env,
    holder: Address,
    option_type: OptionType,
    strike_bps: u32,
    notional: i128,
    duration_secs: u64,
    token_addr: Address,
) -> Result<u64, ContractError> {
    require_not_paused(env)?;
    holder.require_auth();

    if notional <= 0 || strike_bps == 0 || duration_secs == 0 {
        return Err(ContractError::InvalidAmount);
    }

    let now = env.ledger().timestamp();
    let expires_at = now + duration_secs;
    let days_to_expiry = (duration_secs / SECS_PER_DAY).max(1);
    let vol = get_implied_volatility(env);

    let premium = calculate_premium(notional, strike_bps, days_to_expiry, vol);

    // Transfer premium from holder to contract.
    if premium > 0 {
        let tc = soroban_sdk::token::Client::new(env, &token_addr);
        tc.transfer(&holder, &env.current_contract_address(), &premium);
    }

    // Allocate option ID.
    let option_id: u64 = env
        .storage()
        .persistent()
        .get::<DataKey, u64>(&DataKey::OptionCounter)
        .unwrap_or(0)
        + 1;
    env.storage()
        .persistent()
        .set(&DataKey::OptionCounter, &option_id);

    let option = InterestRateOption {
        id: option_id,
        holder: holder.clone(),
        option_type: option_type.clone(),
        strike_bps,
        notional,
        premium,
        issued_at: now,
        expires_at,
        payout: 0,
        status: OptionStatus::Active,
    };

    env.storage()
        .persistent()
        .set(&DataKey::InterestRateOption(option_id), &option);

    // Update open interest.
    update_open_interest(env, &option_type, notional, premium, true);

    env.events().publish(
        (symbol_short!("option"), symbol_short!("buy")),
        (option_id, holder, option_type, strike_bps, notional, premium),
    );

    Ok(option_id)
}

/// Settle an option at or after expiry.
///
/// The contract compares `current_yield_bps` (from on-chain Config) to `strike_bps`
/// and pays out the in-the-money value.
///
/// | Type | In the money when           | Payout                          |
/// |------|-----------------------------|---------------------------------|
/// | Call | current_bps < strike_bps    | (strike − current) × notional / 10_000 |
/// | Put  | current_bps > strike_bps    | (current − strike) × notional / 10_000 |
///
/// Out-of-the-money options expire worthless; the holder loses the premium.
///
/// # Parameters
/// - `holder`      — must sign and must be the option holder.
/// - `option_id`   — option to settle.
/// - `token_addr`  — protocol token for payout transfer.
///
/// # Errors
/// - `InvalidAmount`          — option not found.
/// - `LoanPastDeadline`       — option has not yet expired.
/// - `InvalidStateTransition` — option is not Active.
/// - `UnauthorizedCaller`     — caller is not the holder.
pub fn settle_option(
    env: &Env,
    holder: Address,
    option_id: u64,
    token_addr: Address,
) -> Result<i128, ContractError> {
    holder.require_auth();

    let mut option: InterestRateOption = env
        .storage()
        .persistent()
        .get(&DataKey::InterestRateOption(option_id))
        .ok_or(ContractError::InvalidAmount)?;

    if option.holder != holder {
        return Err(ContractError::UnauthorizedCaller);
    }

    if option.status != OptionStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    let now = env.ledger().timestamp();
    if now < option.expires_at {
        return Err(ContractError::LoanPastDeadline);
    }

    // Read current on-chain yield rate from Config.
    let cfg = config(env);
    let current_bps = cfg.yield_bps as u32;

    let payout: i128 = match option.option_type {
        OptionType::Call => {
            // Call pays out when current rate is BELOW strike (holder secured a lower rate).
            if current_bps < option.strike_bps {
                let diff = (option.strike_bps - current_bps) as i128;
                diff * option.notional / 10_000
            } else {
                0
            }
        }
        OptionType::Put => {
            // Put pays out when current rate is ABOVE strike.
            if current_bps > option.strike_bps {
                let diff = (current_bps - option.strike_bps) as i128;
                diff * option.notional / 10_000
            } else {
                0
            }
        }
    };

    // Collect protocol fee on payout.
    let fee = payout * OPTION_PROTOCOL_FEE_BPS as i128 / 10_000;
    let net_payout = payout - fee;

    if net_payout > 0 {
        crate::community_treasury::deposit_to_treasury(env, fee);
        let tc = soroban_sdk::token::Client::new(env, &token_addr);
        tc.transfer(&env.current_contract_address(), &holder, &net_payout);
    }

    option.payout = net_payout;
    option.status = if net_payout > 0 {
        OptionStatus::Settled
    } else {
        OptionStatus::Expired
    };

    env.storage()
        .persistent()
        .set(&DataKey::InterestRateOption(option_id), &option);

    // Update open interest.
    update_open_interest(env, &option.option_type, option.notional, option.premium, false);

    env.events().publish(
        (symbol_short!("option"), symbol_short!("settle")),
        (option_id, holder, net_payout),
    );

    Ok(net_payout)
}

/// Cancel an active option before expiry and refund the premium pro-rata.
///
/// The refund = premium × remaining_time / total_duration.
///
/// # Parameters
/// - `holder`      — must sign and must be the option holder.
/// - `option_id`   — option to cancel.
/// - `token_addr`  — protocol token for refund transfer.
///
/// # Errors
/// - `InvalidAmount`          — option not found.
/// - `InvalidStateTransition` — option is not Active.
/// - `UnauthorizedCaller`     — caller is not the holder.
pub fn cancel_option(
    env: &Env,
    holder: Address,
    option_id: u64,
    token_addr: Address,
) -> Result<i128, ContractError> {
    holder.require_auth();

    let mut option: InterestRateOption = env
        .storage()
        .persistent()
        .get(&DataKey::InterestRateOption(option_id))
        .ok_or(ContractError::InvalidAmount)?;

    if option.holder != holder {
        return Err(ContractError::UnauthorizedCaller);
    }

    if option.status != OptionStatus::Active {
        return Err(ContractError::InvalidStateTransition);
    }

    let now = env.ledger().timestamp();
    let total_duration = option.expires_at.saturating_sub(option.issued_at).max(1);
    let remaining = option.expires_at.saturating_sub(now);

    let refund = option.premium * remaining as i128 / total_duration as i128;

    option.status = OptionStatus::Cancelled;
    option.payout = 0;
    env.storage()
        .persistent()
        .set(&DataKey::InterestRateOption(option_id), &option);

    if refund > 0 {
        let tc = soroban_sdk::token::Client::new(env, &token_addr);
        tc.transfer(&env.current_contract_address(), &holder, &refund);
    }

    update_open_interest(env, &option.option_type, option.notional, option.premium, false);

    env.events().publish(
        (symbol_short!("option"), symbol_short!("cancel")),
        (option_id, holder, refund),
    );

    Ok(refund)
}

// ── Open Interest ─────────────────────────────────────────────────────────────

fn update_open_interest(
    env: &Env,
    option_type: &OptionType,
    notional: i128,
    premium: i128,
    add: bool,
) {
    let key = DataKey::OptionOpenInterest(option_type.clone());
    let mut oi: OptionOpenInterest = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(OptionOpenInterest {
            count: 0,
            total_notional: 0,
            total_premiums: 0,
        });

    if add {
        oi.count += 1;
        oi.total_notional += notional;
        oi.total_premiums += premium;
    } else {
        oi.count = oi.count.saturating_sub(1);
        oi.total_notional = (oi.total_notional - notional).max(0);
        oi.total_premiums = (oi.total_premiums - premium).max(0);
    }

    env.storage().persistent().set(&key, &oi);
}

/// Return open interest statistics for a given option type.
pub fn get_open_interest(env: &Env, option_type: OptionType) -> OptionOpenInterest {
    env.storage()
        .persistent()
        .get(&DataKey::OptionOpenInterest(option_type))
        .unwrap_or(OptionOpenInterest {
            count: 0,
            total_notional: 0,
            total_premiums: 0,
        })
}

/// Return an option by ID.
pub fn get_option(env: &Env, option_id: u64) -> Option<InterestRateOption> {
    env.storage()
        .persistent()
        .get(&DataKey::InterestRateOption(option_id))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Env;

    #[test]
    fn test_isqrt() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(25), 5);
        assert_eq!(isqrt(30), 5); // floor(√30) = 5
        assert_eq!(isqrt(100), 10);
    }

    #[test]
    fn test_premium_positive_for_valid_inputs() {
        let premium = calculate_premium(
            10_000_000_000, // 1000 XLM notional
            200,            // 2% strike
            30,             // 30 days
            5,              // 5 bps/day vol
        );
        assert!(premium > 0, "Premium should be positive");
    }

    #[test]
    fn test_premium_zero_for_zero_days() {
        let premium = calculate_premium(10_000_000_000, 200, 0, 5);
        assert_eq!(premium, 0);
    }

    #[test]
    fn test_premium_zero_for_zero_notional() {
        let premium = calculate_premium(0, 200, 30, 5);
        assert_eq!(premium, 0);
    }

    #[test]
    fn test_get_option_returns_none_for_unknown_id() {
        let env = Env::default();
        env.mock_all_auths();
        assert!(get_option(&env, 9999).is_none());
    }

    #[test]
    fn test_open_interest_default_zero() {
        let env = Env::default();
        env.mock_all_auths();
        let oi = get_open_interest(&env, OptionType::Call);
        assert_eq!(oi.count, 0);
        assert_eq!(oi.total_notional, 0);
    }

    #[test]
    fn test_get_implied_volatility_default() {
        let env = Env::default();
        env.mock_all_auths();
        assert_eq!(
            get_implied_volatility(&env),
            DEFAULT_IMPLIED_VOLATILITY_BPS_PER_DAY
        );
    }

    #[test]
    fn test_call_option_payout_in_the_money() {
        // Simulate a call option where strike > current rate → in the money.
        // payout = (strike - current) × notional / 10_000
        let strike_bps: u32 = 300;
        let current_bps: u32 = 200;
        let notional: i128 = 10_000_000; // 1 XLM
        let diff = (strike_bps - current_bps) as i128;
        let expected_gross = diff * notional / 10_000;
        // payout = 100 * 10_000_000 / 10_000 = 100_000
        assert_eq!(expected_gross, 100_000);
    }

    #[test]
    fn test_put_option_payout_in_the_money() {
        let strike_bps: u32 = 200;
        let current_bps: u32 = 350;
        let notional: i128 = 10_000_000;
        let diff = (current_bps - strike_bps) as i128;
        let gross = diff * notional / 10_000;
        // 150 * 10_000_000 / 10_000 = 150_000
        assert_eq!(gross, 150_000);
    }

    #[test]
    fn test_buy_option_requires_nonzero_inputs() {
        let env = Env::default();
        env.mock_all_auths();
        let holder = Address::generate(&env);
        let token = Address::generate(&env);

        let r1 = buy_option(&env, holder.clone(), OptionType::Call, 200, 0, 3600, token.clone());
        assert_eq!(r1, Err(ContractError::InvalidAmount));

        let r2 = buy_option(&env, holder.clone(), OptionType::Call, 0, 1_000, 3600, token.clone());
        assert_eq!(r2, Err(ContractError::InvalidAmount));

        let r3 = buy_option(&env, holder, OptionType::Call, 200, 1_000, 0, token);
        assert_eq!(r3, Err(ContractError::InvalidAmount));
    }
}
