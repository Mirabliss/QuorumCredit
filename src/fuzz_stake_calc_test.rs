//! Fuzz / property tests for stake-related arithmetic (issue #942).
//!
//! Covers the calculations called out in `docs/security-audit-checklist.md` §2:
//! - yield:  `stake * yield_bps / BPS_DENOMINATOR`
//! - slash:  `stake * slash_bps / BPS_DENOMINATOR`
//! - summation of voucher stakes (`total_vouched`)
//!
//! Pure-math properties run without a contract env. Contract-level properties
//! vouch random stakes and assert `total_vouched` matches the sum.

use crate::types::{
    BPS_DENOMINATOR, DEFAULT_MIN_YIELD_STAKE, DEFAULT_SLASH_BPS, DEFAULT_YIELD_BPS,
};
use crate::{QuorumCreditContract, QuorumCreditContractClient};
use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env, Vec,
};

/// Checked yield: `stake * yield_bps / BPS_DENOMINATOR`.
fn calc_yield(stake: i128, yield_bps: i128) -> Option<i128> {
    stake
        .checked_mul(yield_bps)?
        .checked_div(BPS_DENOMINATOR)
}

/// Checked slash: `stake * slash_bps / BPS_DENOMINATOR`.
fn calc_slash(stake: i128, slash_bps: i128) -> Option<i128> {
    stake
        .checked_mul(slash_bps)?
        .checked_div(BPS_DENOMINATOR)
}

// ── Pure arithmetic properties ───────────────────────────────────────────────

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 256,
        max_shrink_iters: 64,
        ..Default::default()
    })]

    /// Yield never overflows for stakes up to max XLM supply scale and
    /// yield_bps in the protocol-legal range [0, 10_000]. Result is
    /// non-negative and at most `stake` (bps ≤ 100%).
    #[test]
    fn prop_yield_safe_for_valid_inputs(
        stake in 0i128..=5_000_000_000_000_000i128, // ~5e15 stroops ≈ max XLM supply
        yield_bps in 0i128..=BPS_DENOMINATOR,
    ) {
        let y = calc_yield(stake, yield_bps)
            .expect("yield must not overflow for valid stake × bps");
        prop_assert!(y >= 0);
        prop_assert!(y <= stake);
        // Matches the threat-model sketch: at default 200 bps, yield ≤ 2%.
        if yield_bps == DEFAULT_YIELD_BPS {
            prop_assert!(y <= stake * 2 / 100);
        }
    }

    /// Slash never overflows for the same stake/bps envelope. Result is
    /// non-negative, at most `stake`, and remaining stake is non-negative.
    #[test]
    fn prop_slash_safe_for_valid_inputs(
        stake in 0i128..=5_000_000_000_000_000i128,
        slash_bps in 0i128..=BPS_DENOMINATOR,
    ) {
        let slashed = calc_slash(stake, slash_bps)
            .expect("slash must not overflow for valid stake × bps");
        prop_assert!(slashed >= 0);
        prop_assert!(slashed <= stake);
        let remaining = stake.checked_sub(slashed).expect("remaining must not underflow");
        prop_assert!(remaining >= 0);
        prop_assert_eq!(remaining + slashed, stake);
        if slash_bps == DEFAULT_SLASH_BPS {
            // Default 50% truncates toward zero: slashed == stake / 2.
            prop_assert_eq!(slashed, stake / 2);
        }
    }

    /// Extreme stakes near i128::MAX must return None (ArithmeticError path)
    /// rather than wrapping when bps > 0.
    #[test]
    fn prop_extreme_stake_does_not_wrap(
        stake in (i128::MAX / 2)..=i128::MAX,
        bps in 1i128..=BPS_DENOMINATOR,
    ) {
        // stake * bps overflows i128 for large stake when bps >= 2;
        // for bps == 1 it may still succeed. Either way: never wrap.
        let y = calc_yield(stake, bps);
        let s = calc_slash(stake, bps);
        if let Some(v) = y {
            prop_assert!(v >= 0);
        }
        if let Some(v) = s {
            prop_assert!(v >= 0);
            prop_assert!(v <= stake);
        }
    }

    /// Summing a vector of non-negative stakes with checked_add either
    /// succeeds with the exact sum or reports overflow — never wraps.
    #[test]
    fn prop_stake_summation_never_wraps(
        stakes in prop::collection::vec(0i128..=1_000_000_000_000i128, 0..=32),
    ) {
        let mut total: i128 = 0;
        let mut overflowed = false;
        for s in &stakes {
            match total.checked_add(*s) {
                Some(n) => total = n,
                None => {
                    overflowed = true;
                    break;
                }
            }
        }
        if !overflowed {
            let naive: i128 = stakes.iter().copied().sum();
            prop_assert_eq!(total, naive);
            prop_assert!(total >= 0);
        }
    }

    /// Default protocol constants produce non-zero yield exactly at
    /// `DEFAULT_MIN_YIELD_STAKE` and zero below it (truncation).
    #[test]
    fn prop_min_yield_stake_boundary(stake in 0i128..=200i128) {
        let y = calc_yield(stake, DEFAULT_YIELD_BPS).unwrap();
        if stake < DEFAULT_MIN_YIELD_STAKE {
            prop_assert_eq!(y, 0, "sub-minimum stake must truncate to zero yield");
        } else {
            prop_assert!(y > 0, "at/above min yield stake must earn non-zero yield");
        }
    }
}

