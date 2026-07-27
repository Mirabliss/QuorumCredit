use crate::errors::ContractError;
use crate::types::{
    Config, DataKey, LoanRecord, LoanStatus, PauseMode, ThawState,
    COMPOUND_RATE_BPS, MILESTONE_25_DISCOUNT_BPS, MILESTONE_25_PCT_PERMILLE,
    MILESTONE_50_DISCOUNT_BPS, MILESTONE_50_PCT_PERMILLE, MILESTONE_75_DISCOUNT_BPS,
    MILESTONE_75_PCT_PERMILLE, MILESTONE_FLAG_25, MILESTONE_FLAG_50, MILESTONE_FLAG_75, MIN_DYNAMIC_SLASH_BPS, MAX_DYNAMIC_SLASH_BPS, HEALTH_THRESHOLD_BPS,
};
use soroban_sdk::{token, Address, Env, String, Symbol, Vec};

// ── Reentrancy Guard ──────────────────────────────────────────────────────────

/// Acquires the reentrancy lock. Returns `Err(Reentrancy)` if already locked.
/// Must be paired with `release_lock` at the end of every state-mutating function.
pub fn acquire_lock(env: &Env) -> Result<(), ContractError> {
    let locked: bool = env
        .storage()
        .instance()
        .get(&DataKey::Locked)
        .unwrap_or(false);
    if locked {
        return Err(ContractError::Reentrancy);
    }
    env.storage().instance().set(&DataKey::Locked, &true);
    Ok(())
}

/// Releases the reentrancy lock. Always call this before returning from a guarded function.
pub fn release_lock(env: &Env) {
    env.storage().instance().set(&DataKey::Locked, &false);
}

// ── Pause Check ───────────────────────────────────────────────────────────────

/// Returns the current [`PauseMode`], automatically transitioning from `Thawing`
/// to `None` if the thaw window has expired.
pub fn get_pause_mode(env: &Env) -> PauseMode {
    let mode: PauseMode = env
        .storage()
        .instance()
        .get(&DataKey::PauseMode)
        .unwrap_or(PauseMode::None);

    if mode == PauseMode::Thawing {
        // Auto-transition: if thaw window has elapsed, clear thaw state.
        if let Some(thaw) = env
            .storage()
            .instance()
            .get::<_, ThawState>(&DataKey::ThawState)
        {
            let now = env.ledger().timestamp();
            if now >= thaw.thaw_start_timestamp + thaw.thaw_duration {
                env.storage()
                    .instance()
                    .set(&DataKey::PauseMode, &PauseMode::None);
                env.storage()
                    .instance()
                    .set(&DataKey::Paused, &false);
                env.storage()
                    .instance()
                    .remove(&DataKey::ThawState);
                return PauseMode::None;
            }
        }
    }

    mode
}

/// Blocks all write operations when `Paused`, `Thawing`, or emergency-paused.
/// This is the standard guard — call it in every state-mutating function.
pub fn require_not_paused(env: &Env) -> Result<(), ContractError> {
    let cfg = config(env);
    if cfg.emergency_pause_enabled {
        return Err(ContractError::ContractPaused);
    }

    match get_pause_mode(env) {
        PauseMode::Paused => Err(ContractError::ContractPaused),
        PauseMode::Thawing => Err(ContractError::ContractThawing),
        PauseMode::None => Ok(()),
    }
}

/// Lenient guard for read-friendly operations (e.g., `withdraw_vouch`).
/// Blocks when `Paused` or emergency-paused, but passes through during `Thawing`.
pub fn require_reads_allowed(env: &Env) -> Result<(), ContractError> {
    let cfg = config(env);
    if cfg.emergency_pause_enabled {
        return Err(ContractError::ContractPaused);
    }

    match get_pause_mode(env) {
        PauseMode::Paused => Err(ContractError::ContractPaused),
        PauseMode::Thawing | PauseMode::None => Ok(()),
    }
}

/// Alias kept for call-sites already updated to use the explicit thaw-blocking name.
/// Identical to `require_not_paused`.
#[inline]
pub fn require_not_thawing(env: &Env) -> Result<(), ContractError> {
    require_not_paused(env)
}

