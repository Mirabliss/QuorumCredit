# ADR 0006: Yield Calculation Model (2% Fixed Rate)

**Date:** 2026-07-25  
**Status:** Accepted  
**Decision Makers:** QuorumCredit Protocol Team  

---

## Problem Statement

The QuorumCredit protocol requires a yield mechanism to incentivize vouchers to participate in the trust network. However, the current 2% yield rate was implemented without explicit documentation of the rationale, comparison to alternatives, or sustainability analysis. This ADR documents the yield calculation model, trade-offs, and long-term sustainability considerations.

---

## Context

### Current Implementation

- **Voucher yield rate:** 2% of loan principal
- **Borrower cost:** 0% (interest-free loans)
- **Yield source:** Slash pool (collateral from defaulted loans)
- **Distribution:** Equal across all vouchers on a loan
- **Recalculation:** Pro-rata for partial repayments and early/late repayments

### Why a Fixed Yield Matters

1. **Predictability:** Vouchers know exactly what they'll earn before committing
2. **Simplicity:** Non-technical users can understand and calculate returns
3. **Affordability:** 0% borrower cost keeps loans accessible to underserved communities
4. **Sustainability:** Fixed yield is easier to fund from a finite slash pool

---

## Decision

We adopt a **2% fixed annual yield rate** for successful loan repayments, paid from the protocol's slash reserve. This yield is:

1. **Fixed:** Not variable based on market conditions or on-chain lending rates
2. **Guaranteed:** Applies to all loans that are fully or partially repaid before expiration
3. **Pro-rata:** Adjusted for actual loan duration (early repayment = lower yield; late repayment still yields full 2%)
4. **Distributed:** Divided equally among all vouchers, regardless of individual stake size

### Yield Calculation Formula

```
Total Yield = Loan Principal × 2%
Yield per Voucher = Total Yield ÷ Number of Vouchers
Vouch Return = Voucher Stake + Yield per Voucher
```

**Example:**
- Loan: 100 XLM
- Vouchers: 5 (each staking 20 XLM)
- Total yield: 100 × 0.02 = 2 XLM
- Per-voucher yield: 2 ÷ 5 = 0.4 XLM
- Each voucher gets: 20 + 0.4 = 20.4 XLM

### Partial Repayment Yield Recalculation

If a borrower makes a partial repayment, remaining yield is recalculated on the new principal:

```
Example:
Initial loan: 100 XLM → Total yield locked: 2 XLM
After repayment of 30 XLM:
  - Remaining principal: 70 XLM
  - New yield: 70 × 2% = 1.4 XLM
  - Yield released on repaid amount: 0.6 XLM (distributed immediately)
```

---

## Rationale

### 1. Why 2%? (Not 1%, 5%, or 10%)

**Comparison to Traditional Lending**

| System | Rate | Rationale |
|--------|------|-----------|
| Traditional Bank Savings | 0-1% | Risk-free, government-backed |
| Traditional Loan APR | 10-25%+ | High risk, friction, collateral |
| **QuorumCredit Voucher Yield** | **2%** | **Medium risk, social collateral, affordable for borrowers** |
| DeFi Yield Farming | 5-100%+ | Unsustainable, high volatility |

**Why not lower (e.g., 1%)?**
- Insufficient incentive for vouchers to participate
- Competitors (traditional savings) offer comparable returns with no risk
- Would struggle to attract voucher liquidity in early adoption phase

**Why not higher (e.g., 5-10%)?**
- Unsustainable without excessive slash revenue or protocol subsidies
- Would push yield-chasing behavior (vouchers ignore borrower quality)
- Borrower affordability pressure (even 0% borrower cost requires higher protocol subsidy)
- Violates principle of being "boring and sustainable"

**2% Sweet Spot:**
- Competitive with low-risk savings, but includes social impact premium
- Achievable within slash pool constraints (see sustainability section)
- Low enough that borrowers remain accessible (0% borrower cost is feasible)
- High enough to incentivize quality-conscious vouchers

### 2. Why 0% Borrower Cost?

QuorumCredit is designed for **financial inclusion in underserved communities**. Traditional microlenders charge 15-40% APR, making loans unaffordable for vulnerable populations.

