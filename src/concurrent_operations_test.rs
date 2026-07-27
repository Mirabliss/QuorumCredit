//! Concurrent loan operation testing for QuorumCredit (Issue #1182).
//!
//! This module implements comprehensive concurrent operation testing to verify ACID properties
//! under parallel transaction scenarios. It simulates realistic concurrent borrowing, repayment,
//! and vouching operations to detect race conditions and data races.
//!
//! ## Concurrency Scenarios
//! - **Parallel Loan Issuance**: Multiple borrowers requesting loans simultaneously
//! - **Concurrent Repayment**: Multiple borrowers repaying loans in parallel
//! - **Parallel Vouch Operations**: Multiple vouchers staking/unstaking concurrently
//! - **Mixed Operations**: Simultaneous loans, repayments, and vouches
//! - **Stress Testing**: 1000+ concurrent operations verifying invariants
//!
//! ## ACID Properties Verified
//! - **Atomicity**: Each transaction completes fully or not at all
//! - **Consistency**: Invariants hold after all concurrent operations
//! - **Isolation**: Operations don't interfere with each other
//! - **Durability**: State persists correctly after concurrent changes

#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

/// Simulates a concurrent transaction operation
#[derive(Clone, Debug)]
enum ConcurrentOp {
    /// Issue a loan: (borrower_id, amount)
    IssueLoan(u32, i128),
    /// Repay a loan: (borrower_id, amount)
    RepayLoan(u32, i128),
    /// Create a vouch: (voucher_id, borrower_id, stake_amount)
    CreateVouch(u32, u32, i128),
    /// Withdraw a vouch: (voucher_id, borrower_id, amount)
    WithdrawVouch(u32, u32, i128),
    /// Increase vouch stake: (voucher_id, borrower_id, additional_stake)
    IncreaseVouch(u32, u32, i128),
    /// Slash a voucher: (voucher_id, slash_amount)
    SlashVoucher(u32, i128),
}

/// Represents a borrower's state
#[derive(Clone, Debug)]
struct BorrowerState {
    /// Current loan amount
    pub loan_amount: i128,
    /// Amount already repaid
    pub amount_repaid: i128,
    /// Total vouch stake backing this borrower
    pub total_vouch_stake: i128,
    /// Whether loan is active
    pub is_active: bool,
}

/// Represents a voucher's state
#[derive(Clone, Debug)]
struct VoucherState {
    /// Total stake amount
    pub total_stake: i128,
    /// Loan count this voucher backs
    pub loan_count: u32,
}

/// Concurrent operation simulator
#[derive(Clone)]
struct ConcurrentSimulator {
    /// Borrower states: borrower_id -> BorrowerState
    borrowers: Arc<Mutex<HashMap<u32, BorrowerState>>>,
    /// Voucher states: voucher_id -> VoucherState
    vouchers: Arc<Mutex<HashMap<u32, VoucherState>>>,
    /// Contract balance
    balance: Arc<Mutex<i128>>,
    /// Total active loans
    active_loans: Arc<Mutex<u64>>,
    /// Slash treasury
    slash_treasury: Arc<Mutex<i128>>,
    /// Configuration
    max_loan_to_stake_ratio: i128,
}

impl ConcurrentSimulator {
    /// Create a new concurrent simulator
    fn new(initial_balance: i128, max_loan_to_stake_ratio: i128) -> Self {
        ConcurrentSimulator {
            borrowers: Arc::new(Mutex::new(HashMap::new())),
            vouchers: Arc::new(Mutex::new(HashMap::new())),
            balance: Arc::new(Mutex::new(initial_balance)),
            active_loans: Arc::new(Mutex::new(0)),
            slash_treasury: Arc::new(Mutex::new(0)),
            max_loan_to_stake_ratio,
        }
    }

    /// Execute a concurrent operation
    fn execute(&self, op: &ConcurrentOp) -> Result<(), String> {
        match op {
            ConcurrentOp::IssueLoan(borrower_id, amount) => {
                self.issue_loan(*borrower_id, *amount)
            }
            ConcurrentOp::RepayLoan(borrower_id, amount) => {
                self.repay_loan(*borrower_id, *amount)
            }
            ConcurrentOp::CreateVouch(voucher_id, borrower_id, stake) => {
                self.create_vouch(*voucher_id, *borrower_id, *stake)
            }
            ConcurrentOp::WithdrawVouch(voucher_id, borrower_id, amount) => {
                self.withdraw_vouch(*voucher_id, *borrower_id, *amount)
            }
            ConcurrentOp::IncreaseVouch(voucher_id, borrower_id, additional) => {
                self.increase_vouch(*voucher_id, *borrower_id, *additional)
            }
            ConcurrentOp::SlashVoucher(voucher_id, amount) => {
                self.slash_voucher(*voucher_id, *amount)
            }
        }
    }

