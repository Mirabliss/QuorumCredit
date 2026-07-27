//! # Distributed Tracing — Issue #1235
//!
//! Trace-ID propagation and sampling for QuorumCredit.
//!
//! Because Soroban runs in a deterministic, sandboxed WASM environment
//! without network access, **exporting** traces to Jaeger or Tempo must happen
//! off-chain.  This module provides:
//!
//! 1. **On-chain** — trace IDs and span metadata embedded into Soroban events
//!    so that off-chain indexers can reconstruct full request traces.
//! 2. **Sampling** — a configurable head-based sampler stored in contract
//!    storage (default: 100 % sample rate, adjustable per-operation).
//! 3. **Export structures** — typed Rust structs that mirror the OpenTelemetry
//!    span wire format, serialisable to JSON by the indexer.
//!
//! ## Trace propagation model
//!
//! ```text
//!  Client SDK / Server
//!       │  generates trace_id (128-bit, hex)
//!       │  generates root span_id (64-bit, hex)
//!       │
//!       ▼
//!  Soroban transaction
//!       │  contract function receives (trace_id, parent_span_id)
//!       │  contract emits TraceEvent with span metadata
//!       │
//!       ▼
//!  Indexer (tools/indexer/src/indexer.rs)
//!       │  reads TraceEvent rows
//!       │  POSTs OTLP spans to Jaeger/Tempo
//! ```
//!
//! ## Event schema
//!
//! Every traced call emits a `trace/span` Soroban event with the payload:
//!
//! ```json
//! {
//!   "trace_id":       "0123456789abcdef0123456789abcdef",
//!   "span_id":        "0123456789abcdef",
//!   "parent_span_id": "fedcba9876543210",   // null for root spans
//!   "operation":      "vouch",
//!   "status":         "ok",                 // "ok" | "error"
//!   "sampled":        true
//! }
//! ```
//!
//! ## Sampling
//!
//! Head-based sampling is configured per-operation via
//! [`set_trace_sample_rate`].  The sample decision is deterministic:
//!
//! ```text
//! sampled = hash(trace_id) % 10_000 < sample_rate_bps
//! ```

#![allow(unused)]

use soroban_sdk::{contracttype, symbol_short, Address, Bytes, Env, String, Symbol, Vec};

use crate::errors::ContractError;

// ── Constants ────────────────────────────────────────────────────────────────

/// Default sample rate: 100 % (10 000 bps).
pub const DEFAULT_SAMPLE_RATE_BPS: u32 = 10_000;
/// Maximum trace-ID length (32 hex chars = 128 bits).
pub const TRACE_ID_LEN: u32 = 32;
/// Maximum span-ID length (16 hex chars = 64 bits).
pub const SPAN_ID_LEN: u32 = 16;
/// Soroban event topic for trace spans.
pub const TRACE_EVENT_TOPIC: &str = "trace/span";

// ── Data types ───────────────────────────────────────────────────────────────

/// Span status — mirrors OpenTelemetry SpanStatus.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpanStatus {
    /// The operation completed successfully.
    Ok,
    /// The operation encountered an error.
    Error,
    /// Status not yet set.
    Unset,
}

/// Metadata attached to a single traced operation span.
///
/// This struct is emitted as a Soroban event so that off-chain indexers
/// can reconstruct full distributed traces.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TraceSpan {
    /// 128-bit trace ID as a 32-character lowercase hex string.
    pub trace_id: String,
    /// 64-bit span ID as a 16-character lowercase hex string.
    pub span_id: String,
    /// Parent span ID, or empty string for a root span.
    pub parent_span_id: String,
    /// Name of the operation being traced (e.g., `"vouch"`, `"request_loan"`).
    pub operation: String,
    /// Outcome of the operation.
    pub status: SpanStatus,
    /// Whether this span was sampled (i.e., should be exported).
    pub sampled: bool,
    /// Ledger sequence number at which this span was recorded.
    pub ledger: u32,
}

/// Trace context carried across component boundaries.
///
/// Passed by callers that want to continue an existing trace rather
/// than start a new root span.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TraceContext {
    /// 128-bit trace ID (32-char hex).
    pub trace_id: String,
    /// The immediate parent's span ID (16-char hex).
    pub parent_span_id: String,
}

