# Scalability: Current Limits, Bottlenecks, and Scaling Roadmap

This document describes QuorumCredit's current scalability envelope, the
bottlenecks that constrain it, and the proposed path to scale the protocol
beyond its current capacity. It complements the operational/backup docs in
[`backup-recovery-guide.md`](./backup-recovery-guide.md) and the
[`event-indexing-guide.md`](./event-indexing-guide.md), which describe the
off-chain indexer that scaling proposals below build on.

## 1. Current Capacity Limits

QuorumCredit runs as a Soroban smart contract on the Stellar network. Its
scalability envelope is bounded by three layers:

### 1.1 Ledger/network-level limits
- **Ledger close time**: Stellar produces a new ledger roughly every 5–6
  seconds, which is the hard floor on how quickly any single on-chain
  state transition (loan request, repayment, vouch, slash) can be
  confirmed.
- **Per-transaction resource limits**: Soroban enforces CPU instruction,
  memory, and read/write footprint (ledger entry) limits per transaction.
  Contract calls that touch many storage entries in one invocation (e.g.
  iterating a large vouch list or a large withdrawal queue) are the ones
  most likely to approach these ceilings.
- **Per-ledger throughput**: total resources available per ledger are
  shared across every transaction submitted to the network, not just
  QuorumCredit's, so contract throughput degrades under general network
  congestion, not only under this protocol's own load.

### 1.2 Contract storage limits
- Persistent storage entries (loan records, credit scores, vouch records,
  collateral pool state — see `src/loan.rs`, `src/credit_score.rs`,
  `src/vouch.rs`, `src/collateral_pool.rs`) each have an associated rent
  cost and a bump/TTL mechanism. Storage that isn't actively bumped expires
  and must be restored, which is itself a bounded, costed operation.
- The storage-redesign work (see `src/storage_redesign_test.rs` and the
  `CREDIT_SCORE_IMPLEMENTATION_SUMMARY.md` / backup-restore PR history) was
  itself driven by earlier capacity constraints — moving from monolithic
  per-user blobs toward paginated storage was necessary to keep individual
  reads/writes within Soroban's footprint limits as user counts grow.

### 1.3 Off-chain dependency limits
- **Indexer throughput**: components under `tools/indexer` and the
  event-indexing pipeline must keep up with on-chain event volume to serve
  accurate backup/restore and analytics data; see
  `docs/event-indexing-guide.md`. Indexer lag directly limits how fast
  downstream systems (dashboards, backup snapshots) can reflect protocol
  state.
- **Oracle throughput**: the dynamic rate oracle (`src/dynamic_rate_oracle_test.rs`
  and related oracle integration) has its own update cadence; the
  protocol's interest-rate responsiveness is bounded by that cadence, not
  by the contract itself.

## 2. Identified Bottlenecks

Ranked roughly by how soon each is expected to bind as usage grows:

1. **Vouch-list iteration cost.** Functions that must scan a borrower's
   full set of vouches (used in the vouching credit-score component and in
   slashing) scale linearly with vouch count per borrower. A borrower with
   an unusually large number of vouchers increases the CPU/read cost of
   every credit-score recalculation and slash event that touches them.
2. **Withdrawal queue processing.** Batch processing of a large withdrawal
   queue (see `WITHDRAWAL_QUEUE_OPTIMIZATION.md`, `src/withdrawal_queue_test.rs`)
   is bounded by how many queue entries can be processed within one
   transaction's resource budget; very large queues require multiple
   transactions to fully drain.
3. **Storage rent/TTL churn at scale.** As the number of active borrowers,
   vouches, and loans grows, the aggregate cost of keeping all of that
   state alive (bump operations) grows linearly with active-entry count,
   competing for the same per-ledger resource budget as user transactions.
4. **Indexer/backup lag under high event volume.** The further the
   on-chain event volume outpaces indexer processing speed, the larger the
   gap between real protocol state and the state visible to indexer-backed
   systems (backup/restore, dashboards, monitoring alerts).
5. **Oracle update cadence vs. loan velocity.** If loan request/repayment
   volume grows faster than oracle price/rate updates, rate-sensitive
   calculations (interest, collateral requirements) can lag real market
   conditions during high-volatility periods.
6. **Single-chain settlement finality.** Every state transition currently
   settles directly on Stellar mainnet; there is no batching layer, so
   protocol throughput is capped at whatever share of Stellar's overall
   ledger capacity QuorumCredit can obtain.

## 3. Proposed Layer 2 / Sidechain Solutions

Three complementary directions, from least to most disruptive:

