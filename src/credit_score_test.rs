#![cfg(test)]

use crate::credit_score::{
    calculate_credit_score, calculate_timeliness_score, calculate_repayment_history_score,
    calculate_loan_count_score, calculate_account_age_score, calculate_vouching_score,
};
use crate::types::{
    CreditScore, CreditTier, DataKey, LoanRecord, LoanStatus, PaymentRecord,
    DEFAULT_CREDIT_SCORE_CONFIG,
};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, Vec,
};

#[test]
fn test_timeliness_score_early_repayment() {
    // 5 days early = 432000 seconds
    let early_secs: i64 = 5 * 24 * 60 * 60;
    let score = calculate_timeliness_score(early_secs);
    assert!(score > 500, "Early repayment should score > 500, got {}", score);
}

#[test]
fn test_timeliness_score_late_repayment() {
    // 3 days late = -259200 seconds
    let late_secs: i64 = -3 * 24 * 60 * 60;
    let score = calculate_timeliness_score(late_secs);
    assert!(score < 500, "Late repayment should score < 500, got {}", score);
}

#[test]
fn test_timeliness_score_neutral() {
    let score = calculate_timeliness_score(0);
    assert_eq!(score, 500, "Neutral timeliness should score 500");
}

#[test]
fn test_timeliness_score_very_early() {
    // 7+ days early = max score
    let very_early_secs: i64 = 8 * 24 * 60 * 60;
    let score = calculate_timeliness_score(very_early_secs);
    assert_eq!(score, 1000, "Very early repayment should score 1000");
}

#[test]
fn test_timeliness_score_very_late() {
    // 7+ days late = min score
    let very_late_secs: i64 = -8 * 24 * 60 * 60;
    let score = calculate_timeliness_score(very_late_secs);
    assert_eq!(score, 0, "Very late repayment should score 0");
}

#[test]
fn test_repayment_history_score_perfect() {
    // 5 successful out of 5 loans, no defaults
    let score = calculate_repayment_history_score(5, 5, 0);
    assert_eq!(score, 1000, "Perfect repayment should score 1000");
}

#[test]
fn test_repayment_history_score_with_defaults() {
    // 5 successful out of 6 loans, 1 default
    // success_rate = 5/6 * 1000 = 833
    // penalty = 1 * 200 = 200
    // adjusted = 833 - 200 = 633
    let score = calculate_repayment_history_score(5, 6, 1);
    assert_eq!(score, 633, "5/6 with 1 default should score 633");
}

#[test]
fn test_repayment_history_score_new_user() {
    // New user with no loans
    let score = calculate_repayment_history_score(0, 0, 0);
    assert_eq!(score, 500, "New user should score 500 (neutral)");
}

#[test]
fn test_loan_count_score() {
    // Max benefit at 10 loans
    let score_5 = calculate_loan_count_score(5);
    let score_10 = calculate_loan_count_score(10);
    let score_15 = calculate_loan_count_score(15);
    
    assert!(score_5 < score_10, "5 loans should score < 10 loans");
    assert_eq!(score_10, 1000, "10 loans should score 1000 (max)");
    assert_eq!(score_15, 1000, "15 loans capped at 1000");
}

#[test]
fn test_account_age_score() {
    // Max benefit at 1 year
    let one_year = 365 * 24 * 60 * 60;
    let score_1_year = calculate_account_age_score(one_year);
    let score_half_year = calculate_account_age_score(one_year / 2);
    
    assert_eq!(score_1_year, 1000, "1 year account age should score 1000");
    assert_eq!(score_half_year, 500, "0.5 year account age should score 500");
}

#[test]
fn test_vouching_score() {
    // Legacy path (None env/borrower): max benefit at 20 vouches
    let score_10 = calculate_vouching_score(10, None, None);
    let score_20 = calculate_vouching_score(20, None, None);
    let score_30 = calculate_vouching_score(30, None, None);
    
    assert!(score_10 < score_20, "10 vouches should score < 20 vouches");
    assert_eq!(score_20, 1000, "20 vouches should score 1000 (max)");
    assert_eq!(score_30, 1000, "30 vouches capped at 1000");
}

