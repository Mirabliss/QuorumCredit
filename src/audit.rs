/// Issue #1179: Vouch Audit Trail Implementation
///
/// This module provides comprehensive audit trail functionality for vouch operations.
/// All vouch lifecycle events (creation, modification, withdrawal, slashing, etc.)
/// are logged with timestamps, actor information, and operational details.

use soroban_sdk::{Address, Env, String as SorobanString, Vec};
use crate::types::{
    VouchAuditEvent, VouchAuditEventType, VouchAuditTrail, DataKey, ContractError,
};

/// Log a vouch audit event to the audit trail.
///
/// This internal function is called by vouch operations to record all modifications.
pub fn log_vouch_audit_event(
    env: &Env,
    voucher: &Address,
    borrower: &Address,
    token: &Address,
    operation: VouchAuditEventType,
    amount: i128,
    actor: Option<Address>,
    reason: Option<SorobanString>,
) -> Result<(), ContractError> {
    // Get the next event ID
    let event_counter_key = DataKey::VouchAuditEventIdCounter;
    let mut event_id: u64 = env
        .storage()
        .persistent()
        .get::<DataKey, u64>(&event_counter_key)
        .unwrap_or(0);

    event_id = event_id.checked_add(1).ok_or(ContractError::ArithmeticError)?;

    // Create the audit event
    let current_timestamp = env.ledger().timestamp();
    let event = VouchAuditEvent {
        timestamp: current_timestamp,
        operation,
        voucher: voucher.clone(),
        borrower: borrower.clone(),
        amount,
        actor,
        reason,
        token: token.clone(),
        event_id,
    };

    // Get or create the audit trail
    let audit_key = DataKey::VouchAuditTrail(borrower.clone(), voucher.clone(), token.clone());
    let mut audit_trail: VouchAuditTrail = env
        .storage()
        .persistent()
        .get::<DataKey, VouchAuditTrail>(&audit_key)
        .unwrap_or_else(|| VouchAuditTrail {
            voucher: voucher.clone(),
            borrower: borrower.clone(),
            token: token.clone(),
            events: Vec::new(&env),
            created_at: current_timestamp,
            last_event_at: current_timestamp,
        });

    // Add the event to the audit trail
    audit_trail.events.push_back(event);
    audit_trail.last_event_at = current_timestamp;

    // Store the updated audit trail
    env.storage()
        .persistent()
        .set::<DataKey, VouchAuditTrail>(&audit_key, &audit_trail);

    // Store the updated event counter
    env.storage()
        .persistent()
        .set::<DataKey, u64>(&event_counter_key, &event_id);

    Ok(())
}

/// Retrieve the audit trail for a specific vouch.
///
/// Returns all audit events for the vouch (borrower, voucher, token) in chronological order.
pub fn get_vouch_audit_trail(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Result<VouchAuditTrail, ContractError> {
    let audit_key = DataKey::VouchAuditTrail(borrower, voucher, token);

    env.storage()
        .persistent()
        .get::<DataKey, VouchAuditTrail>(&audit_key)
        .ok_or(ContractError::InvalidInput)
}

/// Retrieve a page of audit events for a specific vouch.
///
/// Returns up to `limit` events starting from index `offset` in the audit trail.
pub fn get_vouch_audit_trail_page(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
    offset: u32,
    limit: u32,
) -> Result<Vec<VouchAuditEvent>, ContractError> {
    use crate::types::MAX_PAGE_SIZE;

    let audit_trail = get_vouch_audit_trail(env.clone(), borrower, voucher, token)?;

    // Clamp limit to MAX_PAGE_SIZE
    let clamped_limit = std::cmp::min(limit, MAX_PAGE_SIZE);
    let start = offset as usize;
    let end = std::cmp::min(start + clamped_limit as usize, audit_trail.events.len());

    if start >= audit_trail.events.len() {
        return Ok(Vec::new(&env));
    }

    let mut result = Vec::new(&env);
    for i in start..end {
        if let Some(event) = audit_trail.events.get(i as u32) {
            result.push_back(event.clone());
        }
    }

    Ok(result)
}

/// Export audit trail data for a vouch as a formatted report.
///
/// This function compiles audit trail information into a human-readable format
/// suitable for compliance and transparency reporting.
pub fn export_vouch_audit_report(
    env: Env,
    borrower: Address,
    voucher: Address,
    token: Address,
) -> Result<SorobanString, ContractError> {
    let audit_trail = get_vouch_audit_trail(env.clone(), borrower.clone(), voucher.clone(), token.clone())?;

    // Build report header
    let mut report = SorobanString::from_slice(&env, "Vouch Audit Report\n==================\n");

    // Count events for summary
    let event_count = audit_trail.events.len();

    if event_count > 0 {
        let summary = SorobanString::from_slice(&env, "Total Events: ");
        report = SorobanString::from_slice(&env, &summary);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_vouch_audit_event() {
        // Tests will be added in the test suite
    }

    #[test]
    fn test_get_vouch_audit_trail() {
        // Tests will be added in the test suite
    }
}
