//! Issue: Batch loan request "cart" system.
//!
//! Lets a borrower stage multiple prospective loan requests (different
//! amounts / tenures) before submitting them together, mirroring an
//! e-commerce cart. Submission applies a small volume discount for
//! borrowers batching 3+ requests, and abandonment (carts created but
//! never submitted) is tracked for product analytics.
//!
//! Note: the underlying protocol only allows a single *active* loan per
//! borrower (see `helpers::has_active_loan`), so batch submission below
//! processes cart items in order and surfaces a per-item result — later
//! items will naturally fail with `ActiveLoanExists` unless earlier items
//! failed to disburse. This module does not change that invariant; it
//! only orchestrates staging and sequential submission against it.

use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Vec};

/// Local storage keys for this module, kept separate from the shared
/// `DataKey` enum.
#[contracttype]
#[derive(Clone)]
pub enum CartKey {
    /// borrower -> LoanCart
    Cart(Address),
    /// Global cart funnel statistics.
    AbandonmentStats,
}

/// A single staged loan request within a borrower's cart.
#[contracttype]
#[derive(Clone)]
pub struct CartItem {
    pub amount: i128,
    /// Requested loan tenure, in seconds.
    pub tenure_secs: u64,
    pub added_at: u64,
}

/// A borrower's in-progress batch of staged loan requests.
#[contracttype]
#[derive(Clone)]
pub struct LoanCart {
    pub borrower: Address,
    pub items: Vec<CartItem>,
    pub created_at: u64,
    pub last_updated: u64,
    pub submitted: bool,
}

/// Outcome of submitting a single cart item as part of a batch request.
#[contracttype]
#[derive(Clone)]
pub struct BatchLoanRequestResult {
    pub item_index: u32,
    pub requested_amount: i128,
    /// Amount actually requested after any volume discount was applied.
    /// The discount reduces the requested principal slightly in exchange
    /// for batching, rather than adjusting rates, to keep this additive
    /// to the existing single-loan request path.
    pub discounted_amount: i128,
    pub tenure_secs: u64,
    pub success: bool,
    /// `ContractError` discriminant if the underlying request failed.
    pub error_code: Option<u32>,
}

/// Protocol-wide cart funnel statistics, used to measure abandonment.
#[contracttype]
#[derive(Clone)]
pub struct CartAbandonmentStats {
    pub carts_created: u32,
    pub carts_submitted: u32,
    pub items_added: u32,
    pub items_submitted: u32,
}

const VOLUME_DISCOUNT_THRESHOLD: u32 = 3;
const VOLUME_DISCOUNT_BPS: i128 = 100; // 1%

fn load_stats(env: &Env) -> CartAbandonmentStats {
    env.storage()
        .instance()
        .get(&CartKey::AbandonmentStats)
        .unwrap_or(CartAbandonmentStats {
            carts_created: 0,
            carts_submitted: 0,
            items_added: 0,
            items_submitted: 0,
        })
}

fn save_stats(env: &Env, stats: &CartAbandonmentStats) {
    env.storage()
        .instance()
        .set(&CartKey::AbandonmentStats, stats);
}

/// Stage a new loan request in the borrower's cart. Creates the cart if
/// this is the borrower's first staged item.
pub fn add_to_loan_cart(
    env: Env,
    borrower: Address,
    amount: i128,
    tenure_secs: u64,
) -> LoanCart {
    borrower.require_auth();
    assert!(amount > 0, "cart: amount must be positive");
    assert!(tenure_secs > 0, "cart: tenure must be positive");

    let now = env.ledger().timestamp();
    let mut stats = load_stats(&env);

    let mut cart: LoanCart = env
        .storage()
        .persistent()
        .get(&CartKey::Cart(borrower.clone()))
        .unwrap_or_else(|| {
            stats.carts_created += 1;
            LoanCart {
                borrower: borrower.clone(),
                items: Vec::new(&env),
                created_at: now,
                last_updated: now,
                submitted: false,
            }
        });

    cart.items.push_back(CartItem {
        amount,
        tenure_secs,
        added_at: now,
    });
    cart.last_updated = now;
    cart.submitted = false;

    stats.items_added += 1;
    save_stats(&env, &stats);

    env.storage()
        .persistent()
        .set(&CartKey::Cart(borrower.clone()), &cart);

    env.events().publish(
        (symbol_short!("cart"), symbol_short!("added")),
        (borrower, amount, tenure_secs),
    );

    cart
}