pub fn require_positive_amount(_env: &Env, amount: i128) -> Result<(), ContractError> {
    if amount <= 0 {
        return Err(ContractError::InvalidAmount);
    }
    Ok(())
}

/// Validates that `timestamp` is non-zero and not in the past relative to `now`.
/// Pass `now = env.ledger().timestamp()` for the current ledger time.
/// Returns `Err(InvalidAmount)` if the timestamp is zero or already expired.
pub fn validate_timestamp(_env: &Env, timestamp: u64, now: u64) -> Result<(), ContractError> {
    if timestamp == 0 || timestamp <= now {
        return Err(ContractError::InvalidAmount);
    }
    Ok(())
}

// ── Config & Loan Helpers ─────────────────────────────────────────────────────

pub fn config(env: &Env) -> Config {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .expect("not initialized")
}

pub fn get_admins(env: &Env) -> Vec<Address> {
    config(env).admins
}

pub fn has_active_loan(env: &Env, borrower: &Address) -> bool {
    matches!(
        get_active_loan_record(env, borrower),
        Ok(loan) if loan.status == LoanStatus::Active
    )
}

pub fn get_active_loan_record(env: &Env, borrower: &Address) -> Result<LoanRecord, ContractError> {
    let loan_id: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::ActiveLoan(borrower.clone()))
        .ok_or(ContractError::NoActiveLoan)?;
    env.storage()
        .persistent()
        .get(&DataKey::Loan(loan_id))
        .ok_or(ContractError::NoActiveLoan)
}

pub fn get_latest_loan_record(env: &Env, borrower: &Address) -> Option<LoanRecord> {
    if let Some(loan_id) = env
        .storage()
        .persistent()
        .get(&DataKey::LatestLoan(borrower.clone()))
    {
        env.storage().persistent().get(&DataKey::Loan(loan_id))
    } else {
        None
    }
}

pub fn next_loan_id(env: &Env) -> u64 {
    let loan_id = env
        .storage()
        .persistent()
        .get(&DataKey::LoanCounter)
        .unwrap_or(0u64)
        .checked_add(1)
        .expect("loan ID overflow");
    env.storage()
        .persistent()
        .set(&DataKey::LoanCounter, &loan_id);
    loan_id
}

pub fn add_slash_balance(env: &Env, amount: i128) {
    let current: i128 = env
        .storage()
        .instance()
        .get(&DataKey::SlashTreasury)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&DataKey::SlashTreasury, &(current + amount));
}

pub fn deduct_slash_balance(env: &Env, amount: i128) -> Result<(), crate::errors::ContractError> {
    let current: i128 = env
        .storage()
        .instance()
        .get(&DataKey::SlashTreasury)
        .unwrap_or(0);
    if current < amount {
        return Err(crate::errors::ContractError::PoolInsufficientFunds);
    }
    env.storage()
        .instance()
        .set(&DataKey::SlashTreasury, &(current - amount));
    Ok(())
}

