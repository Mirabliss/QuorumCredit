//! Contract invariant verification for QuorumCredit.
//!
//! This module provides:
//!
//! - Integration tests that wire [`crate::invariants::verify_invariants`] into the full
//!   loan lifecycle. The invariant definitions themselves (`InvariantViolation`,
//!   `verify_invariants`) live in `src/invariants.rs` — a non-test module compiled into
//!   the production contract, since Issue #1146 exposes them live via the
//!   `check_invariants` entrypoint for `scripts/restore.sh` to gate on. This module
//!   imports rather than redefines them so the test harness and the live entrypoint
//!   can never drift apart.
//! - A proptest-based fuzzing harness that exercises randomised vouch/loan/repay sequences.
//! - Three deliberate negative-control tests that **break** invariants and confirm the
//!   harness catches the violation (not a vacuous pass).
//!
//! ## Guarantees vs. out-of-scope
//!
//! **What this harness guarantees:**
//! - All 8 documented protocol invariants hold after every state-mutating contract call
//!   exercised in these tests.
//! - Randomised input sequences (fuzzing) do not find a path that breaks the invariants.
//! - Deliberate invariant violations are detected within the harness.
//!
//! **What remains unverified (out of scope):**
//! - Cross-contract invariants (e.g., interactions with an external oracle or bridge
//!   contract) are not exercised here; those require integration tests with deployed
//!   contracts.
//! - Upgrade-time invariants (storage migration correctness after `upgrade()`) are not
//!   checked because the WASM upgrade path requires a live network environment.
//! - Invariants over historical/archived loan records are not checked (only active state).

#![cfg(test)]

extern crate std;

use crate::invariants::{verify_invariants_in_contract, InvariantViolation};
use crate::types::{Config, DataKey};
use crate::{QuorumCreditContract, QuorumCreditContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env, String, Vec,
};

/// Test-only wrapper: enters `contract_id`'s storage context via the
/// testutils-only `as_contract`, then delegates to
/// `verify_invariants_in_contract` (which itself has no testutils dependency
/// and is what the live `check_invariants` entrypoint calls directly, since a
/// `#[contractimpl]` method already runs inside its own contract's context).
fn verify_invariants(
    env: &Env,
    contract_id: &Address,
    token: &Address,
    borrowers: &[Address],
) -> Result<(), InvariantViolation> {
    env.as_contract(contract_id, || {
        verify_invariants_in_contract(env, contract_id, token, borrowers)
    })
}

// ── Test helpers ──────────────────────────────────────────────────────────────

struct Setup {
    env: Env,
    client: QuorumCreditContractClient<'static>,
    token: Address,
    contract_id: Address,
    admin: Address,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let deployer = Address::generate(&env);
    let admin = Address::generate(&env);
    let admins = Vec::from_array(&env, [admin.clone()]);

    let token_contract = env.register_stellar_asset_contract_v2(admin.clone());
    let token = token_contract.address();
    let contract_id = env.register(QuorumCreditContract, ());

    // Fund contract generously so yield payouts never fail.
    StellarAssetClient::new(&env, &token).mint(&contract_id, &100_000_000_000);

    let client = QuorumCreditContractClient::new(&env, &contract_id);
    client.initialize(&deployer, &admins, &1u32, &token);

    // Start at t=120 so all vouches pass MIN_VOUCH_AGE (60 s).
    env.ledger().with_mut(|l| l.timestamp = 120);

    Setup { env, client, token, contract_id, admin }
}

fn purpose(env: &Env) -> String {
    String::from_str(env, "test loan")
}

/// Mint tokens to voucher and create a vouch.
fn do_vouch(s: &Setup, borrower: &Address, stake: i128) -> Address {
    let voucher = Address::generate(&s.env);
    StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &stake);
    s.client.vouch(&voucher, borrower, &stake, &s.token, &None);
    voucher
}

/// Request a loan of `amount` with `threshold`.
fn do_loan(s: &Setup, borrower: &Address, amount: i128, threshold: i128) {
    s.client
        .request_loan(borrower, &amount, &threshold, &purpose(&s.env), &s.token);
}

/// Helper: run verify_invariants and panic if violated.
fn assert_invariants(s: &Setup, borrowers: &[Address]) {
    verify_invariants(&s.env, &s.contract_id, &s.token, borrowers)
        .unwrap_or_else(|v| panic!("Invariant violation: {v}"));
}

// ── Integration tests — verify_invariants after every state change ────────────

#[test]
fn test_invariants_after_vouch() {
    let s = setup();
    let borrower = Address::generate(&s.env);

    assert_invariants(&s, &[]);

    do_vouch(&s, &borrower, 1_000_000);

    assert_invariants(&s, &[borrower]);
}

