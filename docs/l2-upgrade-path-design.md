# Design: Upgrade Path to Layer 2

## Overview

Stellar mainnet, while performant, may face throughput constraints as QuorumCredit adoption scales (e.g., millions of loans, frequent repricing, liquidations). This document outlines a design for eventual migration to a Layer 2 (L2) solution while maintaining asset custody and economic guarantees.

## Problem Statement

- **Throughput**: Stellar can process ~1,000 tx/sec; a global lending platform may require 10,000+ tx/sec.
- **Latency**: Stellar block time (~5 seconds) may be too slow for high-frequency trading or liquidations.
- **Cost**: Per-transaction fees accumulate as volume grows; L2 can reduce costs 10–100x.
- **Lock-in**: Early L2 choice limits future portability; design must support multi-L2 future.

## Proposed Architecture

### Phase 1: Mainnet-Only (Current State)

All contracts, assets, and state live on Stellar mainnet.

### Phase 2: L2-Ready Contracts (6–12 months)

Refactor smart contracts to separate state and logic:

1. **Custody Bridge**: Assets remain on Stellar mainnet in escrow.
   - Users deposit funds into a bridge contract; bridge issues synthetic L2 tokens (1:1).
   - Withdrawals destroy L2 tokens and release mainnet assets.

2. **State Sync**: L2 maintains a canonical state root on mainnet periodically.
   - Every N blocks (e.g., 1,000), L2 publishes a Merkle root to mainnet.
   - Mainnet contract verifies signatures and records the root.

3. **Loan Registry**: Mainnet holds a registry of all loans.
   - L2 publishes new loan originations; mainnet records them.
   - Enables mainnet auditability and mainnet-initiated interventions (e.g., freezes).

### Phase 3: L2 Deployment (12–24 months)

Launch L2 with rollup semantics (optimistic or ZK-proof based):

1. **Optimistic Rollup** (faster to deploy):
   - L2 batches transactions and posts commitments to mainnet.
   - Validators can submit fraud proofs if commitment is invalid.
   - 7-day challenge window before finality; assets can be exited after.

2. **ZK Rollup** (higher security):
   - L2 generates cryptographic proofs of correct state transitions.
   - Mainnet verifies proofs; ~1-hour time to finality.
   - No challenge window; faster asset exits.

### Phase 4: Multi-L2 Support (24+ months)

- Loan origination, governance, and yield distribution on L2.
- Cross-L2 atomic swaps via IBC or token bridges.
- Users choose preferred L2; custody and settlement on mainnet.

---

## Technical Design

### Layer 2 Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        Stellar Mainnet                        │
│  ┌────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │   Escrow   │  │  State Root  │  │  Loan Registry     │   │
│  │  Contract  │  │  Verifier    │  │  Contract          │   │
│  └────────────┘  └──────────────┘  └────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
           ▲                ▲                    ▲
           │                │                    │
        (deposits)   (state roots)         (new loans)
           │                │                    │
           ▼                ▼                    ▼