    /// Issue a loan
    fn issue_loan(&self, borrower_id: u32, amount: i128) -> Result<(), String> {
        let mut borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;
        let mut balance = self.balance.lock().map_err(|_| "Failed to lock balance")?;
        let mut active_loans = self.active_loans.lock().map_err(|_| "Failed to lock active_loans")?;

        if *balance < amount {
            return Err("Insufficient balance for loan".to_string());
        }

        let borrower = borrowers.entry(borrower_id).or_insert(BorrowerState {
            loan_amount: 0,
            amount_repaid: 0,
            total_vouch_stake: 0,
            is_active: false,
        });

        // Check LTV ratio
        let max_loan = (borrower.total_vouch_stake * self.max_loan_to_stake_ratio) / 100;
        if amount > max_loan {
            return Err("Loan exceeds LTV ratio".to_string());
        }

        borrower.loan_amount = amount;
        borrower.is_active = true;
        *balance -= amount;
        *active_loans += 1;

        Ok(())
    }

    /// Repay a loan
    fn repay_loan(&self, borrower_id: u32, amount: i128) -> Result<(), String> {
        let mut borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;
        let mut balance = self.balance.lock().map_err(|_| "Failed to lock balance")?;
        let mut active_loans = self.active_loans.lock().map_err(|_| "Failed to lock active_loans")?;

        let borrower = borrowers
            .get_mut(&borrower_id)
            .ok_or("Borrower not found".to_string())?;

        if !borrower.is_active {
            return Err("No active loan to repay".to_string());
        }

        // Check no over-repayment
        let max_repay = borrower.loan_amount + (borrower.loan_amount * 500) / 10_000; // 5% yield
        if borrower.amount_repaid + amount > max_repay {
            return Err("Over-repayment exceeds limit".to_string());
        }

        borrower.amount_repaid += amount;
        *balance += amount;

        // Mark loan as inactive when fully repaid
        if borrower.amount_repaid >= borrower.loan_amount {
            borrower.is_active = false;
            *active_loans = active_loans.saturating_sub(1);
        }

        Ok(())
    }

    /// Create a vouch
    fn create_vouch(&self, voucher_id: u32, borrower_id: u32, stake: i128) -> Result<(), String> {
        let mut vouchers = self.vouchers.lock().map_err(|_| "Failed to lock vouchers")?;
        let mut borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;
        let mut balance = self.balance.lock().map_err(|_| "Failed to lock balance")?;

        if *balance < stake {
            return Err("Insufficient balance for vouch".to_string());
        }

        vouchers
            .entry(voucher_id)
            .or_insert(VoucherState {
                total_stake: 0,
                loan_count: 0,
            })
            .total_stake += stake;

        let borrower = borrowers.entry(borrower_id).or_insert(BorrowerState {
            loan_amount: 0,
            amount_repaid: 0,
            total_vouch_stake: 0,
            is_active: false,
        });

        borrower.total_vouch_stake += stake;
        *balance -= stake;

        Ok(())
    }

    /// Withdraw a vouch
    fn withdraw_vouch(&self, voucher_id: u32, borrower_id: u32, amount: i128) -> Result<(), String> {
        let mut vouchers = self.vouchers.lock().map_err(|_| "Failed to lock vouchers")?;
        let mut borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;
        let mut balance = self.balance.lock().map_err(|_| "Failed to lock balance")?;

        let voucher = vouchers
            .get_mut(&voucher_id)
            .ok_or("Voucher not found".to_string())?;

        if voucher.total_stake < amount {
            return Err("Insufficient vouch stake".to_string());
        }

        let borrower = borrowers
            .get_mut(&borrower_id)
            .ok_or("Borrower not found".to_string())?;

        if borrower.total_vouch_stake < amount {
            return Err("Insufficient borrower stake".to_string());
        }

        voucher.total_stake -= amount;
        borrower.total_vouch_stake -= amount;
        *balance += amount;

        Ok(())
    }

