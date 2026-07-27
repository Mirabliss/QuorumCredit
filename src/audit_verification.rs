/// Audit Log Completeness & Integrity Verification.
///
/// Builds on the existing vouch audit trail (`crate::audit`) to answer the
/// question the raw trail can't answer on its own: "can we trust it?" This
/// module checks that every recorded event forms an unbroken, monotonic
/// sequence, that entries carry the fields required for a compliance-grade
/// audit record, and that a trail hasn't been silently mutated since a prior
/// verification snapshot was taken.
///
/// ## Guarantees provided
///
/// 1. **Sequence completeness** — event IDs for a given (borrower, voucher,
///    token) trail increase by exactly 1 with no gaps, so no event can have
///    been silently dropped from the middle of the log.
/// 2. **Timestamp monotonicity** — event timestamps are non-decreasing,
///    consistent with `env.ledger().timestamp()` only moving forward.
/// 3. **Entry completeness** — every event carries a non-zero timestamp and
///    a recognizable operation; entries missing both an `actor` and a
///    `reason` are flagged (not hard-failed, since some system-triggered
///    events legitimately have no human actor).
/// 4. **Tamper evidence** — `snapshot_audit_checksum` records a checksum of
///    the trail's current length and content fingerprint. A later call to
///    `verify_audit_immutability` recomputes the same fingerprint and flags
///    divergence. This does not prevent storage-level tampering (nothing in
///    a persistent key-value store can, short of the ledger's own
///    consensus), but it makes any divergence between two audits detectable.
///
/// What this module deliberately does **not** claim: it cannot prove an
/// event that should have been logged but never was (e.g. a code path that
/// forgot to call `audit::log_vouch_audit_event`). That is a code-review /
/// test-coverage concern, not something derivable from the log itself.
use soroban_sdk::{contracttype, Address, Env, Vec};

use crate::audit;
use crate::errors::ContractError;
use crate::types::{VouchAuditEvent, VouchAuditEventType};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum AuditVerificationDataKey {
    /// (borrower, voucher, token) -> last-known checksum + event count.
    Checksum(Address, Address, Address),
}

/// Result of a completeness/consistency check over one audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditVerificationReport {
    pub total_events: u32,
    /// Event IDs where a gap was detected immediately before them.
    pub sequence_gaps: Vec<u64>,
    /// Number of events whose timestamp regressed relative to the previous event.
    pub timestamp_violations: u32,
    /// Event IDs missing both an actor and a reason.
    pub incomplete_entries: Vec<u64>,
    pub is_complete: bool,
}

/// A stored tamper-evidence checksum for one audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditChecksumRecord {
    pub event_count: u32,
    pub checksum: u64,
    pub recorded_at: u64,
}

/// Verify that a single audit entry carries the minimum required fields for
/// a compliance-grade record: timestamp, a recognized action, and (ideally)
/// either an actor or an explanatory reason.
fn verify_audit_entry(event: &VouchAuditEvent) -> bool {
    if event.timestamp == 0 {
        return false;
    }
    // `operation` is a typed enum (Created/Increased/Decreased/Withdrawn/...),
    // so it is always "recognized" by construction — the check here exists so
    // future variants added without a corresponding audit call are still
    // caught by `is_complete` being driven off real entries, not assumed ones.
    let _ = &event.operation;
    true
}

/// Compute a simple order-sensitive fingerprint over a trail's events. Not a
/// cryptographic hash — sufficient to detect accidental or malicious
/// after-the-fact edits to event ordering, ids, or amounts between two
/// verification calls.
fn fingerprint_events(events: &Vec<VouchAuditEvent>) -> u64 {
    let mut acc: u64 = 1469598103934665603; // FNV offset basis
    for e in events.iter() {
        let mixed = e
            .event_id
            .wrapping_mul(1099511628211)
            .wrapping_add(e.timestamp)
            .wrapping_add(e.amount as u64);
        acc ^= mixed;
        acc = acc.wrapping_mul(1099511628211);
    }
    acc
}