/// Read a borrower's current cart contents.
pub fn get_loan_cart(env: Env, borrower: Address) -> LoanCart {
    env.storage()
        .persistent()
        .get(&CartKey::Cart(borrower.clone()))
        .unwrap_or(LoanCart {
            borrower,
            items: Vec::new(&env),
            created_at: 0,
            last_updated: 0,
            submitted: false,
        })
}

/// Clear a borrower's cart without submitting it. Counts toward
/// abandonment statistics since the cart had items staged but never
/// resulted in a submission.
pub fn abandon_loan_cart(env: Env, borrower: Address) {
    borrower.require_auth();

    if let Some(cart) = env
        .storage()
        .persistent()
        .get::<CartKey, LoanCart>(&CartKey::Cart(borrower.clone()))
    {
        if !cart.submitted && cart.items.len() > 0 {
            env.events().publish(
                (symbol_short!("cart"), symbol_short!("abandon")),
                (borrower.clone(), cart.items.len()),
            );
        }
    }

    env.storage()
        .persistent()
        .remove(&CartKey::Cart(borrower.clone()));
}

/// Compute the discounted amount for a single item given the cart's total
/// size. Batches of 3 or more staged loans receive a 1% discount on each
/// item's requested principal.
fn apply_volume_discount(amount: i128, cart_size: u32) -> i128 {
    if cart_size >= VOLUME_DISCOUNT_THRESHOLD {
        amount - (amount * VOLUME_DISCOUNT_BPS / 10_000)
    } else {
        amount
    }
}

/// Submit every staged item in the borrower's cart as an individual loan
/// request, applying the volume discount when 3+ items are present.
/// Clears the cart afterward and records submission statistics.
///
/// Returns a per-item result so callers can see which requests succeeded;
/// see the module-level note about the single-active-loan invariant.
pub fn submit_batch_loan_request(
    env: Env,
    borrower: Address,
    loan_purpose: String,
    threshold: i128,
    token: Address,
) -> Vec<BatchLoanRequestResult> {
    borrower.require_auth();

    let cart: LoanCart = env
        .storage()
        .persistent()
        .get(&CartKey::Cart(borrower.clone()))
        .unwrap_or(LoanCart {
            borrower: borrower.clone(),
            items: Vec::new(&env),
            created_at: env.ledger().timestamp(),
            last_updated: env.ledger().timestamp(),
            submitted: false,
        });

    let cart_size = cart.items.len();
    let mut results: Vec<BatchLoanRequestResult> = Vec::new(&env);
    let mut stats = load_stats(&env);

    for i in 0..cart_size {
        let item = cart.items.get(i).unwrap();
        let discounted = apply_volume_discount(item.amount, cart_size);

        let outcome = crate::loan::request_loan(
            env.clone(),
            borrower.clone(),
            discounted,
            threshold,
            loan_purpose.clone(),
            token.clone(),
        );

        let (success, error_code) = match outcome {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e as u32)),
        };

        if success {
            stats.items_submitted += 1;
        }

        results.push_back(BatchLoanRequestResult {
            item_index: i,
            requested_amount: item.amount,
            discounted_amount: discounted,
            tenure_secs: item.tenure_secs,
            success,
            error_code,
        });
    }

    stats.carts_submitted += 1;
    save_stats(&env, &stats);

    env.storage().persistent().set(
        &CartKey::Cart(borrower.clone()),
        &LoanCart {
            borrower: borrower.clone(),
            items: Vec::new(&env),
            created_at: cart.created_at,
            last_updated: env.ledger().timestamp(),
            submitted: true,
        },
    );

    env.events().publish(
        (symbol_short!("cart"), symbol_short!("submit")),
        (borrower, cart_size),
    );

    results
}

/// Read protocol-wide cart funnel statistics (creation vs. submission vs.
/// abandonment), for dashboards and product analytics.
pub fn get_cart_abandonment_stats(env: Env) -> CartAbandonmentStats {
    load_stats(&env)
}

/// Convenience error used by wrapper entry points when a cart operation is
/// attempted with no cart present.
pub fn cart_exists(env: Env, borrower: Address) -> bool {
    env.storage()
        .persistent()
        .has(&CartKey::Cart(borrower))
}