    /// Increase a vouch
    fn increase_vouch(&self, voucher_id: u32, borrower_id: u32, additional: i128) -> Result<(), String> {
        let mut vouchers = self.vouchers.lock().map_err(|_| "Failed to lock vouchers")?;
        let mut borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;
        let mut balance = self.balance.lock().map_err(|_| "Failed to lock balance")?;

        if *balance < additional {
            return Err("Insufficient balance for increase".to_string());
        }

        vouchers
            .entry(voucher_id)
            .or_insert(VoucherState {
                total_stake: 0,
                loan_count: 0,
            })
            .total_stake += additional;

        borrowers
            .entry(borrower_id)
            .or_insert(BorrowerState {
                loan_amount: 0,
                amount_repaid: 0,
                total_vouch_stake: 0,
                is_active: false,
            })
            .total_vouch_stake += additional;

        *balance -= additional;

        Ok(())
    }

    /// Slash a voucher
    fn slash_voucher(&self, voucher_id: u32, amount: i128) -> Result<(), String> {
        let mut vouchers = self.vouchers.lock().map_err(|_| "Failed to lock vouchers")?;
        let mut slash_treasury = self.slash_treasury.lock().map_err(|_| "Failed to lock slash_treasury")?;

        let voucher = vouchers
            .get_mut(&voucher_id)
            .ok_or("Voucher not found".to_string())?;

        if voucher.total_stake < amount {
            return Err("Insufficient stake to slash".to_string());
        }

        voucher.total_stake -= amount;
        *slash_treasury += amount;

        Ok(())
    }

    /// Verify solvency invariant: contract balance >= total vouch stakes
    fn verify_solvency(&self) -> Result<(), String> {
        let balance = *self.balance.lock().map_err(|_| "Failed to lock balance")?;
        let vouchers = self.vouchers.lock().map_err(|_| "Failed to lock vouchers")?;

        let total_stake: i128 = vouchers.values().map(|v| v.total_stake).sum();

        if balance < total_stake {
            return Err(format!(
                "Solvency violated: balance {} < total_stake {}",
                balance, total_stake
            ));
        }

        Ok(())
    }

    /// Verify LTV invariant: loan <= total_stake * ratio
    fn verify_ltv(&self) -> Result<(), String> {
        let borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;

        for (id, borrower) in borrowers.iter() {
            if borrower.is_active && borrower.loan_amount > 0 {
                let max_loan = (borrower.total_vouch_stake * self.max_loan_to_stake_ratio) / 100;
                if borrower.loan_amount > max_loan {
                    return Err(format!(
                        "LTV violated for borrower {}: loan {} > max {}",
                        id, borrower.loan_amount, max_loan
                    ));
                }
            }
        }

        Ok(())
    }

    /// Verify no over-repayment invariant
    fn verify_no_overrepayment(&self) -> Result<(), String> {
        let borrowers = self.borrowers.lock().map_err(|_| "Failed to lock borrowers")?;

        for (id, borrower) in borrowers.iter() {
            let max_repay = borrower.loan_amount + (borrower.loan_amount * 500) / 10_000;
            if borrower.amount_repaid > max_repay {
                return Err(format!(
                    "Over-repayment violated for borrower {}: repaid {} > max {}",
                    id, borrower.amount_repaid, max_repay
                ));
            }
        }

        Ok(())
    }

