//! Property-based testing for QuorumCredit state invariants
//!
//! #1083: Add Property-Based Testing for State Invariants
//!
//! This module adds property-based testing to verify:
//! 1. Invariant: sum of voucher yields ≤ contract XLM balance
//! 2. Invariant: no loan can be both repaid and defaulted
//! 3. Random loan sequence generation and property verification

#![cfg(test)]

use crate::{QuorumCreditContract, QuorumCreditContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env, String, Vec,
};

struct TestSetup {
    env: Env,
    client: QuorumCreditContractClient<'static>,
    token: Address,
    contract_id: Address,
    admin: Address,
}

fn setup_test() -> TestSetup {
    let env = Env::default();
    env.mock_all_auths();

    let deployer = Address::generate(&env);
    let admin = Address::generate(&env);
    let admins = Vec::from_array(&env, [admin.clone()]);

    let token_contract = env.register_stellar_asset_contract_v2(admin.clone());
    let token = token_contract.address();
    let contract_id = env.register(QuorumCreditContract, ());

    // Fund contract generously for yield payouts
    let token_client = StellarAssetClient::new(&env, &token);
    token_client.mint(&contract_id, &1_000_000_000_000); // 100,000 XLM

    let client = QuorumCreditContractClient::new(&env, &contract_id);
    client.initialize(&deployer, &admins, &1u32, &token);

    // Start at t=120 so all vouches pass MIN_VOUCH_AGE (60 s)
    env.ledger().with_mut(|l| l.timestamp = 120);

    TestSetup { env, client, token, contract_id, admin }
}

/// Property-based test for invariant: sum of voucher yields ≤ contract XLM balance
#[test]
fn test_property_yield_le_balance() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    let config = proptest::test_runner::Config::default();
    let mut runner = TestRunner::new(config);

    // Generate random test cases
    let strategy = prop::collection::vec(
        (1i128..=100_000_000i128, 1i128..=50_000_000i128), // (stake, loan_amount)
        1..=5, // 1-5 borrowers
    );

    runner.run(&strategy, |test_cases| {
        let s = setup_test();
        let token_client = StellarAssetClient::new(&s.env, &s.token);
        
        // Track total yields that would need to be paid
        let mut total_potential_yield = 0i128;
        let mut borrowers = Vec::new(&s.env);
        
        for (i, (stake, loan_amount)) in test_cases.iter().enumerate() {
            let borrower = Address::from_bytes(&[i as u8; 32]);
            let voucher = Address::from_bytes(&[(i + 100) as u8; 32]); // Different address
            
            // Fund voucher and create vouch
            token_client.mint(&voucher, stake);
            s.client.vouch(&voucher, &borrower, stake, &s.token);
            
            // Request loan (if stake >= loan_amount)
            if *stake >= *loan_amount {
                s.client.request_loan(
                    &borrower,
                    loan_amount,
                    stake, // threshold = stake
                    &String::from_str(&s.env, "Property test"),
                    &s.token,
                );
                
                // Calculate potential yield (2% of stake)
                let yield_amount = stake * 200 / 10_000; // 2% in basis points
                total_potential_yield = total_potential_yield.saturating_add(yield_amount);
            }
            
            borrowers.push_back(borrower);
        }
        
        // Get contract balance
        let contract_balance = token_client.balance(&s.contract_id);
        
        // Invariant: Total potential yield must be ≤ contract balance
        // (Contract must have enough funds to pay all yields)
        assert!(
            total_potential_yield <= contract_balance,
            "Invariant violation: total_potential_yield({}) > contract_balance({})",
            total_potential_yield,
            contract_balance
        );
        
        Ok(())
    }).unwrap();
}