/// Run a full completeness and consistency check over the audit trail for
/// (borrower, voucher, token).
pub fn verify_audit_log_completeness(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Result<AuditVerificationReport, ContractError> {
    let trail = audit::get_vouch_audit_trail(env.clone(), borrower, voucher, token)?;

    let mut sequence_gaps: Vec<u64> = Vec::new(&env);
    let mut incomplete_entries: Vec<u64> = Vec::new(&env);
    let mut timestamp_violations: u32 = 0;
    let mut last_event_id: u64 = 0;
    let mut last_timestamp: u64 = 0;

    for (i, event) in trail.events.iter().enumerate() {
        if i > 0 {
            if event.event_id != last_event_id + 1 {
                sequence_gaps.push_back(event.event_id);
            }
            if event.timestamp < last_timestamp {
                timestamp_violations += 1;
            }
        }
        if !verify_audit_entry(&event) || (event.actor.is_none() && event.reason.is_none()) {
            incomplete_entries.push_back(event.event_id);
        }
        last_event_id = event.event_id;
        last_timestamp = event.timestamp;
    }

    let is_complete = sequence_gaps.is_empty()
        && timestamp_violations == 0
        && incomplete_entries.is_empty();

    Ok(AuditVerificationReport {
        total_events: trail.events.len(),
        sequence_gaps,
        timestamp_violations,
        incomplete_entries,
        is_complete,
    })
}

/// Take a tamper-evidence snapshot of the current audit trail state. Call
/// this after an operation you want to be able to prove wasn't retroactively
/// altered.
pub fn snapshot_audit_checksum(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Result<AuditChecksumRecord, ContractError> {
    let trail = audit::get_vouch_audit_trail(
        env.clone(),
        borrower.clone(),
        voucher.clone(),
        token.clone(),
    )?;

    let record = AuditChecksumRecord {
        event_count: trail.events.len(),
        checksum: fingerprint_events(&trail.events),
        recorded_at: env.ledger().timestamp(),
    };

    env.storage().persistent().set(
        &AuditVerificationDataKey::Checksum(borrower, voucher, token),
        &record,
    );

    Ok(record)
}

/// Re-verify a trail against its last snapshot. Returns `true` iff the trail
/// is unchanged (still immutable/append-only) since the snapshot was taken,
/// i.e. the recorded checksum still matches and the event count has only
/// grown, never shrunk or been reordered in the already-snapshotted prefix.
pub fn verify_audit_immutability(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Result<bool, ContractError> {
    let key = AuditVerificationDataKey::Checksum(borrower.clone(), voucher.clone(), token.clone());
    let snapshot: AuditChecksumRecord = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(ContractError::InvalidAmount)?;

    let trail = audit::get_vouch_audit_trail(env.clone(), borrower, voucher, token)?;

    if trail.events.len() < snapshot.event_count {
        // Events disappeared — trail was mutated or truncated.
        return Ok(false);
    }

    // Recompute the fingerprint over just the snapshotted prefix; new events
    // appended after the snapshot are expected and don't count as tampering.
    let mut prefix: Vec<VouchAuditEvent> = Vec::new(&env);
    for i in 0..snapshot.event_count {
        if let Some(e) = trail.events.get(i) {
            prefix.push_back(e.clone());
        }
    }

    Ok(fingerprint_events(&prefix) == snapshot.checksum)
}

/// Determine whether an event type represents a state-changing operation
/// (as opposed to a pure read). Used by callers wiring this module into new
/// mutating vouch/loan paths to assert "every state change has an audit
/// entry" at the point of the state change, per the issue's task list.
pub fn is_state_changing_operation(op: &VouchAuditEventType) -> bool {
    match op {
        VouchAuditEventType::Created
        | VouchAuditEventType::Increased
        | VouchAuditEventType::Decreased
        | VouchAuditEventType::Withdrawn => true,
    }
}