/// Per-operation sampling configuration stored in contract storage.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TraceSamplingConfig {
    /// Operation name this config applies to.
    pub operation: String,
    /// Sample rate in basis points (0 = never sample; 10 000 = always sample).
    pub sample_rate_bps: u32,
}

/// Storage key for tracing configuration.
#[contracttype]
pub enum TracingKey {
    /// Sampling config keyed by operation name string.
    SampleRate(String),
    /// Global fallback sample rate (bps).
    GlobalSampleRate,
}

// ── Sampling logic ───────────────────────────────────────────────────────────

/// Determine whether a trace identified by `trace_id` should be sampled for
/// operation `operation`, using head-based sampling.
///
/// The sample decision is:
/// ```text
/// hash(trace_id_bytes) % 10_000 < effective_rate_bps
/// ```
/// This is deterministic: re-evaluating on the same `trace_id` always gives
/// the same answer, which is the key property needed for consistent sampling
/// across service hops.
pub fn should_sample(env: &Env, trace_id: &String, operation: &String) -> bool {
    let rate_bps = effective_sample_rate(env, operation);
    if rate_bps == 0 {
        return false;
    }
    if rate_bps >= 10_000 {
        return true;
    }
    let hash = hash_trace_id(env, trace_id);
    (hash % 10_000) < rate_bps as u64
}

/// Retrieve the effective sample rate for `operation`, falling back to the
/// global rate if no per-operation config exists.
pub fn effective_sample_rate(env: &Env, operation: &String) -> u32 {
    if let Some(cfg) = env
        .storage()
        .persistent()
        .get::<_, TraceSamplingConfig>(&TracingKey::SampleRate(operation.clone()))
    {
        return cfg.sample_rate_bps;
    }
    env.storage()
        .persistent()
        .get::<_, u32>(&TracingKey::GlobalSampleRate)
        .unwrap_or(DEFAULT_SAMPLE_RATE_BPS)
}

/// Simple hash of a trace-ID string — XOR-fold of bytes with a FNV-like mix.
fn hash_trace_id(env: &Env, trace_id: &String) -> u64 {
    let bytes = trace_id.to_xdr(env);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in bytes.iter() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
    }
    h
}

// ── Core API ─────────────────────────────────────────────────────────────────

/// Record a trace span by emitting a Soroban event.
///
/// Call this at the **start** of any contract function you want traced.
/// The returned [`TraceSpan`] contains the span data; callers may inspect
/// `span.sampled` to decide whether to propagate the context further.
///
/// # Arguments
///
/// * `ctx`       — incoming trace context from the caller (may be a root span
///                 if `parent_span_id` is empty).
/// * `span_id`   — the span ID generated by the caller for *this* function
///                 invocation (16-char hex string).
/// * `operation` — human-readable name for this operation.
/// * `status`    — outcome of the operation.
pub fn record_span(
    env: &Env,
    ctx: TraceContext,
    span_id: String,
    operation: String,
    status: SpanStatus,
) -> TraceSpan {
    let sampled = should_sample(env, &ctx.trace_id, &operation);

    let span = TraceSpan {
        trace_id: ctx.trace_id,
        span_id,
        parent_span_id: ctx.parent_span_id,
        operation: operation.clone(),
        status,
        sampled,
        ledger: env.ledger().sequence(),
    };

    if sampled {
        emit_trace_event(env, &span);
    }

    span
}

/// Emit a `trace/span` Soroban event for the given span.
fn emit_trace_event(env: &Env, span: &TraceSpan) {
    let topics = (
        symbol_short!("trace"),
        symbol_short!("span"),
    );
    env.events().publish(topics, span.clone());
}

/// Set the sample rate for a specific operation.
///
/// Admin-only.  `sample_rate_bps` must be in range `[0, 10_000]`.
pub fn set_trace_sample_rate(
    env: &Env,
    admin: Address,
    operation: String,
    sample_rate_bps: u32,
) -> Result<(), ContractError> {
    admin.require_auth();
    if sample_rate_bps > 10_000 {
        return Err(ContractError::InvalidAmount);
    }
    let cfg = TraceSamplingConfig {
        operation: operation.clone(),
        sample_rate_bps,
    };
    env.storage()
        .persistent()
        .set(&TracingKey::SampleRate(operation), &cfg);
    Ok(())
}