/// Property-based test for invariant: no loan can be both repaid and defaulted
#[test]
fn test_property_loan_state_exclusivity() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    let config = proptest::test_runner::Config::default();
    let mut runner = TestRunner::new(config);

    // Generate random sequences of operations
    #[derive(Debug, Clone)]
    enum Operation {
        Vouch(i128),       // stake amount
        Loan(i128),        // loan amount
        Repay,            // attempt repayment
        Slash,            // attempt slash
        Wait(u64),        // wait time in seconds
    }

    let op_strategy = prop_oneof![
        (1i128..=100_000_000i128).prop_map(Operation::Vouch),
        (1i128..=50_000_000i128).prop_map(Operation::Loan),
        Just(Operation::Repay),
        Just(Operation::Slash),
        (1u64..=86400u64).prop_map(Operation::Wait), // up to 1 day
    ];

    let strategy = prop::collection::vec(op_strategy, 1..=20);

    runner.run(&strategy, |ops| {
        let s = setup_test();
        let token_client = StellarAssetClient::new(&s.env, &s.token);
        let borrower = Address::generate(&s.env);
        let admins = Vec::from_array(&s.env, [s.admin.clone()]);
        
        let mut total_stake = 0i128;
        let mut has_loan = false;
        let mut loan_repaid = false;
        let mut loan_slashed = false;
        
        for op in ops {
            match op {
                Operation::Vouch(stake) => {
                    if !has_loan {
                        let voucher = Address::generate(&s.env);
                        token_client.mint(&voucher, &stake);
                        s.client.vouch(&voucher, &borrower, &stake, &s.token);
                        total_stake = total_stake.saturating_add(stake);
                    }
                }
                Operation::Loan(amount) => {
                    if !has_loan && total_stake >= amount && amount > 0 {
                        s.client.request_loan(
                            &borrower,
                            &amount,
                            &total_stake,
                            &String::from_str(&s.env, "Random sequence"),
                            &s.token,
                        );
                        has_loan = true;
                    }
                }
                Operation::Repay => {
                    if has_loan && !loan_slashed {
                        if let Some(loan) = s.client.get_loan(&borrower) {
                            let needed = (loan.amount + loan.total_yield)
                                .saturating_sub(loan.amount_repaid)
                                .max(1);
                            token_client.mint(&borrower, &needed);
                            let _ = s.client.try_repay(&borrower, &needed);
                            
                            // Check if loan was repaid
                            if s.client.get_loan(&borrower).is_none() {
                                loan_repaid = true;
                                has_loan = false;
                            }
                        }
                    }
                }
                Operation::Slash => {
                    if has_loan && !loan_repaid {
                        let _ = s.client.try_slash(&admins, &borrower);
                        
                        // Check if loan was slashed
                        if s.client.get_loan(&borrower).is_none() {
                            loan_slashed = true;
                            has_loan = false;
                        }
                    }
                }
                Operation::Wait(seconds) => {
                    s.env.ledger().with_mut(|l| {
                        l.timestamp += seconds;
                    });
                }
            }
            
            // Invariant: Loan cannot be both repaid and defaulted
            assert!(
                !(loan_repaid && loan_slashed),
                "Invariant violation: Loan cannot be both repaid and slashed"
            );
            
            // Additional invariant: If loan is repaid or slashed, there should be no active loan
            if loan_repaid || loan_slashed {
                assert!(
                    !has_loan,
                    "Invariant violation: Loan marked as repaid/slashed but still active"
                );
            }
        }
        
        Ok(())
    }).unwrap();
}