#[test]
fn test_different_repayment_histories_produce_different_scores() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(crate::QuorumCreditContract, ());
    let borrower_early = Address::generate(&env);
    let borrower_late = Address::generate(&env);

    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::CreditScoreConfig, &DEFAULT_CREDIT_SCORE_CONFIG);

        let loan_id_early = 1u64;
        let now = env.ledger().timestamp();
        let deadline = now + 100_000;

        let loan_early = LoanRecord {
            id: loan_id_early,
            borrower: borrower_early.clone(),
            guarantor: None,
            buyback_price: 0,
            auto_repay_enabled: false,
            auto_repay_attempts: 0,
            escrow_status: crate::types::EscrowStatus::None,
            co_borrowers: Vec::new(&env),
            amount: 1_000_000,
            amount_repaid: 1_000_000,
            total_yield: 20_000,
            status: LoanStatus::Repaid,
            repaid: true,
            defaulted: false,
            created_at: now,
            disbursement_timestamp: now,
            repayment_timestamp: Some(now + 10_000),
            deadline,
            loan_purpose: soroban_sdk::String::from_str(&env, "test"),
            token_address: Address::generate(&env),
            amortization_schedule: Vec::new(&env),
            reminder_sent: false,
            risk_score: 50,
            deferment_periods: 0,
            maturity_date: None,
            rate_type: crate::types::RateType::Fixed,
            index_reference: None,
            last_interest_calc: now,
            accrued_interest: 0,
            milestone_bonus_applied: 0,
            retry_count: 0,
            suspension_timestamp: None,
            suspension_amount_repaid: 0,
        };

        let loan_id_late = 2u64;
        let loan_late = LoanRecord {
            id: loan_id_late,
            borrower: borrower_late.clone(),
            guarantor: None,
            buyback_price: 0,
            auto_repay_enabled: false,
            auto_repay_attempts: 0,
            escrow_status: crate::types::EscrowStatus::None,
            co_borrowers: Vec::new(&env),
            amount: 1_000_000,
            amount_repaid: 1_000_000,
            total_yield: 20_000,
            status: LoanStatus::Repaid,
            repaid: true,
            defaulted: false,
            created_at: now,
            disbursement_timestamp: now,
            repayment_timestamp: Some(deadline + 10_000),
            deadline,
            loan_purpose: soroban_sdk::String::from_str(&env, "test"),
            token_address: Address::generate(&env),
            amortization_schedule: Vec::new(&env),
            reminder_sent: false,
            risk_score: 50,
            deferment_periods: 0,
            maturity_date: None,
            rate_type: crate::types::RateType::Fixed,
            index_reference: None,
            last_interest_calc: now,
            accrued_interest: 0,
            milestone_bonus_applied: 0,
            retry_count: 0,
            suspension_timestamp: None,
            suspension_amount_repaid: 0,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan_id_early), &loan_early);
        env.storage()
            .persistent()
            .set(&DataKey::Loan(loan_id_late), &loan_late);
        env.storage()
            .persistent()
            .set(&DataKey::LoanCounter, &(2u64));
        env.storage()
            .persistent()
            .set(&DataKey::LoanCount(borrower_early.clone()), &(1u32));
        env.storage()
            .persistent()
            .set(&DataKey::LoanCount(borrower_late.clone()), &(1u32));
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower_early.clone()), &(1u32));
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower_late.clone()), &(1u32));

        let registration_time = now - 30_000_000;
        env.storage()
            .persistent()
            .set(&DataKey::BorrowerRegistered(borrower_early.clone()), &registration_time);
        env.storage()
            .persistent()
            .set(&DataKey::BorrowerRegistered(borrower_late.clone()), &registration_time);

        let score_early = calculate_credit_score(&env, &borrower_early).unwrap();
        let score_late = calculate_credit_score(&env, &borrower_late).unwrap();

        assert!(
            score_early.score > score_late.score,
            "Early repayment score ({}) should be > late repayment score ({})",
            score_early.score,
            score_late.score
        );
        assert!(
            score_early.avg_repayment_time > 0,
            "Early repayment avg_repayment_time ({}) should be positive",
            score_early.avg_repayment_time
        );
        assert!(
            score_late.avg_repayment_time < 0,
            "Late repayment avg_repayment_time ({}) should be negative",
            score_late.avg_repayment_time
        );
    });
}