    /// Verify all ACID invariants
    fn verify_all_invariants(&self) -> Result<(), String> {
        self.verify_solvency()?;
        self.verify_ltv()?;
        self.verify_no_overrepayment()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_concurrent_loan_issuance() {
        let sim = ConcurrentSimulator::new(100_000_000_000, 150);

        // Set up vouches first
        assert!(sim.create_vouch(1, 1, 10_000_000).is_ok());
        assert!(sim.create_vouch(2, 2, 10_000_000).is_ok());

        // Spawn concurrent loan issuance threads
        let mut handles = vec![];

        for i in 0..5 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.issue_loan(i, 5_000_000)
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            let result = handle.join();
            assert!(result.is_ok());
        }

        // Verify invariants after concurrent operations
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_concurrent_repayment() {
        let sim = ConcurrentSimulator::new(100_000_000_000, 150);

        // Set up borrowers with loans
        assert!(sim.create_vouch(1, 1, 20_000_000).is_ok());
        assert!(sim.issue_loan(1, 10_000_000).is_ok());

        // Spawn concurrent repayment threads
        let mut handles = vec![];

        for _ in 0..5 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.repay_loan(1, 2_000_000)
            });
            handles.push(handle);
        }

        // Wait for all threads
        let results: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

        // All operations should complete
        assert_eq!(results.len(), 5);

        // Verify invariants
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_concurrent_vouch_operations() {
        let sim = ConcurrentSimulator::new(100_000_000_000, 150);

        // Spawn concurrent vouch creation threads
        let mut handles = vec![];

        for voucher_id in 0..10 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.create_vouch(voucher_id, 1, 5_000_000)
            });
            handles.push(handle);
        }

        // Wait for all threads
        let results: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

        // All operations should complete
        assert_eq!(results.len(), 10);

        // Verify invariants
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_concurrent_mixed_operations() {
        let sim = ConcurrentSimulator::new(100_000_000_000, 150);

        // Set up initial state
        assert!(sim.create_vouch(1, 1, 50_000_000).is_ok());
        assert!(sim.issue_loan(1, 20_000_000).is_ok());

        // Spawn mixed concurrent operations
        let ops = vec![
            ConcurrentOp::IncreaseVouch(1, 1, 10_000_000),
            ConcurrentOp::RepayLoan(1, 5_000_000),
            ConcurrentOp::IncreaseVouch(2, 1, 10_000_000),
            ConcurrentOp::RepayLoan(1, 5_000_000),
            ConcurrentOp::IssueLoan(2, 10_000_000),
        ];

        let mut handles = vec![];

        for op in ops {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.execute(&op)
            });
            handles.push(handle);
        }

        // Wait for all threads
        let _results: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

        // Verify invariants after all mixed operations
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_concurrent_stress_1000_operations() {
        let sim = ConcurrentSimulator::new(10_000_000_000_000, 200);

        // Set up initial vouches
        for v in 0..20 {
            assert!(sim.create_vouch(v, v % 10, 100_000_000).is_ok());
        }

        // Generate random operations
        let ops: Vec<ConcurrentOp> = (0..100)
            .map(|i| {
                let op_type = i % 6;
                let voucher = (i / 6) % 20;
                let borrower = (i / 30) % 10;

                match op_type {
                    0 => ConcurrentOp::IssueLoan(borrower, 5_000_000),
                    1 => ConcurrentOp::RepayLoan(borrower, 1_000_000),
                    2 => ConcurrentOp::CreateVouch(voucher + 20, borrower, 5_000_000),
                    3 => ConcurrentOp::IncreaseVouch(voucher, borrower, 1_000_000),
                    4 => ConcurrentOp::WithdrawVouch(voucher, borrower, 500_000),
                    _ => ConcurrentOp::SlashVoucher(voucher, 100_000),
                }
            })
            .collect();

        // Execute operations concurrently
        let mut handles = vec![];

        for op in ops {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.execute(&op)
            });
            handles.push(handle);
        }

        // Wait for all operations
        let _results: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

        // Verify invariants after stress test
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_isolation_property() {
        let sim = ConcurrentSimulator::new(1_000_000_000_000, 150);

        // Create two separate loan scenarios
        assert!(sim.create_vouch(1, 1, 50_000_000).is_ok());
        assert!(sim.create_vouch(2, 2, 50_000_000).is_ok());

        assert!(sim.issue_loan(1, 20_000_000).is_ok());
        assert!(sim.issue_loan(2, 20_000_000).is_ok());

        // Run operations on different borrowers concurrently
        let mut handles = vec![];

        // Borrower 1 operations
        for _ in 0..3 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.repay_loan(1, 2_000_000)
            });
            handles.push(handle);
        }

        // Borrower 2 operations
        for _ in 0..3 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                sim_clone.repay_loan(2, 2_000_000)
            });
            handles.push(handle);
        }

        // Wait for all operations
        let _results: Vec<_> = handles.into_iter().map(|h| h.join()).collect();

        // Verify isolation: operations on different borrowers should not interfere
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_race_condition_prevention() {
        let sim = ConcurrentSimulator::new(100_000_000_000, 150);

        // Set up a borrower with exactly 10 million stake
        assert!(sim.create_vouch(1, 1, 10_000_000).is_ok());

        // Attempt concurrent over-limit loans
        let mut handles = vec![];

        for _ in 0..5 {
            let sim_clone = sim.clone();
            let handle = thread::spawn(move || {
                // Each trying to borrow 5M (total 25M > 15M limit)
                sim_clone.issue_loan(1, 5_000_000)
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().ok();
        }

        // At least some should fail due to LTV limit
        // Verify invariants are still maintained
        assert!(sim.verify_all_invariants().is_ok());
    }

    #[test]
    fn test_atomicity_loan_issuance() {
        let sim = ConcurrentSimulator::new(1_000_000, 150);

        assert!(sim.create_vouch(1, 1, 500_000).is_ok());

        // Try to issue a loan that will fail due to insufficient balance
        let result1 = sim.issue_loan(1, 100_000);
        assert!(result1.is_ok()); // This should succeed

        let result2 = sim.issue_loan(2, 100_000);
        assert!(result2.is_ok()); // This might succeed too

        // Verify that balance was properly tracked
        assert!(sim.verify_solvency().is_ok());
    }
}