┌──────────────────────────────────────────────────────────────┐
│                      Layer 2 (e.g., Soroban L2)               │
│  ┌────────────┐  ┌──────────────┐  ┌────────────────────┐   │
│  │   Loan     │  │  Yield       │  │  Governance        │   │
│  │  Engine    │  │  Accrual     │  │  & Voting          │   │
│  └────────────┘  └──────────────┘  └────────────────────┘   │
│                                                               │
│  State: Balances, Loans, Collateral, Yields                 │
│  Throughput: 10,000+ tx/sec                                 │
│  Latency: ~1-5 sec finality (rollup-dependent)              │
└──────────────────────────────────────────────────────────────┘
```

### Custody Model

**Mainnet Escrow**:
```
Escrow Contract {
    total_locked: uint128,
    l2_bridge_address: bytes32,
    
    fn deposit(asset: Asset, amount: uint128) {
        // User transfers asset to escrow
        // Escrow emits event; L2 bridge listens
        // L2 mints synthetic token 1:1
    }
    
    fn withdraw(asset: Asset, amount: uint128, proof: bytes) {
        // Verify proof signed by L2 validators
        // Burn synthetic token on L2 (via state root)
        // Transfer asset back to user
    }
}
```

### State Sync

**Frequency**: Every 1,000 L2 blocks (~5–10 minutes).

**Data Posted**:
- Merkle root of L2 state (loans, balances, collateral).
- Validator set and multisig threshold.
- Proof of new loans originated (for auditability).

**Cost**:
- ~500 stroops per state root (~0.00005 USDC at current rates).
- ~50 MB/year of on-chain data.

### Exit Mechanism

**User Withdrawal**:
1. User initiates withdrawal request on L2 (burn synthetic token).
2. L2 includes withdrawal in next state root.
3. After challenge window (7 days for optimistic rollup), user can exit.
4. Mainnet contract transfers asset to user.

**Emergency Exit** (if L2 is offline):
- Users can submit L2 state proof to mainnet contract.
- Proof must include user's withdrawal and be signed by validator multisig.
- Fallback: Governance can unlock escrow if L2 failure is verified.

---

## Governance & Multisig

**Layer 2 Validator Set**:
- Initial: 7–11 validators (core team + community).
- Signatures required: 2/3 (for Optimistic) or 1/1 (for ZK proof).
- Rotations: Governance votes to add/remove validators.

**Mainnet Multisig** (escrow & state root verification):
- 5–7 signatories (geographically diverse).
- Threshold: 3/5.
- Powers: Pause deposits/withdrawals, update L2 validator set, emergency exits.

---

## Security & Risk Mitigations

| Risk | Mitigation |
|------|-----------|
| L2 Censorship | Users can force exit to mainnet; no funds trapped on L2. |
| State Root Divergence | Mainnet verifies proofs; fraud proofs challenge invalid roots. |
| Bridge Exploit | Escrow uses multisig; whitelists only canonical L2 contract. |
| Validator Collusion | 2/3 threshold requires honest minority; liveness guaranteed. |
| Cross-L2 Arbitrage | Governance sets price sync frequency; caps slippage to <1%. |

---

## Migration Timeline

| Phase | Timeline | Deliverables |
|-------|----------|--------------|
| 1. Audit & Planning | Months 1–3 | Detailed technical spec, threat model, simulations. |
| 2. Contract Refactor | Months 3–9 | State/logic separation, custody bridge, mainnet registry. |
| 3. L2 Deployment | Months 9–15 | L2 sequencer, rollup contracts, validator setup. |
| 4. Beta Testing | Months 15–18 | Testnet launch, user testing, security audit. |
| 5. Mainnet L2 Launch | Months 18–24 | Limited beta (e.g., $1M TVL cap), gradual rollout. |
| 6. Full Migration | Months 24+ | Optional: close mainnet operations, go L2-only. |

---

## Long-Term Considerations

1. **Multi-L2 Interoperability**: Support deployments on Soroban L2, Arbitrum (via Stellar bridge), or other EVMs.
2. **Atomic Settlements**: Cross-L2 loan swaps using IBC or wrapped assets.
3. **Regulatory**: Stablecoin bridge tokens may face jurisdiction-specific requirements; consult legal.
4. **Cost Sustainability**: L2 fees should be <0.1% of transaction value; monitor and adjust pricing.

---

## Success Metrics

- **Throughput**: 10,000+ tx/sec vs. ~1,000 on Stellar.
- **Latency**: <5 sec average (vs. ~5 sec on Stellar).
- **Cost**: <0.001 USDC/transaction (vs. ~0.00001 USDC on mainnet, but cheaper per-unit due to volume).
- **Decentralization**: 7+ validators, <1% monthly churn.
- **Security**: Zero unplanned capital loss events; <5 successful fraud proofs/year (indicating healthy competition).