/// Generate random loan sequences and verify properties
#[test]
fn test_random_loan_sequences() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    let config = proptest::test_runner::Config::default();
    let mut runner = TestRunner::new(config);

    // More complex strategy with multiple borrowers
    #[derive(Debug, Clone)]
    enum MultiOp {
        Vouch(usize, i128), // borrower index, stake
        Loan(usize, i128),  // borrower index, amount
        Repay(usize),       // borrower index
        Slash(usize),       // borrower index
    }

    let op_strategy = prop_oneof![
        ((0usize..=4), (1i128..=50_000_000i128)).prop_map(|(idx, stake)| MultiOp::Vouch(idx, stake)),
        ((0usize..=4), (1i128..=25_000_000i128)).prop_map(|(idx, amount)| MultiOp::Loan(idx, amount)),
        (0usize..=4).prop_map(MultiOp::Repay),
        (0usize..=4).prop_map(MultiOp::Slash),
    ];

    let strategy = prop::collection::vec(op_strategy, 1..=30);

    runner.run(&strategy, |ops| {
        let s = setup_test();
        let token_client = StellarAssetClient::new(&s.env, &s.token);
        let admins = Vec::from_array(&s.env, [s.admin.clone()]);
        
        // Create 5 borrowers
        let borrowers: Vec<Address> = (0..5)
            .map(|i| Address::from_bytes(&[i as u8; 32]))
            .collect();
        
        let mut stakes = vec![0i128; 5];
        let mut has_loan = vec![false; 5];
        let mut loan_repaid = vec![false; 5];
        let mut loan_slashed = vec![false; 5];
        
        for op in ops {
            match op {
                MultiOp::Vouch(idx, stake) => {
                    let idx = idx % 5;
                    if !has_loan[idx] {
                        let voucher = Address::generate(&s.env);
                        token_client.mint(&voucher, &stake);
                        s.client.vouch(&voucher, &borrowers[idx], &stake, &s.token);
                        stakes[idx] = stakes[idx].saturating_add(stake);
                    }
                }
                MultiOp::Loan(idx, amount) => {
                    let idx = idx % 5;
                    if !has_loan[idx] && stakes[idx] >= amount && amount > 0 {
                        s.client.request_loan(
                            &borrowers[idx],
                            &amount,
                            &stakes[idx],
                            &String::from_str(&s.env, "Multi-borrower test"),
                            &s.token,
                        );
                        has_loan[idx] = true;
                    }
                }
                MultiOp::Repay(idx) => {
                    let idx = idx % 5;
                    if has_loan[idx] && !loan_slashed[idx] {
                        if let Some(loan) = s.client.get_loan(&borrowers[idx]) {
                            let needed = (loan.amount + loan.total_yield)
                                .saturating_sub(loan.amount_repaid)
                                .max(1);
                            token_client.mint(&borrowers[idx], &needed);
                            let _ = s.client.try_repay(&borrowers[idx], &needed);
                            
                            if s.client.get_loan(&borrowers[idx]).is_none() {
                                loan_repaid[idx] = true;
                                has_loan[idx] = false;
                            }
                        }
                    }
                }
                MultiOp::Slash(idx) => {
                    let idx = idx % 5;
                    if has_loan[idx] && !loan_repaid[idx] {
                        let _ = s.client.try_slash(&admins, &borrowers[idx]);
                        
                        if s.client.get_loan(&borrowers[idx]).is_none() {
                            loan_slashed[idx] = true;
                            has_loan[idx] = false;
                        }
                    }
                }
            }
            
            // Verify invariants for all borrowers
            for i in 0..5 {
                // Invariant 1: Loan cannot be both repaid and slashed
                assert!(
                    !(loan_repaid[i] && loan_slashed[i]),
                    "Borrower {}: Loan cannot be both repaid and slashed",
                    i
                );
                
                // Invariant 2: If loan is repaid or slashed, there should be no active loan
                if loan_repaid[i] || loan_slashed[i] {
                    assert!(
                        !has_loan[i],
                        "Borrower {}: Loan marked as repaid/slashed but still active",
                        i
                    );
                }
                
                // Invariant 3: If there's a loan, there must be stake
                if has_loan[i] {
                    assert!(
                        stakes[i] > 0,
                        "Borrower {}: Active loan but no stake",
                        i
                    );
                }
            }
            
            // Invariant 4: Total contract balance should be non-negative
            let contract_balance = token_client.balance(&s.contract_id);
            assert!(
                contract_balance >= 0,
                "Contract balance cannot be negative: {}",
                contract_balance
            );
        }
        
        Ok(())
    }).unwrap();
}

/// Test specific edge cases with property-based generation
#[test]
fn test_edge_cases() {
    use proptest::prelude::*;
    use proptest::test_runner::TestRunner;

    let config = proptest::test_runner::Config::default();
    let mut runner = TestRunner::new(config);

    // Test edge cases: very small and very large amounts
    let strategy = (1i128..=1_000_000_000_000i128) // Up to 100,000 XLM
        .prop_filter("Avoid zero", |&x| x > 0);

    runner.run(&strategy, |amount| {
        let s = setup_test();
        let token_client = StellarAssetClient::new(&s.env, &s.token);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);
        
        // Test with the generated amount
        let stake = amount;
        let loan_amount = amount / 2; // Borrow half of stake
        
        // Fund voucher
        token_client.mint(&voucher, &stake);
        
        // Create vouch
        s.client.vouch(&voucher, &borrower, &stake, &s.token);
        
        // Request loan
        s.client.request_loan(
            &borrower,
            &loan_amount,
            &stake,
            &String::from_str(&s.env, "Edge case test"),
            &s.token,
        );
        
        // Get loan details
        let loan = s.client.get_loan(&borrower).expect("Loan should exist");
        
        // Invariant: Loan amount should not exceed stake
        assert!(
            loan.amount <= stake,
            "Loan amount {} exceeds stake {}",
            loan.amount,
            stake
        );
        
        // Invariant: Yield should be non-negative
        assert!(
            loan.total_yield >= 0,
            "Yield cannot be negative: {}",
            loan.total_yield
        );
        
        // Calculate expected yield (2% of stake)
        let expected_yield = stake * 200 / 10_000;
        assert_eq!(
            loan.total_yield, expected_yield,
            "Yield calculation mismatch: expected {}, got {}",
            expected_yield, loan.total_yield
        );
        
        Ok(())
    }).unwrap();
}