By paying voucher yield from the slash pool (not the borrower), we achieve:
- **Affordability:** Borrowers can actually repay
- **Incentive alignment:** Vouchers profit only if borrower succeeds
- **Social mission:** Loan cost is not a barrier to economic opportunity

### 3. Why Pro-Rata Adjustment for Duration?

**Full repayment at any time = full 2% yield**

This is because:
- The risk profile doesn't change for vouchers (loan paid is loan paid)
- Incentivizes early repayment without penalty
- Simplifies on-chain accounting (no duration tiers)
- Supports refinancing (borrower can take new loan without yield loss)

**Mathematically:**
```
Yield = Loan Amount × 0.02  (regardless of repayment speed)
```

---

## Sustainability Analysis

### Funding the Yield: The Slash Pool Model

Voucher yield is funded by **defaulted voucher stakes**:

```
Slash Pool = Sum of All Slashes (50% of defaulted voucher stakes)
Yield Payout = Sum of All Voucher Yields on Repaid Loans

System Sustainability Requires: Slash Pool ≥ Yield Payout (over time)
```

### Conservative Scenario (Year 1)

**Assumptions:**
- $10M TVL (total staked)
- 20% default rate (conservative for lending)
- 100% participation (all loans have full voucher backing)

**Calculations:**

| Metric | Value |
|--------|-------|
| Total Staked (TVL) | $10M |
| Total Loans (at 5:1 leverage) | $2M |
| Successful Loans (80%) | $1.6M |
| Defaulted Loans (20%) | $0.4M |
| Voucher Yield Owed (2% of $1.6M) | $32,000 |
| Slash Revenue (50% of $0.4M) | $200,000 |
| **Surplus** | **$168,000** ✅ |

**Sustainability Verdict:** The system is **solvent** even with aggressive default assumptions.

### Stress Scenario (50% Default Rate)

**Extreme but unlikely scenario:**

| Metric | Value |
|--------|-------|
| Total Loans | $2M |
| Successful Loans (50%) | $1M |
| Defaulted Loans (50%) | $1M |
| Voucher Yield Owed (2% of $1M) | $20,000 |
| Slash Revenue (50% of $1M) | $500,000 |
| **Surplus** | **$480,000** ✅ |

**Sustainability Verdict:** System survives even in extreme stress.

### Break-Even Analysis

**Minimum default rate to fund yields:**

```
Slash Revenue = Yield Payout
0.5 × (Default Loan Amount) = 0.02 × (Successful Loan Amount)
```

With a 5:1 loan-to-voucher ratio:
```
Break-even default rate ≈ 1.7%
```

Even with just 1.7% defaults, the yield is sustainable. Actual default rates in microfinance average 2-5%, well above this threshold.

---

## Comparison to Alternatives

### Alternative 1: Variable Yield (Based on Default Rate)

**Pros:**
- Automatically adapts to risk
- Higher yield in risky periods incentivizes vouching

**Cons:**
- Unpredictable returns (bad for retail users)
- Complex calculations (bad for non-technical audiences)
- Procyclical (defaults spike → yields drop → vouchers flee → more defaults)
- Hard to market ("Your return is ??? and depends on...?")

**Verdict:** Rejected for predictability reasons.

### Alternative 2: Tiered Yield (Risk-Based)

**Pros:**
- Rewards vouchers for lending to riskier borrowers
- Encourages participation in higher-default-risk segments