#[test]
fn test_credit_score_total_borrowed() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(crate::QuorumCreditContract, ());
    let borrower = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let now = env.ledger().timestamp();

        env.storage()
            .instance()
            .set(&DataKey::CreditScoreConfig, &DEFAULT_CREDIT_SCORE_CONFIG);

        let loan1 = LoanRecord {
            id: 1u64,
            borrower: borrower.clone(),
            guarantor: None,
            buyback_price: 0,
            auto_repay_enabled: false,
            auto_repay_attempts: 0,
            escrow_status: crate::types::EscrowStatus::None,
            co_borrowers: Vec::new(&env),
            amount: 500_000,
            amount_repaid: 500_000,
            total_yield: 10_000,
            status: LoanStatus::Repaid,
            repaid: true,
            defaulted: false,
            created_at: now,
            disbursement_timestamp: now,
            repayment_timestamp: Some(now + 50_000),
            deadline: now + 100_000,
            loan_purpose: soroban_sdk::String::from_str(&env, "test1"),
            token_address: Address::generate(&env),
            amortization_schedule: Vec::new(&env),
            reminder_sent: false,
            risk_score: 50,
            deferment_periods: 0,
            maturity_date: None,
            rate_type: crate::types::RateType::Fixed,
            index_reference: None,
            last_interest_calc: now,
            accrued_interest: 0,
            milestone_bonus_applied: 0,
            retry_count: 0,
            suspension_timestamp: None,
            suspension_amount_repaid: 0,
        };

        let loan2 = LoanRecord {
            id: 2u64,
            borrower: borrower.clone(),
            guarantor: None,
            buyback_price: 0,
            auto_repay_enabled: false,
            auto_repay_attempts: 0,
            escrow_status: crate::types::EscrowStatus::None,
            co_borrowers: Vec::new(&env),
            amount: 300_000,
            amount_repaid: 300_000,
            total_yield: 6_000,
            status: LoanStatus::Repaid,
            repaid: true,
            defaulted: false,
            created_at: now,
            disbursement_timestamp: now,
            repayment_timestamp: Some(now + 50_000),
            deadline: now + 100_000,
            loan_purpose: soroban_sdk::String::from_str(&env, "test2"),
            token_address: Address::generate(&env),
            amortization_schedule: Vec::new(&env),
            reminder_sent: false,
            risk_score: 50,
            deferment_periods: 0,
            maturity_date: None,
            rate_type: crate::types::RateType::Fixed,
            index_reference: None,
            last_interest_calc: now,
            accrued_interest: 0,
            milestone_bonus_applied: 0,
            retry_count: 0,
            suspension_timestamp: None,
            suspension_amount_repaid: 0,
        };

        env.storage().persistent().set(&DataKey::Loan(1u64), &loan1);
        env.storage().persistent().set(&DataKey::Loan(2u64), &loan2);
        env.storage()
            .persistent()
            .set(&DataKey::LoanCounter, &(2u64));
        env.storage()
            .persistent()
            .set(&DataKey::LoanCount(borrower.clone()), &(2u32));
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower.clone()), &(2u32));
        env.storage()
            .persistent()
            .set(&DataKey::BorrowerRegistered(borrower.clone()), &now);

        let credit_score = calculate_credit_score(&env, &borrower).unwrap();
        assert_eq!(
            credit_score.total_borrowed, 800_000,
            "Total borrowed should be 800_000"
        );
    });
}