// ── Contract-level: total_vouched matches sum of accepted stakes ─────────────

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 32,
        max_shrink_iters: 16,
        ..Default::default()
    })]

    /// After a random set of valid vouches, `total_vouched(borrower)` equals
    /// the sum of stakes that were accepted.
    #[test]
    fn prop_total_vouched_equals_sum_of_stakes(
        stakes in prop::collection::vec(
            DEFAULT_MIN_YIELD_STAKE..=10_000_000i128, // up to 1 XLM each
            1..=8,
        ),
    ) {
        let env = Env::default();
        env.mock_all_auths();

        let deployer = Address::generate(&env);
        let admin = Address::generate(&env);
        let admins = Vec::from_array(&env, [admin.clone()]);
        let token_id = env.register_stellar_asset_contract_v2(admin.clone());
        let contract_id = env.register_contract(None, QuorumCreditContract);
        let client = QuorumCreditContractClient::new(&env, &contract_id);
        client.initialize(&deployer, &admins, &1, &token_id.address());
        env.ledger().with_mut(|l| l.timestamp = 120);

        let borrower = Address::generate(&env);
        let token = token_id.address();
        let mut expected: i128 = 0;

        for stake in &stakes {
            let voucher = Address::generate(&env);
            StellarAssetClient::new(&env, &token).mint(&voucher, stake);
            let result = client.try_vouch(&voucher, &borrower, stake, &token, &None);
            if result.is_ok() {
                expected = expected.checked_add(*stake).expect("test stake sum overflow");
            }
        }

        let total = client.total_vouched(&borrower);
        prop_assert_eq!(total, expected);
    }
}

// ── Deterministic edge cases (complements the proptest suite) ─────────────────

#[test]
fn yield_at_default_bps_known_values() {
    assert_eq!(calc_yield(0, DEFAULT_YIELD_BPS), Some(0));
    assert_eq!(calc_yield(49, DEFAULT_YIELD_BPS), Some(0)); // below min truncates
    assert_eq!(calc_yield(50, DEFAULT_YIELD_BPS), Some(1)); // 50 * 200 / 10000
    assert_eq!(calc_yield(10_000_000, DEFAULT_YIELD_BPS), Some(200_000)); // 1 XLM → 0.02 XLM
}

#[test]
fn slash_at_default_bps_known_values() {
    assert_eq!(calc_slash(0, DEFAULT_SLASH_BPS), Some(0));
    assert_eq!(calc_slash(100, DEFAULT_SLASH_BPS), Some(50));
    assert_eq!(calc_slash(10_000_000, DEFAULT_SLASH_BPS), Some(5_000_000));
}

#[test]
fn yield_overflow_returns_none() {
    assert!(calc_yield(i128::MAX, DEFAULT_YIELD_BPS).is_none());
    assert!(calc_slash(i128::MAX, DEFAULT_SLASH_BPS).is_none());
}