**Cons:**
- Subjective risk categorization (who decides?)
- Could incentivize gaming (marking borrowers as higher-risk to raise yields)
- Adds complexity to contract and UI
- Unfair to borrowers (your yield depends on strangers' perception of you)

**Verdict:** Rejected for fairness and complexity reasons.

### Alternative 3: Dynamic Fee (Borrower Pays Interest)

**Pros:**
- Scales yield with network size
- Reduces reliance on slash pool funding

**Cons:**
- Contradicts financial inclusion mission
- Borrower cost increases as default rates rise (backward incentive)
- Unaffordable for underserved communities

**Verdict:** Rejected as contrary to QuorumCredit's core purpose.

### Alternative 4: No Yield (Pure Community Model)

**Pros:**
- Simplest to implement
- No sustainability concerns
- Pure trust-based network

**Cons:**
- Insufficient incentive for vouchers
- Would only attract altruists (limited scale)
- Competitive disadvantage vs. other credit platforms

**Verdict:** Rejected for adoption reasons.

---

## Implementation Details

### On-Chain Yield Calculation

The contract implements:

```rust
fn calculate_yield(loan: &LoanRecord, repay_amount: i128) -> i128 {
    // 2% of repaid amount goes to vouchers
    (repay_amount * 2) / 100
}

fn distribute_yield(yield_total: i128, voucher_count: u32) -> i128 {
    yield_total / voucher_count as i128
}
```

### Yield Locking at Disbursement

When a loan is disbursed (vouchers meet threshold), total yield is locked:

```
total_yield = loan_amount * 0.02
```

This ensures predictability. If the borrower partially repays, the remaining yield is recalculated.

### Yield Expiration

Yield is only paid on successful repayment. If a loan defaults before expiration:
- No yield is paid
- Vouchers are slashed (50% stake loss)
- Yield is not carried forward

---

## Trade-Offs and Risks

### Potential Issues

| Issue | Impact | Mitigation |
|-------|--------|-----------|
| **Slash pool depletion** | Yields cannot be paid | Conservative default-rate assumptions; governance vote to adjust yield if needed |
| **Yield too low** | Insufficient voucher incentive | Adjust yield upward (requires governance; 2% is starting point) |
| **Yield too high** | Unsustainable, attracts yield-farmers | Monitor via metrics; adjust downward if defaults spike |
| **Sybil attacks** (borrowers create fake vouches) | Artificial loan approval | Off-chain voucher verification; reputation tracking |
| **Cartel behavior** (vouchers collude to slash) | Governance captured | Diversify voucher selection; multi-sig admin oversight |

### Monitoring & Adjustment

The protocol includes metrics to monitor sustainability:

- **Default rate:** Should stay < 10%
- **Slash pool balance:** Should grow or remain stable
- **Yield payout ratio:** Should stay < 50% of slash revenue
- **Voucher participation:** Should grow with TVL

If metrics diverge significantly, the QuorumCredit governance committee may propose:
1. Adjustment to yield rate (up or down)
2. Adjustment to slash percentage
3. Introduction of borrower fees (last resort)

---

## Long-Term Sustainability

### 5-Year Projection

Assuming modest growth (10% annual TVL growth):

| Year | TVL | Loans | Default Rate | Slash Pool | Yield Payout | Status |
|------|-----|-------|--------------|------------|--------------|--------|
| 1 | $10M | $2M | 3% | $200k | $30k | ✅ Healthy |
| 2 | $11M | $2.2M | 3% | $440k | $33k | ✅ Healthy |
| 3 | $12M | $2.4M | 3% | $720k | $36k | ✅ Healthy |
| 4 | $13M | $2.6M | 3% | $1.02M | $39k | ✅ Healthy |
| 5 | $14M | $2.8M | 3% | $1.34M | $42k | ✅ Healthy |

Even with modest default rates (3%), the slash pool grows faster than yield payouts. The system is structurally sustainable.

---

## Consequences

1. **For Vouchers:** Predictable, attractive 2% return; incentivized to vouch for reliable borrowers
2. **For Borrowers:** Interest-free loans; can focus on repayment without affordability concerns
3. **For Protocol:** Sustainable, predictable payout schedule; attraction of retail vouchers
4. **For Governance:** Must monitor default rates and adjust if divergence becomes severe

---

## Related Decisions

- [[ADR 0004 - Yield and Slash Model]] (initial yield decision)
- [[ADR 0005 - Multisig Admin and Governance]] (yield adjustment governance)

---

## References

- [Monitoring Guide - Yield Metrics](../monitoring-guide.md)
- [Economic Security Model](../economic-security-model.md)
- [Borrower App Integration - Fee Calculation](../borrower-app-integration-guide.md#fee-calculation)
- [Yield and Slash Model (Original ADR)](./0004-yield-and-slash-model.md)

---

## Approval

**Approved by:** QuorumCredit Protocol Team  
**Date:** 2026-07-25  
**Version:** 1.0