### 3.1 Off-chain aggregation with on-chain settlement (near-term)
Batch high-frequency, low-individual-value operations off-chain (e.g.
vouch-weight recalculation, timeliness tracking) and only commit periodic
aggregate checkpoints on-chain — similar in spirit to a rollup's
"execute off-chain, settle on-chain" model, but scoped narrowly to
credit-score inputs rather than the full protocol. The existing indexer
infrastructure (`tools/indexer`) is a natural starting point for computing
these aggregates, since it already observes all relevant events.

### 3.2 Sidechain for high-volume, low-value operations
Operations that don't require Stellar mainnet's full security guarantees
(e.g. reputation-point accrual, non-financial vouch bookkeeping) could move
to a purpose-built sidechain or app-chain that periodically anchors state
roots back to the mainnet contract via a Merkle proof — the
`src/merkle_tree.rs` module already provides Merkle-tree primitives that
could be extended for this purpose. This isolates high-frequency,
lower-stakes traffic from the resource budget shared with loan/collateral
transactions.

### 3.3 Rollup-style batched settlement for loans (long-term)
For the highest-value operations (loan issuance, repayment, collateral
release), a rollup-style approach — batching many users' loan state
transitions into a single mainnet transaction with a validity or fraud
proof — would allow throughput to scale roughly with batch size rather than
with mainnet's per-ledger transaction limit. This is the most disruptive
option because it requires a proof system and a settlement contract
redesign, and should only be pursued once the near-term options are
exhausted.

## 4. Upgrade Path to Scaled Infrastructure

Recommended phased rollout, each phase gated on the previous one's
bottlenecks actually being observed in production (see
`docs/monitoring-guide.md` for the metrics to watch):

**Phase 0 — Current state.** Direct on-chain settlement for everything;
indexer used for read-side/backup purposes only (already implemented).

**Phase 1 — Indexer-assisted aggregation.** Move vouch-weight and
timeliness aggregation computation off-chain into the indexer, with the
contract accepting periodic signed/attested checkpoint updates instead of
recomputing from raw on-chain iteration every time. Reduces per-transaction
CPU cost for credit-score updates without changing trust assumptions
materially, since the indexer's output can still be independently
recomputed from on-chain events.

**Phase 2 — Sidechain for reputation/vouch bookkeeping.** Stand up a
sidechain or app-chain for high-frequency reputation and vouch state,
anchoring periodic Merkle roots back to the mainnet contract. Loan and
collateral logic remain on mainnet. This is the first phase requiring new
infrastructure (a sidechain validator set or existing app-chain framework)
rather than only off-chain compute.

**Phase 3 — Rollup-style batched loan settlement.** Once loan volume alone
saturates mainnet capacity (i.e. Phase 1/2 have already absorbed the
non-loan bottlenecks), introduce batched settlement for loan issuance and
repayment with a proof system, migrating the collateral pool and loan
lifecycle logic to operate against batches rather than individual
transactions.

Each phase should ship behind a feature flag / governance-gated switch, so
mainnet loan/collateral logic (the highest-value, most security-sensitive
path) is never migrated until the lower-risk phases have been live and
monitored for a full governance/audit cycle.

## 5. Cost-Benefit Analysis

| Option | Dev effort | Est. throughput gain | Latency impact | New trust assumptions | Recommended when |
|---|---|---|---|---|---|
| Phase 1: Indexer-assisted aggregation | Low–Medium (extends existing indexer) | Moderate — removes vouch-iteration cost from hot path | Neutral to slightly faster (fewer on-chain reads per tx) | None — checkpoints independently verifiable from on-chain events | Vouch-list iteration or storage-rent costs are the observed bottleneck |
| Phase 2: Sidechain for reputation/vouching | Medium–High (new chain/validator infra) | High for reputation-related traffic; no effect on loan throughput | Slightly higher for reputation reads (cross-chain proof verification) | New validator set or app-chain trust assumption for non-financial data | Reputation/vouch traffic itself saturates mainnet capacity independent of loan volume |
| Phase 3: Rollup-style loan settlement | High (proof system + contract redesign) | Highest — throughput scales with batch size, not per-ledger tx limit | Higher settlement latency per individual loan (batch window) in exchange for higher aggregate throughput | Proof-system soundness assumption; sequencer liveness/censorship assumptions | Loan/collateral transaction volume itself saturates mainnet capacity |

**Recommendation:** pursue Phase 1 first — it has the best cost-to-benefit
ratio, requires no new trust assumptions, and directly targets the
bottleneck (vouch-list iteration, storage rent churn) most likely to bind
first given current usage patterns. Phases 2 and 3 should only be
prioritized once monitoring data (per `docs/monitoring-guide.md`) shows
their respective bottlenecks are actually being approached in production,
not preemptively.