pub fn is_zero_address(env: &Env, addr: &Address) -> bool {
    let zero_account = Address::from_string(&String::from_str(
        env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    let zero_contract = Address::from_string(&String::from_str(
        env,
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
    ));
    addr == &zero_account || addr == &zero_contract
}

pub fn require_valid_address(env: &Env, addr: &Address) -> Result<(), ContractError> {
    if is_zero_address(env, addr) {
        Err(ContractError::ZeroAddress)
    } else {
        Ok(())
    }
}

pub fn require_valid_token(env: &Env, addr: &Address) -> Result<(), ContractError> {
    require_valid_address(env, addr)?;
    let client = token::Client::new(env, addr);
    let probe = env.current_contract_address();
    if client.try_balance(&probe).is_err() {
        return Err(ContractError::InvalidToken);
    }
    Ok(())
}

pub fn validate_admin_config(
    env: &Env,
    admins: &Vec<Address>,
    admin_threshold: u32,
    admin_whitelist: &Vec<Address>,
    admin_blacklist: &Vec<Address>,
) -> Result<(), ContractError> {
    if admins.is_empty() {
        return Err(ContractError::InvalidAdminThreshold);
    }
    if admin_threshold == 0 || admin_threshold > admins.len() {
        return Err(ContractError::InvalidAdminThreshold);
    }
    for i in 0..admin_whitelist.len() {
        let allowed = admin_whitelist.get(i).unwrap();
        require_valid_address(env, &allowed)?;
        for j in 0..i {
            let prior = admin_whitelist.get(j).unwrap();
            if allowed == prior {
                return Err(ContractError::InvalidAdminThreshold);
            }
        }
    }
    for i in 0..admin_blacklist.len() {
        let blocked = admin_blacklist.get(i).unwrap();
        require_valid_address(env, &blocked)?;
        for j in 0..i {
            let prior = admin_blacklist.get(j).unwrap();
            if blocked == prior {
                return Err(ContractError::InvalidAdminThreshold);
            }
        }
        if admin_whitelist.iter().any(|a| a == blocked) {
            return Err(ContractError::InvalidAdminThreshold);
        }
    }
    let admin_count = admins.len();
    for i in 0..admin_count {
        let admin = admins.get(i).unwrap();
        require_valid_address(env, &admin)?;
        for j in 0..i {
            let prior_admin = admins.get(j).unwrap();
            if admin == prior_admin {
                return Err(ContractError::InvalidAdminThreshold);
            }
        }
        if !admin_whitelist.is_empty()
            && !admin_whitelist.iter().any(|allowed| allowed == admin)
        {
            return Err(ContractError::AdminNotWhitelisted);
        }
        if admin_blacklist.iter().any(|blocked| blocked == admin) {
            return Err(ContractError::AdminBlacklisted);
        }
    }
    Ok(())
}

pub fn require_admin_approval(env: &Env, admin_signers: &Vec<Address>) {
    let cfg = config(env);
    assert!(
        admin_signers.len() >= cfg.admin_threshold,
        "insufficient admin approvals"
    );
    for signer in admin_signers.iter() {
        assert!(
            cfg.admins.iter().any(|a| a == signer),
            "signer is not a registered admin"
        );
        // Reject any signer whose key has been emergency-revoked
        let revoked: bool = env
            .storage()
            .persistent()
            .get(&DataKey::RevokedAdmin(signer.clone()))
            .unwrap_or(false);
        assert!(!revoked, "signer has been emergency-revoked");
        signer.require_auth();
    }
}

/// Issue #893: Require admin approval for a specific operation type with multi-tier thresholds.
/// Different operation types can require different numbers of approvals based on criticality.
pub fn require_admin_approval_for_operation(
    env: &Env,
    admin_signers: &Vec<Address>,
    operation_type: crate::types::AdminOperationType,
) {
    
    
    let cfg = config(env);
    
    // Determine the required threshold based on operation type
    let required_threshold = if let Some(multi_tier) = env.storage().instance().get::<crate::types::DataKey, crate::types::MultiTierAdminThresholds>(&crate::types::DataKey::MultiTierAdminThresholds) {
        multi_tier.get_threshold(operation_type)
    } else {
        // Fall back to single threshold if multi-tier not configured
        cfg.admin_threshold
    };

    assert!(
        admin_signers.len() >= required_threshold,
        "insufficient admin approvals for this operation type"
    );

    for signer in admin_signers.iter() {
        assert!(
            cfg.admins.iter().any(|a| a == signer),
            "signer is not a registered admin"
        );
        signer.require_auth();
    }
}

pub fn is_admin(env: &Env, addr: &Address) -> bool {
    config(env).admins.iter().any(|a| a == *addr)
}

/// Governance participant: registered admin or holder of the protocol token.
pub fn is_governance_participant(env: &Env, addr: &Address) -> bool {
    if is_admin(env, addr) {
        return true;
    }
    let cfg = config(env);
    let token = token::Client::new(env, &cfg.token);
    token.balance(addr) > 0
}

pub fn require_governance_participant(env: &Env, addr: &Address) -> Result<(), ContractError> {
    if is_governance_participant(env, addr) {
        Ok(())
    } else {
        Err(ContractError::NotGovernanceParticipant)
    }
}

pub fn require_allowed_token<'a>(
    env: &'a Env,
    addr: &Address,
) -> Result<token::Client<'a>, ContractError> {
    let cfg = config(env);
    if *addr == cfg.token || cfg.allowed_tokens.iter().any(|t| t == *addr) {
        Ok(token::Client::new(env, addr))
    } else {
        Err(ContractError::InvalidToken)
    }
}