/// Set the global fallback sample rate.
///
/// Admin-only.  `sample_rate_bps` must be in range `[0, 10_000]`.
pub fn set_global_sample_rate(
    env: &Env,
    admin: Address,
    sample_rate_bps: u32,
) -> Result<(), ContractError> {
    admin.require_auth();
    if sample_rate_bps > 10_000 {
        return Err(ContractError::InvalidAmount);
    }
    env.storage()
        .persistent()
        .set(&TracingKey::GlobalSampleRate, &sample_rate_bps);
    Ok(())
}

/// Get the current sample rate for an operation (returns effective rate,
/// including global fallback).
pub fn get_trace_sample_rate(env: &Env, operation: String) -> u32 {
    effective_sample_rate(env, &operation)
}

// ── Off-chain export types (for the indexer / OTLP bridge) ───────────────────

/// OpenTelemetry-compatible span export structure.
///
/// The indexer reads `trace/span` events and maps them to this struct, then
/// serialises to OTLP/JSON for forwarding to Jaeger, Grafana Tempo, or any
/// other OTLP-compatible backend.
///
/// This struct lives in the Rust crate but is only used off-chain — it is
/// NOT a `#[contracttype]` because it is intended for serialisation via
/// `serde`, not Soroban's XDR codec.
pub struct OtlpSpanExport {
    pub trace_id: &'static str,
    pub span_id: &'static str,
    pub parent_span_id: Option<&'static str>,
    pub name: &'static str,
    pub start_time_unix_nano: u64,
    pub end_time_unix_nano: u64,
    pub status_code: u32, // 0 = Unset, 1 = Error, 2 = Ok
    pub attributes: &'static [(&'static str, &'static str)],
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Env, String};

    fn make_trace_ctx(env: &Env) -> TraceContext {
        TraceContext {
            trace_id: String::from_str(env, "aabbccddeeff00112233445566778899"),
            parent_span_id: String::from_str(env, ""),
        }
    }

    #[test]
    fn test_should_sample_100_pct() {
        let env = Env::default();
        let ctx = make_trace_ctx(&env);
        let op = String::from_str(&env, "vouch");
        // Default is 100 % — all spans sampled.
        assert!(should_sample(&env, &ctx.trace_id, &op));
    }

    #[test]
    fn test_should_sample_0_pct() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let op = String::from_str(&env, "vouch");
        set_trace_sample_rate(&env, admin, op.clone(), 0).unwrap();

        let ctx = make_trace_ctx(&env);
        assert!(!should_sample(&env, &ctx.trace_id, &op));
    }

    #[test]
    fn test_record_span_emits_sampled() {
        let env = Env::default();
        let ctx = make_trace_ctx(&env);
        let span_id = String::from_str(&env, "1122334455667788");
        let op = String::from_str(&env, "request_loan");

        let span = record_span(&env, ctx, span_id, op, SpanStatus::Ok);
        assert!(span.sampled);
        assert_eq!(span.status, SpanStatus::Ok);
    }

    #[test]
    fn test_set_global_sample_rate() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        set_global_sample_rate(&env, admin, 5_000).unwrap();

        let op = String::from_str(&env, "repay");
        let rate = get_trace_sample_rate(&env, op);
        assert_eq!(rate, 5_000);
    }

    #[test]
    fn test_per_operation_overrides_global() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        // Global = 50 %
        set_global_sample_rate(&env, admin.clone(), 5_000).unwrap();
        // Per-operation for "slash" = 100 %
        set_trace_sample_rate(&env, admin, String::from_str(&env, "slash"), 10_000).unwrap();

        let rate = get_trace_sample_rate(&env, String::from_str(&env, "slash"));
        assert_eq!(rate, 10_000);

        // Other operations still see global fallback.
        let rate2 = get_trace_sample_rate(&env, String::from_str(&env, "vouch"));
        assert_eq!(rate2, 5_000);
    }

    #[test]
    fn test_invalid_sample_rate_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let result = set_trace_sample_rate(
            &env,
            admin,
            String::from_str(&env, "vouch"),
            10_001,
        );
        assert_eq!(result, Err(ContractError::InvalidAmount));
    }
}