#[test]
fn test_credit_score_total_repaid() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(crate::QuorumCreditContract, ());
    let borrower = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let now = env.ledger().timestamp();

        env.storage()
            .instance()
            .set(&DataKey::CreditScoreConfig, &DEFAULT_CREDIT_SCORE_CONFIG);

        let loan = LoanRecord {
            id: 1u64,
            borrower: borrower.clone(),
            guarantor: None,
            buyback_price: 0,
            auto_repay_enabled: false,
            auto_repay_attempts: 0,
            escrow_status: crate::types::EscrowStatus::None,
            co_borrowers: Vec::new(&env),
            amount: 1_000_000,
            amount_repaid: 750_000,
            total_yield: 20_000,
            status: LoanStatus::Active,
            repaid: false,
            defaulted: false,
            created_at: now,
            disbursement_timestamp: now,
            repayment_timestamp: None,
            deadline: now + 100_000,
            loan_purpose: soroban_sdk::String::from_str(&env, "test"),
            token_address: Address::generate(&env),
            amortization_schedule: Vec::new(&env),
            reminder_sent: false,
            risk_score: 50,
            deferment_periods: 0,
            maturity_date: None,
            rate_type: crate::types::RateType::Fixed,
            index_reference: None,
            last_interest_calc: now,
            accrued_interest: 0,
            milestone_bonus_applied: 0,
            retry_count: 0,
            suspension_timestamp: None,
            suspension_amount_repaid: 0,
        };

        env.storage().persistent().set(&DataKey::Loan(1u64), &loan);
        env.storage()
            .persistent()
            .set(&DataKey::LoanCounter, &(1u64));
        env.storage()
            .persistent()
            .set(&DataKey::LoanCount(borrower.clone()), &(1u32));
        env.storage()
            .persistent()
            .set(&DataKey::RepaymentCount(borrower.clone()), &(0u32));
        env.storage()
            .persistent()
            .set(&DataKey::BorrowerRegistered(borrower.clone()), &now);

        let credit_score = calculate_credit_score(&env, &borrower).unwrap();
        assert_eq!(
            credit_score.total_repaid, 750_000,
            "Total repaid should be 750_000"
        );
    });
}

#[test]
fn test_credit_score_migration_strategy_note() {
    // This test documents the migration strategy for existing borrowers
    // Currently, there is NO HISTORICAL DATA for borrowers on the old contract
    // They will have:
    // - total_borrowed: 0 (since they may have had loans, but loan records don't predate)
    // - total_repaid: 0 (same)
    // - avg_repayment_time: 0 (neutral, no history)
    // 
    // Migration path:
    // 1. Any existing borrower's historical loans should be backfilled from off-chain data or contract logs
    // 2. Create LoanRecord entries for past loans with accurate:
    //    - disbursement_timestamp
    //    - repayment_timestamp
    //    - amount
    //    - status (Repaid or Defaulted)
    // 3. This populates their aggregates correctly going forward
    // 4. For borrowers with no off-chain history, they restart with neutral score (500) and build from new loans
    
    // Until backfill is implemented, all credit scores will be based on post-upgrade activity only
}

// ────────────────────────────────────────────────────────────────────────────
// Sybil Resistance Tests
// ────────────────────────────────────────────────────────────────────────────

use crate::credit_score::{
    integer_sqrt_u64, SYBIL_MIN_STAKE_FOR_CREDIT, SYBIL_MIN_VOUCH_AGE_SECS,
    SYBIL_STAKE_TIME_SATURATION,
};
use crate::types::VouchRecord;
use crate::vouch::{
    vouch_reputation_weight, SYBIL_MIN_STAKE_FOR_REP, SYBIL_REP_SATURATION,
};

// ── integer_sqrt_u64 ─────────────────────────────────────────────────────────

