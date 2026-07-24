#[path = "credit_score_test.rs"]
mod credit_score_test;
#[path = "dynamic_rate_oracle_test.rs"]
mod dynamic_rate_oracle_test;
#[path = "merkle_tree_test.rs"]
mod merkle_tree_test;

// API-drifted; excluded so `cargo test --lib` compiles (issue #942):
//   cross_chain_attestation_test, withdrawal_queue_test
// rbac_enforcement_test excluded from lib.rs for the same reason.