#[test]
fn test_invariants_after_loan_disbursement() {
    let s = setup();
    let borrower = Address::generate(&s.env);

    do_vouch(&s, &borrower, 10_000_000);
    assert_invariants(&s, &[borrower.clone()]);

    do_loan(&s, &borrower, 5_000_000, 10_000_000);
    assert_invariants(&s, &[borrower]);
}

#[test]
fn test_invariants_after_full_repayment() {
    let s = setup();
    let borrower = Address::generate(&s.env);

    do_vouch(&s, &borrower, 10_000_000);
    do_loan(&s, &borrower, 5_000_000, 10_000_000);

    assert_invariants(&s, &[borrower.clone()]);

    // Fund borrower for repayment (principal + yield).
    let loan = s.client.get_loan(&borrower).expect("loan should exist");
    let repay_amount = loan.amount + loan.total_yield;
    StellarAssetClient::new(&s.env, &s.token).mint(&borrower, &repay_amount);

    s.client.repay(&borrower, &repay_amount);

    // After repay borrower has no active loan — pass empty slice.
    assert_invariants(&s, &[]);
}

#[test]
fn test_invariants_after_slash() {
    let s = setup();
    let borrower = Address::generate(&s.env);
    let admins = Vec::from_array(&s.env, [s.admin.clone()]);

    do_vouch(&s, &borrower, 10_000_000);
    do_loan(&s, &borrower, 5_000_000, 10_000_000);

    assert_invariants(&s, &[borrower.clone()]);

    s.client.slash(&admins, &borrower);

    // After slash borrower has no active loan.
    assert_invariants(&s, &[]);
}

#[test]
fn test_invariants_multi_borrower() {
    let s = setup();
    let b1 = Address::generate(&s.env);
    let b2 = Address::generate(&s.env);

    do_vouch(&s, &b1, 8_000_000);
    do_vouch(&s, &b2, 6_000_000);

    assert_invariants(&s, &[b1.clone(), b2.clone()]);

    do_loan(&s, &b1, 4_000_000, 8_000_000);

    assert_invariants(&s, &[b1.clone(), b2.clone()]);

    do_loan(&s, &b2, 3_000_000, 6_000_000);

    assert_invariants(&s, &[b1, b2]);
}

#[test]
fn test_invariants_after_config_update() {
    let s = setup();
    let borrower = Address::generate(&s.env);
    let admins = Vec::from_array(&s.env, [s.admin.clone()]);

    do_vouch(&s, &borrower, 10_000_000);

    // Update yield_bps to 500 (5%).
    s.client.update_config(&admins, &Some(500i128), &None);

    assert_invariants(&s, &[borrower]);
}

// ── Proptest-based fuzzing harness ────────────────────────────────────────────

#[cfg(test)]
mod fuzz {
    use super::*;
    use proptest::prelude::*;

    /// A single operation in a randomised scenario.
    #[derive(Debug, Clone)]
    enum Op {
        Vouch { stake: i128 },
        Loan { amount_fraction: u32 }, // fraction of total stake (1-100%)
        Repay,
        Slash,
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            // Vouch with a stake between 1 stroop and 10 XLM
            (1i128..=100_000_000i128).prop_map(|s| Op::Vouch { stake: s }),
            // Loan for 10%–100% of stake
            (10u32..=100u32).prop_map(|f| Op::Loan { amount_fraction: f }),
            Just(Op::Repay),
            Just(Op::Slash),
        ]
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 64,
            max_shrink_iters: 32,
            ..Default::default()
        })]

        /// Invariants hold after any randomised sequence of vouch/loan/repay/slash operations.
        #[test]
        fn prop_invariants_hold_under_random_ops(ops in prop::collection::vec(op_strategy(), 1..=12)) {
            let s = setup();
            let borrower = Address::generate(&s.env);
            let admins = Vec::from_array(&s.env, [s.admin.clone()]);
            let mut total_stake: i128 = 0;
            let mut has_loan = false;

            for op in &ops {
                match op {
                    Op::Vouch { stake } => {
                        if !has_loan {
                            do_vouch(&s, &borrower, *stake);
                            total_stake = total_stake.saturating_add(*stake);
                        }
                    }
                    Op::Loan { amount_fraction } => {
                        if !has_loan && total_stake > 0 {
                            let amount = (total_stake * (*amount_fraction as i128) / 100).max(100_000);
                            let _ = s.client.try_request_loan(
                                &borrower,
                                &amount,
                                &total_stake,
                                &purpose(&s.env),
                                &s.token,
                            );
                            // Check if loan was actually created.
                            has_loan = s.client.get_loan(&borrower).is_some();
                        }
                    }
                    Op::Repay => {
                        if has_loan {
                            if let Some(loan) = s.client.get_loan(&borrower) {
                                let needed = (loan.amount + loan.total_yield)
                                    .saturating_sub(loan.amount_repaid)
                                    .max(1);
                                StellarAssetClient::new(&s.env, &s.token)
                                    .mint(&borrower, &needed);
                                let _ = s.client.try_repay(&borrower, &needed);
                                has_loan = s.client.get_loan(&borrower).is_some();
                            }
                        }
                    }
                    Op::Slash => {
                        if has_loan {
                            let _ = s.client.try_slash(&admins, &borrower);
                            has_loan = s.client.get_loan(&borrower).is_some();
                        }
                    }
                }

                // Assert all invariants after every operation.
                let borrowers: std::vec::Vec<Address> = if has_loan || total_stake > 0 {
                    std::vec![borrower.clone()]
                } else {
                    std::vec![]
                };

                verify_invariants(&s.env, &s.contract_id, &s.token, &borrowers)
                    .unwrap_or_else(|v| panic!("Invariant violated after {op:?}: {v}"));
            }
        }
    }
}