#[test]
fn test_integer_sqrt_zero() {
    assert_eq!(integer_sqrt_u64(0), 0);
}

#[test]
fn test_integer_sqrt_one() {
    assert_eq!(integer_sqrt_u64(1), 1);
}

#[test]
fn test_integer_sqrt_perfect_squares() {
    assert_eq!(integer_sqrt_u64(4), 2);
    assert_eq!(integer_sqrt_u64(9), 3);
    assert_eq!(integer_sqrt_u64(100), 10);
    assert_eq!(integer_sqrt_u64(10_000), 100);
}

#[test]
fn test_integer_sqrt_floor_rounding() {
    // sqrt(2) ≈ 1.41 → floor = 1
    assert_eq!(integer_sqrt_u64(2), 1);
    // sqrt(8) ≈ 2.83 → floor = 2
    assert_eq!(integer_sqrt_u64(8), 2);
}

// ── calculate_vouching_score (legacy path) ───────────────────────────────────

#[test]
fn test_vouching_score_legacy_zero_vouches() {
    let score = calculate_vouching_score(0, None, None);
    assert_eq!(score, 0, "zero vouches = 0 score");
}

#[test]
fn test_vouching_score_legacy_half_max() {
    let score = calculate_vouching_score(10, None, None);
    assert_eq!(score, 500, "10/20 vouches = 500 score");
}

// ── calculate_vouching_score (stake-time path) ──────────────────────────────

#[test]
fn test_vouching_score_sybil_ring_earns_zero() {
    // A Sybil ring: vouches with stake below the floor OR age below the floor
    // should contribute 0 score.
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, QuorumCreditContract);
    let borrower = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let now = env.ledger().timestamp();

        // Create 10 vouches with trivial stake (below SYBIL_MIN_STAKE_FOR_CREDIT)
        let mut vouches: soroban_sdk::Vec<VouchRecord> = soroban_sdk::Vec::new(&env);
        for _ in 0..10 {
            vouches.push_back(VouchRecord {
                voucher: Address::generate(&env),
                stake: SYBIL_MIN_STAKE_FOR_CREDIT - 1, // below floor
                vouch_timestamp: now, // just created, below age floor
                token: Address::generate(&env),
                expiry_timestamp: None,
                delegate: None,
                chain_id: None,
            });
        }
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower.clone()), &vouches);

        let score = calculate_vouching_score(0, Some(&env), Some(&borrower));
        assert_eq!(
            score, 0,
            "Sybil ring with trivial stakes and new vouches should score 0, got {}",
            score
        );
    });
}

#[test]
fn test_vouching_score_genuine_voucher_scores_higher() {
    // A genuine voucher with significant stake aged past the floor
    // should score higher than the Sybil ring.
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();

    // Set ledger time to something significant so we can age vouches
    env.ledger().with_mut(|l| {
        l.timestamp = 10 * 24 * 60 * 60; // 10 days
    });

    let contract_id = env.register_contract(None, QuorumCreditContract);
    let borrower = Address::generate(&env);
    let borrower2 = Address::generate(&env);

    env.as_contract(&contract_id, || {
        let now = env.ledger().timestamp();

        // One genuine vouch: 10 XLM stake, 7 days old
        let vouch_timestamp = now - 7 * 24 * 60 * 60; // 7 days ago
        let genuine_stake = 100_000_000i128; // 10 XLM = 100_000_000 stroops
        let mut vouches: soroban_sdk::Vec<VouchRecord> = soroban_sdk::Vec::new(&env);
        vouches.push_back(VouchRecord {
            voucher: Address::generate(&env),
            stake: genuine_stake,
            vouch_timestamp,
            token: Address::generate(&env),
            expiry_timestamp: None,
            delegate: None,
            chain_id: None,
        });
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower.clone()), &vouches);

        let genuine_score = calculate_vouching_score(0, Some(&env), Some(&borrower));

        // Compare with a Sybil ring of 100 micro-vouches (below stake floor)
        let mut sybil_vouches: soroban_sdk::Vec<VouchRecord> = soroban_sdk::Vec::new(&env);
        for _ in 0..100u32 {
            sybil_vouches.push_back(VouchRecord {
                voucher: Address::generate(&env),
                stake: 100_000, // 0.01 XLM — below 0.1 XLM floor
                vouch_timestamp: now - 7 * 24 * 60 * 60, // old enough but under-stake
                token: Address::generate(&env),
                expiry_timestamp: None,
                delegate: None,
                chain_id: None,
            });
        }
        env.storage()
            .persistent()
            .set(&DataKey::Vouches(borrower2.clone()), &sybil_vouches);

        let sybil_score = calculate_vouching_score(0, Some(&env), Some(&borrower2));

        assert!(
            genuine_score > sybil_score,
            "Genuine 10 XLM voucher (score={}) should beat 100 micro-vouchers (score={})",
            genuine_score,
            sybil_score
        );
    });
}