pub fn loan_status(env: &Env, borrower: &Address) -> LoanStatus {
    if let Ok(loan) = get_active_loan_record(env, borrower) {
        return loan.status;
    }
    if let Some(loan) = get_latest_loan_record(env, borrower) {
        return loan.status;
    }
    LoanStatus::None
}

/// Compute the effective slash threshold considering dynamic adjustment.
/// When `Config.dynamic_slash_threshold` is false, returns `Config.slash_bps` unchanged.
/// When true, lowers the threshold proportionally when protocol health ≥ `HEALTH_THRESHOLD_BPS`
/// and raises it when health is poor, clamped to `[MIN_DYNAMIC_SLASH_BPS, MAX_DYNAMIC_SLASH_BPS]`.
pub fn calculate_dynamic_slash_threshold(env: &Env) -> i128 {
    let cfg = config(env);
    if !cfg.dynamic_slash_threshold {
        return cfg.slash_bps;
    }

    let health = calculate_protocol_health_score(env);
    if health >= HEALTH_THRESHOLD_BPS {
        cfg.slash_bps.max(MIN_DYNAMIC_SLASH_BPS)
    } else {
        let adjustment = (HEALTH_THRESHOLD_BPS - health) * (MAX_DYNAMIC_SLASH_BPS - MIN_DYNAMIC_SLASH_BPS) / HEALTH_THRESHOLD_BPS;
        (cfg.slash_bps + adjustment).clamp(MIN_DYNAMIC_SLASH_BPS, MAX_DYNAMIC_SLASH_BPS)
    }
}

/// Protocol health score in basis points (0–10000).
/// Factors: whether config exists (30%), not paused (30%), yield reserve solvency (40%).
pub fn calculate_protocol_health_score(env: &Env) -> i128 {
    let mut score: i128 = 0;

    // 30%: initialized
    if env.storage().instance().has(&DataKey::Config) {
        score += 3_000;
    }

    // 30%: not paused
    let cfg = config(env);
    if get_pause_mode(env) == PauseMode::None && !cfg.emergency_pause_enabled {
        score += 3_000;
    }

    // 40%: yield reserve solvent (non-zero balance)
    let reserve: i128 = env.storage().instance().get(&DataKey::YieldReserve).unwrap_or(0);
    if reserve > 0 {
        score += 4_000;
    }

    score
}

/// Register `borrower` in the global `BorrowerList` if not already present.
pub fn register_borrower_if_needed(env: &Env, borrower: &Address) {
    use soroban_sdk::Vec as SdkVec;
    let mut list: SdkVec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::BorrowerList)
        .unwrap_or(SdkVec::new(env));

    if !list.iter().any(|b| &b == borrower) {
        list.push_back(borrower.clone());
        env.storage().persistent().set(&DataKey::BorrowerList, &list);
    }
}

// ── Pagination (Issue #1146) ──────────────────────────────────────────────────

