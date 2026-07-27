# Design: Credit Derivatives Market

## Overview

Voucher holders currently bear full default risk on loans they originate. A credit derivatives market enables hedging: vouchers can be transferred, bundled, or partially insured. This document outlines a market for credit default swaps (CDS) and related primitives that allow risk transfer without modifying underlying loan contracts.

## Problem Statement

- Vouchers are illiquid instruments tied to specific loans; holders bear 100% default risk.
- Risk concentration discourages large pools or new entrants.
- No mechanism exists to hedge defaults or transfer risk efficiently.

## Proposed Primitives

### 1. Credit Default Swap (CDS)

**Definition**: A bilateral contract in which a protection buyer pays a premium to a protection seller. If the underlying loan defaults, the seller compensates the buyer for the loss.

**Parameters**:
- `underlying_loan_id`: The loan being hedged.
- `protection_buyer`: Entity seeking to hedge default risk.
- `protection_seller`: Entity accepting default risk in exchange for premium.
- `notional_amount`: Amount of default loss covered (up to the loan principal).
- `premium_bps`: Annual premium in basis points (e.g., 500 bps = 5%).
- `maturity`: End date of the swap.

**Cashflows**:
1. **Premium**: Protection buyer pays `(notional * premium_bps / 10000) / 12` monthly.
2. **Contingent Payment**: On loan default, seller pays buyer the loss amount (capped at notional).

**Settlement**:
- Physical settlement: Seller takes possession of defaulted voucher; buyer receives cash.
- Cash settlement: Seller pays difference between recovery value and notional.

### 2. Voucher Tranches (Securitization)

**Definition**: A pool of loans is divided into risk tiers (tranches). Senior tranches absorb losses last; junior tranches absorb losses first.

**Tranches**:
- **Senior (A)**: First to receive yield, last to absorb defaults. Lower risk, lower yield.
- **Mezzanine (B)**: Middle tier. Moderate risk, moderate yield.
- **Equity (Z)**: Last to receive yield, first to absorb defaults. Highest risk, highest yield.

**Example**: A $1M pool of loans:
- Senior: $700k, 2% yield, covers 99% of defaults.
- Mezzanine: $200k, 6% yield, covers 90% of defaults (after senior exhausted).
- Equity: $100k, 15% yield, covers defaults first.

### 3. Loan Index and Futures

**Definition**: A synthetic index tracking average loan performance metrics (e.g., default rate, yield).

**Use Case**: Traders can take positions on market-wide credit risk without holding individual vouchers.

**Parameters**:
- `index_constituents`: List of loans and weights.
- `mark_price`: Index price updated daily based on default events and yield accrual.

---

## Contract Interface

```solidity
// Pseudocode for CDS contract

contract CreditDefaultSwap {
    struct Swap {
        bytes32 loan_id;
        address protection_buyer;
        address protection_seller;
        uint128 notional;
        uint16 premium_bps;  // Annual premium in basis points
        uint64 maturity_timestamp;
        bool is_active;
    }

    // Initiate a new CDS
    function initiate_swap(
        bytes32 loan_id,
        address protection_seller,
        uint128 notional,
        uint16 premium_bps,
        uint64 maturity_timestamp
    ) -> bytes32 swap_id;

    // Pay premium (called monthly or on demand)
    function pay_premium(bytes32 swap_id, uint128 amount) -> bool;

    // Settle on default (called by oracle or loan contract)
    function settle_on_default(
        bytes32 swap_id,
        uint128 recovery_amount
    ) -> bool;

    // Early termination (both parties agree)
    function unwind_swap(bytes32 swap_id) -> bool;

    // Query swap state
    function get_swap(bytes32 swap_id) -> Swap;
    function get_accrued_premium(bytes32 swap_id) -> uint128;
}

contract VoucherTranche {
    struct Tranche {
        bytes32[] loan_pool;
        uint128 size;
        uint8 seniority;  // 0 = senior, 255 = equity
        uint16 expected_yield_bps;
        uint128 realized_loss;
    }

    // Mint tranche tokens
    function deposit(bytes32 tranche_id, uint128 amount) -> uint128 tokens;

    // Redeem tranche tokens
    function withdraw(bytes32 tranche_id, uint128 tokens) -> uint128 amount;

    // Update loss cascade (called on loan default)
    function process_default(bytes32 loan_id, uint128 loss_amount) -> bool;

    // Query tranche metrics
    function get_loss_absorption(bytes32 tranche_id) -> uint128;
}
```

---

## Market Design

### Pricing

**CDS Premium** (simplified Black-Scholes analog):
```
premium_bps = base_rate + (probability_of_default * loss_given_default * 10000)
```

Example:
- Base rate: 50 bps (risk-free)
- Loan default probability: 5% over 1 year
- Loss given default: 30% (30% recovery)
- Premium: 50 + (0.05 * 0.70 * 10000) = 50 + 350 = 400 bps (4% annually)

### Counterparty Risk

- Protection sellers must post collateral (e.g., USDC reserves) equal to `notional * max_loss_estimate`.
- Collateral is liquidated if mark-to-market losses exceed thresholds.

### Liquidity

- Derivatives are tradeable; buyers can exit early by selling the swap to a third party.
- An order book or AMM (Automated Market Maker) facilitates price discovery.
- Bid-ask spreads reflect counterparty risk and liquidity depth.

---

## Regulatory & Compliance

- **Counterparty Limits**: Exposure to any single counterparty capped at governance-defined threshold (e.g., 10% of pool).
- **Disclosure**: All CDS positions on a loan are disclosed to borrowers and other syndicate members.
- **Position Limits**: Individual entities capped at max notional (e.g., 200% of underlying loan size) to prevent naked short selling.

---

## Implementation Roadmap

**Phase 1**: Simple CDS with physical settlement and manual accounting.

**Phase 2**: Add tranche-based securitization and automated loss cascade.

**Phase 3**: Deploy loan index and futures contracts.

**Phase 4**: Integrate external pricing feeds and regulatory reporting.

---

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Basis Risk | CDS may not perfectly hedge; choose loans with stable, correlated defaults. |
| Model Risk | Pricing models may underestimate tail risk; stress-test assumptions regularly. |
| Contagion | Sellers' default can cascade; diversify seller base and set per-counterparty limits. |
| Illiquidity | Tranches may be hard to exit; offer secondary market and clear pricing. |