// ── Negative-control tests — prove the harness catches violations ─────────────

/// These tests deliberately corrupt state to check that `verify_invariants`
/// returns the expected `InvariantViolation` rather than silently passing.
#[cfg(test)]
mod negative_controls {
    use super::*;

    // ── NC-1: I7 — yield_bps out of range ─────────────────────────────────────
    //
    // We directly write an invalid yield_bps (11_000 > 10_000) into the Config
    // storage and verify that verify_invariants catches it.
    #[test]
    fn negative_control_yield_bps_out_of_range() {
        let s = setup();
        let borrower = Address::generate(&s.env);
        do_vouch(&s, &borrower, 1_000_000);

        // Corrupt yield_bps directly in storage.
        s.env.as_contract(&s.contract_id, || {
            let mut cfg: Config = s
                .env
                .storage()
                .instance()
                .get(&DataKey::Config)
                .expect("config");
            cfg.yield_bps = 11_000; // invalid: > 10_000
            s.env.storage().instance().set(&DataKey::Config, &cfg);
        });

        let result = verify_invariants(&s.env, &s.contract_id, &s.token, &[borrower]);
        match result {
            Err(InvariantViolation::YieldBpsOutOfRange { yield_bps }) => {
                assert_eq!(yield_bps, 11_000, "should report the bad yield_bps value");
            }
            other => panic!("Expected YieldBpsOutOfRange, got: {other:?}"),
        }
    }

    // ── NC-2: I8 — admin_threshold exceeds admin count ────────────────────────
    //
    // We set admin_threshold to 5 when there is only 1 admin and verify that
    // verify_invariants returns AdminThresholdInvalid.
    #[test]
    fn negative_control_admin_threshold_exceeds_admin_count() {
        let s = setup();

        // Corrupt admin_threshold directly in storage.
        s.env.as_contract(&s.contract_id, || {
            let mut cfg: Config = s
                .env
                .storage()
                .instance()
                .get(&DataKey::Config)
                .expect("config");
            cfg.admin_threshold = 5; // invalid: only 1 admin
            s.env.storage().instance().set(&DataKey::Config, &cfg);
        });

        let result = verify_invariants(&s.env, &s.contract_id, &s.token, &[]);
        match result {
            Err(InvariantViolation::AdminThresholdInvalid { threshold, admin_count }) => {
                assert_eq!(threshold, 5);
                assert_eq!(admin_count, 1);
            }
            other => panic!("Expected AdminThresholdInvalid, got: {other:?}"),
        }
    }

    // ── NC-3: I6 — slash treasury is negative ────────────────────────────────
    //
    // We directly write -1 into the SlashTreasury key and verify that
    // verify_invariants returns SlashTreasuryNegative.
    #[test]
    fn negative_control_slash_treasury_negative() {
        let s = setup();

        // Corrupt SlashTreasury directly in storage.
        s.env.as_contract(&s.contract_id, || {
            s.env
                .storage()
                .instance()
                .set(&DataKey::SlashTreasury, &(-1i128));
        });

        let result = verify_invariants(&s.env, &s.contract_id, &s.token, &[]);
        match result {
            Err(InvariantViolation::SlashTreasuryNegative { balance }) => {
                assert_eq!(balance, -1);
            }
            other => panic!("Expected SlashTreasuryNegative, got: {other:?}"),
        }
    }
}