/// Slice `items[offset..offset+limit]` out of an unbounded persistent-storage
/// `Vec`, capping `limit` to `MAX_PAGE_SIZE`, so a single call's cost is
/// bounded by the page size rather than the collection's total length.
///
/// Returns the page plus `next_cursor`: `Some(offset)` to pass on the next
/// call, or `None` once the end of `items` has been reached.
pub fn paginate_vec<T>(env: &Env, items: &Vec<T>, offset: u32, limit: u32) -> (Vec<T>, Option<u32>)
where
    T: Clone + soroban_sdk::TryFromVal<Env, soroban_sdk::Val> + soroban_sdk::IntoVal<Env, soroban_sdk::Val>,
{
    let capped_limit = limit.clamp(1, crate::types::MAX_PAGE_SIZE);
    let len = items.len();
    let mut page = Vec::new(env);

    if offset >= len {
        return (page, None);
    }

    let end = (offset + capped_limit).min(len);
    for i in offset..end {
        page.push_back(items.get(i).unwrap());
    }

    let next_cursor = if end < len { Some(end) } else { None };
    (page, next_cursor)
}

/// Total number of borrowers ever registered (Issue #1146). Lets an operator
/// cross-check an off-chain indexer's derived address set against ground
/// truth on-chain without reading the full unbounded `BorrowerList`.
pub fn get_borrower_count(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get::<DataKey, Vec<Address>>(&DataKey::BorrowerList)
        .map(|l| l.len())
        .unwrap_or(0)
}

/// Paginated read of the global `BorrowerList` (Issue #1146).
pub fn get_borrower_list_page(env: &Env, offset: u32, limit: u32) -> (Vec<Address>, Option<u32>) {
    let list: Vec<Address> = env
        .storage()
        .persistent()
        .get(&DataKey::BorrowerList)
        .unwrap_or(Vec::new(env));
    paginate_vec(env, &list, offset, limit)
}

pub fn primary_token(env: &Env) -> token::Client<'_> {
    token::Client::new(env, &config(env).token)
}

pub fn token(env: &Env) -> token::Client<'_> {
    primary_token(env)
}

pub fn token_client(env: &Env) -> token::Client<'_> {
    primary_token(env)
}

// ── Rate Limiting ─────────────────────────────────────────────────────────────

pub fn check_rate_limit(env: &Env, account: &Address) -> Result<(), ContractError> {
    let cfg = config(env);
    if !cfg.rate_limit_config.enabled {
        return Ok(());
    }

    let now = env.ledger().timestamp();
    let (last_window_start, count): (u64, u32) = env
        .storage()
        .persistent()
        .get(&DataKey::RateLimit(account.clone()))
        .unwrap_or((0, 0));

    if now < last_window_start + cfg.rate_limit_config.window_secs {
        if count >= cfg.rate_limit_config.max_calls {
            return Err(ContractError::RateLimitExceeded);
        }
        env.storage().persistent().set(
            &DataKey::RateLimit(account.clone()),
            &(last_window_start, count + 1),
        );
    } else {
        env.storage()
            .persistent()
            .set(&DataKey::RateLimit(account.clone()), &(now, 1));
    }

    Ok(())
}

// ── Access Control ────────────────────────────────────────────────────────────

pub fn check_permission(
    env: &Env,
    account: &Address,
    permission_fn: fn(&crate::types::RolePermissions) -> bool,
) -> Result<(), ContractError> {
    // Admins always have all permissions
    if is_admin(env, account) {
        return Ok(());
    }

    let permissions: crate::types::RolePermissions = match env
        .storage()
        .persistent()
        .get(&DataKey::RolePermissions(account.clone()))
    {
        Some(p) => p,
        // No permissions record: default-allow (RBAC is opt-in, not opt-out)
        None => return Ok(()),
    };

    if permission_fn(&permissions) {
        Ok(())
    } else {
        Err(ContractError::PermissionDenied)
    }
}

// ── Issue #63: Signature Replay Protection ────────────────────────────────────

/// Verifies `nonce` is strictly greater than the last consumed nonce for `caller`,
/// then records it. Returns `Err(ReplayAttackDetected)` if the nonce has already
/// been consumed.
///
/// Callers must include `nonce` in the signed payload to prevent replays.
pub fn verify_and_consume_nonce(
    env: &Env,
    caller: &Address,
    nonce: u64,
) -> Result<(), ContractError> {
    let last: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::Nonce(caller.clone()))
        .unwrap_or(0);
    if nonce <= last {
        return Err(ContractError::ReplayAttackDetected);
    }
    env.storage()
        .persistent()
        .set(&DataKey::Nonce(caller.clone()), &nonce);
    Ok(())
}