// ── vouch_reputation_weight ──────────────────────────────────────────────────

#[test]
fn test_vouch_rep_weight_baseline_no_history() {
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, QuorumCreditContract);
    let voucher = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // No VoucherStats stored → base multiplier 1× (BPS_DENOMINATOR)
        let weight = vouch_reputation_weight(&env, &voucher);
        assert_eq!(weight, crate::types::BPS_DENOMINATOR, "No history → 1× weight");
    });
}

#[test]
fn test_vouch_rep_weight_below_min_yield_floor() {
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, QuorumCreditContract);
    let voucher = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // Set VoucherStats with yield below SYBIL_MIN_STAKE_FOR_REP (1_000_000 stroops = 0.1 XLM)
        let stats = crate::types::VoucherStats {
            successful_vouches: 20,
            total_vouches_slashed: 0,
            total_yield_earned: SYBIL_MIN_STAKE_FOR_REP - 1,
            total_slashed: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::VoucherStats(voucher.clone()), &stats);

        let weight = vouch_reputation_weight(&env, &voucher);
        // With successful_vouches > 0 but yield < floor, legacy path gives modest boost
        assert!(
            weight > crate::types::BPS_DENOMINATOR,
            "Legacy path should give modest boost, got {}",
            weight
        );
        assert!(
            weight <= 2 * crate::types::BPS_DENOMINATOR,
            "Weight should be ≤ 2×, got {}",
            weight
        );
    });
}

#[test]
fn test_vouch_rep_weight_substantial_yield() {
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, QuorumCreditContract);
    let voucher = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // Substantial yield: 10 XLM yield = 100_000_000 stroops (way above floor)
        let stats = crate::types::VoucherStats {
            successful_vouches: 0,
            total_vouches_slashed: 0,
            total_yield_earned: 100_000_000,
            total_slashed: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::VoucherStats(voucher.clone()), &stats);

        let weight = vouch_reputation_weight(&env, &voucher);
        // stake_time_units = 100_000_000 / 1000 = 100_000
        // sqrt(100_000) ≈ 316, capped at SYBIL_REP_SATURATION = 200
        // weight_bps = 200 * 10_000 / 200 = 10_000
        // final = BPS_DENOMINATOR + 10_000 = 20_000 (2× max)
        assert_eq!(
            weight,
            2 * crate::types::BPS_DENOMINATOR,
            "Very high yield should reach max 2× multiplier, got {}",
            weight
        );
    });
}

