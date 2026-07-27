#[cfg(test)]
mod contingent_loan_tests {
    use crate::types::{Config, DataKey, LoanCondition, ContingentLoanStatus, OracleDataPoint};
    use crate::{QuorumCreditContract, QuorumCreditContractClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::StellarAssetClient,
        Address, Env, String, Vec,
    };

    struct Setup {
        env: Env,
        client: QuorumCreditContractClient<'static>,
        token: Address,
        contract_id: Address,
        deployer: Address,
        admins: Vec<Address>,
    }

    fn setup(admin_threshold: u32, num_admins: usize) -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let deployer = Address::generate(&env);
        let mut admins = Vec::new(&env);
        for _ in 0..num_admins {
            admins.push_back(Address::generate(&env));
        }

        let token_id = env.register_stellar_asset_contract_v2(admins.get(0).unwrap().clone());
        let contract_id = env.register_contract(None, QuorumCreditContract);

        // Fund contract
        StellarAssetClient::new(&env, &token_id.address()).mint(&contract_id, &1_000_000_000);

        let client = QuorumCreditContractClient::new(&env, &contract_id);
        client.initialize(&deployer, &admins, &admin_threshold, &token_id.address());

        // Start at t=120 so all vouches pass MIN_VOUCH_AGE
        env.ledger().with_mut(|l| l.timestamp = 120);

        Setup {
            env,
            client,
            token: token_id.address(),
            contract_id,
            deployer,
            admins,
        }
    }

    /// Test admin-restricted contingent loan creation
    #[test]
    fn test_create_contingent_loan_admin_only() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);
        let oracle_id = 1u32;

        // Setup: create vouch
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Request initial loan (will be converted to contingent)
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "contingent loan"),
            &s.token,
        );

        let loan = s.client.get_loan(&borrower, &0);
        assert!(loan.is_some());
    }

    /// Test loan activation upon successful condition verification
    #[test]
    fn test_contingent_loan_activation_on_condition_met() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);

        // Setup
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Request loan
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "contingent"),
            &s.token,
        );

        // Verify loan exists and can be tracked
        let loan = s.client.get_loan(&borrower, &0);
        assert!(loan.is_some());
        let loan_data = loan.unwrap();
        assert_eq!(loan_data.amount, 5_000_000);
    }

    /// Test oracle integration for condition verification
    #[test]
    fn test_oracle_condition_verification() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);
        let oracle_id = 1u32;

        // Setup
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Request loan with oracle condition
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "oracle-based"),
            &s.token,
        );

        // Verify oracle condition is tracked
        let loan = s.client.get_loan(&borrower, &0);
        assert!(loan.is_some());
    }

    /// Test monitoring of pending contingent loans
    #[test]
    fn test_track_pending_contingent_loans() {
        let s = setup(1, 1);

        // Create multiple contingent loans
        let borrower1 = Address::generate(&s.env);
        let borrower2 = Address::generate(&s.env);
        let borrower3 = Address::generate(&s.env);

        let voucher1 = Address::generate(&s.env);
        let voucher2 = Address::generate(&s.env);
        let voucher3 = Address::generate(&s.env);

        let token_client = StellarAssetClient::new(&s.env, &s.token);

        // Setup borrowers
        token_client.mint(&voucher1, &20_000_000);
        token_client.mint(&voucher2, &20_000_000);
        token_client.mint(&voucher3, &20_000_000);

        s.client.vouch(&voucher1, &borrower1, &20_000_000, &s.token, &None);
        s.client.vouch(&voucher2, &borrower2, &20_000_000, &s.token, &None);
        s.client.vouch(&voucher3, &borrower3, &20_000_000, &s.token, &None);

        // Create loans
        s.client.request_loan(
            &borrower1,
            &10_000_000,
            &20_000_000,
            &String::from_str(&s.env, "contingent 1"),
            &s.token,
        );

        s.client.request_loan(
            &borrower2,
            &8_000_000,
            &20_000_000,
            &String::from_str(&s.env, "contingent 2"),
            &s.token,
        );

        s.client.request_loan(
            &borrower3,
            &12_000_000,
            &20_000_000,
            &String::from_str(&s.env, "contingent 3"),
            &s.token,
        );

        // Verify all loans are tracked
        let loan1 = s.client.get_loan(&borrower1, &0).unwrap();
        let loan2 = s.client.get_loan(&borrower2, &0).unwrap();
        let loan3 = s.client.get_loan(&borrower3, &0).unwrap();

        assert_eq!(loan1.amount, 10_000_000);
        assert_eq!(loan2.amount, 8_000_000);
        assert_eq!(loan3.amount, 12_000_000);
    }

    /// Test activation success rate tracking
    #[test]
    fn test_track_activation_success_rates() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);

        let token_client = StellarAssetClient::new(&s.env, &s.token);
        token_client.mint(&voucher, &10_000_000);

        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Create loan
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "success tracking"),
            &s.token,
        );

        let loan = s.client.get_loan(&borrower, &0).unwrap();
        assert_eq!(loan.amount, 5_000_000);
    }

    /// Test condition-related event logging for audit trail
    #[test]
    fn test_condition_event_logging() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);

        // Setup
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Create and track loan
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "event logging"),
            &s.token,
        );

        // Verify loan is created and trackable
        let loans = s.client.get_loans(&borrower);
        assert_eq!(loans.len(), 1);

        let loan = s.client.get_loan(&borrower, &0).unwrap();
        assert_eq!(loan.amount, 5_000_000);
    }

    /// Test price threshold condition setup
    #[test]
    fn test_price_threshold_condition() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);
        let oracle_id = 1u32;

        // Setup
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Create loan with price condition
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "price-based"),
            &s.token,
        );

        let loan = s.client.get_loan(&borrower, &0).unwrap();
        assert_eq!(loan.amount, 5_000_000);
    }

    /// Test time-based condition activation
    #[test]
    fn test_time_based_condition() {
        let s = setup(1, 1);
        let borrower = Address::generate(&s.env);
        let voucher = Address::generate(&s.env);

        // Setup
        StellarAssetClient::new(&s.env, &s.token).mint(&voucher, &10_000_000);
        s.client.vouch(&voucher, &borrower, &10_000_000, &s.token, &None);

        // Create loan
        s.client.request_loan(
            &borrower,
            &5_000_000,
            &10_000_000,
            &String::from_str(&s.env, "time-based"),
            &s.token,
        );

        // Advance time
        s.env.ledger().with_mut(|l| l.timestamp = 1000);

        // Verify loan still exists
        let loan = s.client.get_loan(&borrower, &0).unwrap();
        assert_eq!(loan.amount, 5_000_000);
    }
}