/// Returns the next expected nonce for `caller` (last consumed + 1).
pub fn next_nonce(env: &Env, caller: &Address) -> u64 {
    let last: u64 = env
        .storage()
        .persistent()
        .get(&DataKey::Nonce(caller.clone()))
        .unwrap_or(0);
    last + 1
}

// ── Issue #64: Oracle Price Staleness ─────────────────────────────────────────

/// Validates that an oracle price record is fresh (within `ORACLE_PRICE_MAX_AGE_SECS`).
/// Returns `Err(AttestationExpired)` if the price is stale.
pub fn validate_price_freshness(
    env: &Env,
    price_record: &crate::types::OraclePriceRecord,
) -> Result<(), ContractError> {
    let now = env.ledger().timestamp();
    if now.saturating_sub(price_record.recorded_at) > crate::types::ORACLE_PRICE_MAX_AGE_SECS {
        return Err(ContractError::AttestationExpired);
    }
    Ok(())
}

// ── Daily-Compound Interest ───────────────────────────────────────────────────

/// Calculate compound interest that has accrued over `days_elapsed` whole days
/// on `outstanding_principal` at the annual rate `COMPOUND_RATE_BPS`.
///
/// Formula (all integer arithmetic, truncating):
/// ```text
/// interest_per_day = outstanding_principal * COMPOUND_RATE_BPS / 10_000 / 365
/// total            = interest_per_day * days_elapsed
/// ```
///
/// Notes:
/// - Returns 0 when `days_elapsed == 0` (same-day calls never double-charge).
/// - Returns 0 when `outstanding_principal <= 0` (defensive guard).
/// - The result is always ≥ 0.
pub fn calculate_daily_compound_interest(
    outstanding_principal: i128,
    days_elapsed: u64,
) -> i128 {
    if outstanding_principal <= 0 || days_elapsed == 0 {
        return 0;
    }
    // interest_per_day = principal * COMPOUND_RATE_BPS / (10_000 * 365)
    // We compute it as two separate divisions to stay within i128 for realistic
    // Soroban loan sizes (principal ≤ 10^18 stroops is safe).
    let daily_interest = outstanding_principal
        .checked_mul(COMPOUND_RATE_BPS)
        .unwrap_or(0)
        / 10_000
        / 365;

    daily_interest
        .checked_mul(days_elapsed as i128)
        .unwrap_or(0)
}

// ── Milestone Bonus Application ───────────────────────────────────────────────

/// Check whether any new repayment milestone has been crossed by the borrower
/// and, if so, apply a one-time discount to `accrued_interest` (floor 0).
///
/// Milestones are checked in ascending order (25 % → 50 % → 75 %).  Each fires
/// at most once per loan, tracked by the `milestone_bonus_applied` bitmask on
/// the `LoanRecord`.
///
/// `amount_repaid_after_payment` is the *new* cumulative repaid amount after
/// the current payment is applied.  `total_obligation` is the full amount owed
/// (principal + static yield; compound interest is handled separately).
///
/// Returns the updated `(accrued_interest, milestone_bonus_applied)`.
pub fn apply_milestone_bonus(
    loan: &LoanRecord,
    amount_repaid_after_payment: i128,
    total_obligation: i128,
) -> (i128, u32) {
    if total_obligation <= 0 {
        return (loan.accrued_interest, loan.milestone_bonus_applied);
    }

    let mut accrued = loan.accrued_interest;
    let mut flags = loan.milestone_bonus_applied;

    // Compute the repaid fraction in per-mille (‰) to avoid floating-point.
    // repaid_permille = amount_repaid_after_payment * 1000 / total_obligation
    let repaid_permille: u32 = (amount_repaid_after_payment
        .saturating_mul(1_000)
        / total_obligation)
        .max(0) as u32;

    // 75 % milestone — must be applied before 50 % / 25 % to avoid applying
    // lower-tier discounts to an already-reduced balance.
    if repaid_permille >= MILESTONE_75_PCT_PERMILLE && (flags & MILESTONE_FLAG_75 == 0) {
        let discount = accrued
            .saturating_mul(MILESTONE_75_DISCOUNT_BPS)
            / 10_000;
        accrued = (accrued - discount).max(0);
        flags |= MILESTONE_FLAG_75;
    }

    // 50 % milestone
    if repaid_permille >= MILESTONE_50_PCT_PERMILLE && (flags & MILESTONE_FLAG_50 == 0) {
        let discount = accrued
            .saturating_mul(MILESTONE_50_DISCOUNT_BPS)
            / 10_000;
        accrued = (accrued - discount).max(0);
        flags |= MILESTONE_FLAG_50;
    }

    // 25 % milestone
    if repaid_permille >= MILESTONE_25_PCT_PERMILLE && (flags & MILESTONE_FLAG_25 == 0) {
        let discount = accrued
            .saturating_mul(MILESTONE_25_DISCOUNT_BPS)
            / 10_000;
        accrued = (accrued - discount).max(0);
        flags |= MILESTONE_FLAG_25;
    }

    (accrued, flags)
}