#[test]
fn test_vouch_rep_weight_slash_penalty_reduces_bonus() {
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, QuorumCreditContract);
    let voucher = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // High yield but 1 slash
        let stats = crate::types::VoucherStats {
            successful_vouches: 0,
            total_vouches_slashed: 1,
            total_yield_earned: 100_000_000, // Would normally give 2× (10_000 bps bonus)
            total_slashed: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::VoucherStats(voucher.clone()), &stats);

        let weight = vouch_reputation_weight(&env, &voucher);
        // weight_bps = 10_000 (at saturation)
        // penalty = 1 * 10_000 / 5 = 2_000 bps (20%)
        // final weight_bps = 10_000 - 2_000 = 8_000
        // total = BPS_DENOMINATOR + 8_000 = 18_000
        let expected = crate::types::BPS_DENOMINATOR + 8_000;
        assert_eq!(
            weight, expected,
            "1 slash should reduce bonus by 20%, expected {}, got {}",
            expected, weight
        );
    });
}

// ── Before/After attack cost measurement ─────────────────────────────────────

/// Simulate the "before" attack cost:
/// Old logic: each successful_vouch adds 500 bps → 20 accounts × tiny loans = 2× multiplier.
/// The raw count made 20 trivial vouches equivalent to one genuine voucher.
#[test]
fn test_before_attack_sybil_ring_raw_count_achieves_max_weight() {
    // Simulate what the old code would have done:
    // successful_vouches = 20 → weight_bps = min(20*500, 10000) = 10000 → 2× multiplier
    // The old code applied no stake or time floor.
    let old_rep_score = 20u32; // 20 tiny successful vouches
    let old_weight_bps = (old_rep_score as i128 * 500).min(10_000);
    let old_multiplier = crate::types::BPS_DENOMINATOR + old_weight_bps;
    assert_eq!(
        old_multiplier,
        2 * crate::types::BPS_DENOMINATOR,
        "BEFORE: 20 micro-vouches gave 2× max multiplier (Sybil-vulnerable)"
    );
}

/// Simulate the "after" attack cost:
/// New logic: 20 zero-yield vouches → effective_yield = 0 → base 1× only.
/// Attacker needs massive yield to reach 2×.
#[test]
fn test_after_attack_sybil_ring_zero_yield_gets_base_weight() {
    use crate::QuorumCreditContract;

    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, QuorumCreditContract);
    let attacker = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // Attacker has 20 successful vouches but zero yield (micro-loans = zero yield)
        let stats = crate::types::VoucherStats {
            successful_vouches: 20,
            total_vouches_slashed: 0,
            total_yield_earned: 0,
            total_slashed: 0,
        };
        env.storage()
            .persistent()
            .set(&DataKey::VoucherStats(attacker.clone()), &stats);

        let weight = vouch_reputation_weight(&env, &attacker);
        // Zero yield, zero count → base multiplier only
        assert_eq!(
            weight,
            crate::types::BPS_DENOMINATOR,
            "AFTER: Sybil ring with 20 micro-vouches but zero yield → base 1× weight"
        );
    });
}

/// Verify that to reach 2× the new design requires genuine capital commitment.
#[test]
fn test_attack_cost_to_reach_max_weight_requires_real_capital() {
    // To reach 2× under the new design:
    // weight_bps = 10_000 requires sqrt_val = SYBIL_REP_SATURATION = 200
    // sqrt_val = sqrt(effective_yield / 1_000) = 200
    // → effective_yield / 1_000 = 200² = 40_000
    // → effective_yield = 40_000_000 stroops = 4 XLM in YIELD alone
    //
    // At 2% yield rate that requires:
    // stake = effective_yield / 0.02 = 4 XLM / 0.02 = 200 XLM staked
    //
    // That's a significant real capital requirement vs the old trivial 20 micro-cycles.

    let required_yield_for_max = (SYBIL_REP_SATURATION as u64).pow(2) * 1_000;
    let required_stake_at_2pct = required_yield_for_max * 10_000 / 200; // divide by yield bps

    // Assert the requirement is substantial (at least 100 XLM = 1_000_000_000 stroops in stake)
    assert!(
        required_stake_at_2pct >= 1_000_000_000,
        "Attack cost to reach 2× weight should require ≥ 100 XLM stake, \
         actual required stake = {} stroops",
        required_stake_at_2pct
    );
}