/// Retrieves an oracle price record and validates its freshness in one call.
pub fn get_fresh_price(
    env: &Env,
    key: soroban_sdk::Symbol,
) -> Result<crate::types::OraclePriceRecord, ContractError> {
    let record: crate::types::OraclePriceRecord = env
        .storage()
        .persistent()
        .get(&DataKey::OraclePrice(key))
        .ok_or(ContractError::OracleUnauthorized)?;
    validate_price_freshness(env, &record)?;
    Ok(record)
}

/// Called by the registered oracle (see `verify_repayment` for the same pattern)
/// to publish a fresh price for `key`. Overwrites any previous record for that key.
pub fn set_oracle_price(
    env: &Env,
    oracle: &Address,
    key: Symbol,
    price: i128,
) -> Result<(), ContractError> {
    oracle.require_auth();

    let cfg = config(env);
    let registered = cfg.oracle_address.ok_or(ContractError::OracleUnauthorized)?;
    if *oracle != registered {
        return Err(ContractError::OracleUnauthorized);
    }

    let record = crate::types::OraclePriceRecord {
        price,
        recorded_at: env.ledger().timestamp(),
    };
    env.storage()
        .persistent()
        .set(&DataKey::OraclePrice(key), &record);
    Ok(())
}

// ── Issue #65: Graduated Response / Tiered Lockdown ───────────────────────────

/// Returns the current [`ThreatLevel`] (defaults to `Normal`).
pub fn get_threat_level(env: &Env) -> crate::types::ThreatLevel {
    env.storage()
        .persistent()
        .get(&DataKey::ThreatLevelKey)
        .unwrap_or(crate::types::ThreatLevel::Normal)
}

/// Sets the protocol threat level. Requires admin approval.
pub fn set_threat_level(
    env: &Env,
    admin_signers: &soroban_sdk::Vec<Address>,
    level: crate::types::ThreatLevel,
) {
    require_admin_approval(env, admin_signers);
    env.storage()
        .persistent()
        .set(&DataKey::ThreatLevelKey, &level);
}

/// Enforces graduated response restrictions for a write operation.
///
/// - `Normal`   → allowed
/// - `Elevated` → blocks new vouches/loans (callers pass `is_new_activity = true`)
/// - `Critical` → blocks all writes
/// - `Lockdown` → blocks all writes
pub fn require_graduated_response(
    env: &Env,
    is_new_activity: bool,
) -> Result<(), ContractError> {
    match get_threat_level(env) {
        crate::types::ThreatLevel::Normal => Ok(()),
        crate::types::ThreatLevel::Elevated => {
            if is_new_activity {
                Err(ContractError::ContractPaused)
            } else {
                Ok(())
            }
        }
        crate::types::ThreatLevel::Lockdown => {
            Err(ContractError::ContractPaused)
        }
        crate::types::ThreatLevel::Critical => {
            Err(ContractError::ContractPaused)
        }
    }
}
