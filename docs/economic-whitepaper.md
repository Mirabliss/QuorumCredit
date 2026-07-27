# QuorumCredit Economic Whitepaper

## Status

Formal documentation of the economic model underlying the QuorumCredit
lending pool: capital structure, interest rate derivation, risk/default
assumptions, a comparison against traditional lending, and a sensitivity
analysis over the protocol's key parameters. This document complements
[`economic-security-model.md`](./economic-security-model.md) (which focuses
on attack economics) by explaining the *incentive* structure for honest
participants.

All monetary figures below are expressed in stroops (1 XLM = 10,000,000
stroops), matching the convention documented at the top of `src/types.rs`.

---

## 1. Lending Pool Economics and Capital Structure

QuorumCredit is a **peer-vouched, uncollateralized-by-default** credit
protocol. There is no single pooled reserve that all borrowers draw from;
instead, capital structure is built bottom-up from individual vouch
relationships:

- **Vouchers** stake capital (`vouch(voucher, borrower, stake, token)`)
  against a specific borrower's creditworthiness. This stake is the
  borrower's collateral base.
- **Borrowers** may request a loan up to
  `total_vouched_stake * max_loan_to_stake_ratio / 100`
  (`DEFAULT_MAX_LOAN_TO_STAKE_RATIO = 150`, i.e. up to 1.5x the vouched
  stake), reflecting that vouching is a reputational signal, not a 1:1
  collateral lock.
- **The contract's token balance** (funded by prior repayments, slashed
  stakes routed to treasury, and any admin-seeded liquidity) is the actual
  disbursement source; `request_loan` fails if the contract lacks liquidity.

This makes the capital structure closer to a **credit union / mutual
guarantee society** than a traditional over-collateralized DeFi money
market (compare Aave/Compound, where borrowers post >100% collateral in a
separate asset). The tradeoff: capital efficiency for borrowers is much
higher, but the protocol's credit risk is entirely a function of voucher
judgment and the slashing mechanism's ability to make bad vouches costly
after the fact rather than preventing them upfront.

With this PR, capital structure also supports **tranching** via
`src/loan_priority.rs`: loans can be tagged Senior, Mezzanine, or Junior,
and default recoveries are distributed through a waterfall (Senior paid to
par first, Junior absorbs first loss). This lets a pool operator construct
a senior-junior capital stack — e.g. junior stakers accept first-loss
exposure in exchange for a higher yield share — mirroring structured credit
products in traditional finance (CLOs, mezzanine debt funds) rather than
the flat, equal-priority pool the protocol had before.

## 2. Interest Rate Model Derivation

Yield paid to vouchers is governed by `yield_bps` (`DEFAULT_YIELD_BPS =
200`, i.e. 2%) applied to the loan's principal at repayment:

```
total_yield = loan.amount * yield_bps / BPS_DENOMINATOR
```

This is a **flat-rate, borrower-pays-on-repayment** model, not a
continuously-compounding money-market rate. Three adjustments compose on
top of the base rate:

1. **Vouch-age tiering** (`verify_repayment` in `lib.rs`): vouches held
   ≥30 days receive a 150% multiplier on their yield share, ≥7 days receive
   125%, otherwise 100%. This rewards vouchers for taking on duration risk
   — a longer-committed stake is worth more to the protocol's stability
   than a stake added the day before repayment, so it is priced higher, the
   same logic that underlies a term premium in traditional fixed income.
2. **Liquidity mining bonus** (`liquidity_mining_rate_bps`, default 50 bps):
   an emissions-style top-up on the voucher's stake, independent of loan
   performance, used to bootstrap voucher participation in excess of what
   pure credit yield would justify.
3. **Maturity/loyalty bonus** (`src/maturity.rs`): long-tenured vouch
   relationships (see `get_vouch_total_interest_bonus`) accrue up to 100bps
   of additional interest, capturing relationship value that a one-shot
   rate model would miss.

**Derivation intuition:** the effective yield a voucher earns is
approximately

```
effective_yield ≈ base_yield_bps * age_multiplier + liquidity_mining_bps + maturity_bonus_bps
```

This is a discrete, table-driven approximation of a continuous credit curve
(rate as a function of duration and relationship depth) rather than a
closed-form model — appropriate for an on-chain contract where gas cost
scales with computational complexity, but worth flagging as a
simplification: real credit curves also price in *loan size* and
*borrower-specific default probability*, both of which this protocol
handles separately (via `risk_score` and slashing) rather than folding into
rate.

## 3. Risk Model and Default Assumptions

Default handling has three layers:

1. **Slashing** (`slash`, `auto_slash`): on default, each voucher's stake
   is reduced by `slash_bps` (`DEFAULT_SLASH_BPS = 5000`, i.e. 50%). The
   remaining 50% is returned to the voucher; the slashed 50% flows to the
   protocol's slash treasury. This is the primary loss-absorption
   mechanism and implicitly assumes that a 50% haircut is sufficient
   deterrent against reckless vouching without being so punitive that it
   discourages vouching altogether — a calibration, not a derived
   optimum, and a natural candidate for the sensitivity analysis in §5.
2. **Credit scoring** (`src/credit_score.rs` — referenced from `lib.rs` via
   `update_credit_score`): borrower repayment/default history feeds into a
   score used to gate future loan eligibility, functioning as a
   reputation-based analogue to a credit bureau score.
3. **Tranching / subordination** (new in this PR, `src/loan_priority.rs`):
   default proceeds recovered from a slashed pool of loans are no longer
   split pro-rata across all loans; Senior tranche loans are paid to par
   first. This changes the *implicit* default assumption from "all loans
   are equally risky" to "risk is explicitly priced per tranche," which is
   more accurate for a protocol whose loans vary widely in borrower
   history and vouch depth.

**Core default assumption:** the model assumes defaults are largely
*idiosyncratic* (borrower-specific) rather than *systemic* (correlated
across many borrowers simultaneously). The slashing and tranching
mechanisms both operate per-borrower/per-loan and do not model contagion —
e.g. there is no explicit stress scenario for "many borrowers vouched by
overlapping voucher sets default in the same period." This is a reasonable
starting assumption for a peer-vouching network bootstrapped from
personal/community trust relationships, but it should be revisited as the
protocol scales and voucher concentration risk grows (see
`get_portfolio_risk` in `vouch.rs`, which already surfaces per-voucher
concentration for exactly this reason).

## 4. Comparative Analysis with Traditional Lending

| Dimension | QuorumCredit | Traditional over-collateralized DeFi (Aave/Compound) | Traditional unsecured bank lending |
|---|---|---|---|
| Collateral basis | Peer stake (reputational), up to 1.5x loan value | Same-protocol crypto collateral, typically 125-150%+ | None (credit score + income underwriting) |
| Default recourse | Slash voucher stake (50% haircut) + credit score damage | Automatic liquidation of collateral | Collections, credit bureau reporting, legal action |
| Capital efficiency for borrower | High — no need to lock >100% of loan value | Low — must over-collateralize | High, but requires off-chain identity/credit history |
| Priority structure | Configurable Senior/Mezzanine/Junior waterfall (this PR) | Typically flat (all suppliers pro-rata) | Common — secured vs. unsecured tranches, syndicated loan tranching |
| Underwriting basis | Social vouching + on-chain history | None (over-collateralization substitutes for underwriting) | Centralized credit scoring, income verification |
| Interest rate model | Flat rate + tiered bonuses (this doc, §2) | Continuous utilization-based curve | Risk-based pricing from centralized underwriting |
| Sybil resistance | Economic (`estimate_sybil_attack_cost`) | N/A (over-collateralization makes sybil attacks self-defeating) | Off-chain KYC |

The closest traditional analogue to QuorumCredit's design is a **mutual
guarantee society / credit union with tranched participation certificates**
rather than a bank or a DeFi money market: capital comes from members
vouching for each other, losses are socialized within the vouching
relationship (not the whole pool) via slashing, and — as of this PR — risk
appetite can be explicitly stratified by tranche.

## 5. Sensitivity Analysis for Key Parameters

The table below sketches the qualitative direction of effect for the
protocol's key economic levers. This is intended as a starting point for
future quantitative modeling (e.g. Monte Carlo simulation over historical
default rates), not a substitute for it.

| Parameter | Default | Increase → | Decrease → |
|---|---|---|---|
| `yield_bps` (base yield) | 200 (2%) | More voucher participation, but compresses protocol margin / requires higher borrower rates elsewhere | Fewer vouchers willing to stake, tighter liquidity |
| `slash_bps` (default penalty) | 5000 (50%) | Stronger deterrent against reckless vouching, but discourages vouching for higher-risk (still legitimate) borrowers | Weaker deterrent, higher expected losses per default, but more vouching activity at the margin |
| `max_loan_to_stake_ratio` | 150% | More capital-efficient for borrowers, but increases loss-given-default per unit of vouched stake | More conservative, safer, but reduces borrower access |
| `liquidity_mining_rate_bps` | 50 | Faster voucher bootstrapping, but is a pure protocol cost with no offsetting revenue unless funded from treasury/token emissions | Slower growth, but avoids subsidizing yield with unsustainable emissions |
| Large-loan multi-sig threshold (this PR, `DEFAULT_LARGE_LOAN_THRESHOLD` = 50,000 USDC-equiv) | 50,000 | Fewer loans require multi-sig, faster disbursement, less friction — but larger single-admin blast radius | More loans gated behind multi-sig, slower disbursement, but caps the loss any single compromised admin key can cause |
| Senior tranche share of a pool (this PR) | operator-configured | Safer for senior stakers, lower yield for them; junior stakers bear more first-loss risk for higher yield | Flatter risk profile, closer to the pre-tranching equal-priority model |
| `admin_threshold` (base multi-sig) | protocol-configured | More resistant to a single compromised/malicious admin, slower operational response | Faster operations, higher single-point-of-failure risk |

**Key interaction to watch:** `slash_bps` and `max_loan_to_stake_ratio`
move loss-given-default in opposite directions relative to borrower access
— a protocol tuning for growth (higher ratio, lower slash) is implicitly
accepting higher expected loss per defaulted loan, and should size the
slash treasury / tranche waterfall thresholds accordingly. The tranching
mechanism introduced in this PR is the primary tool for absorbing that
tradeoff without changing the two base parameters: it lets risk-tolerant
capital (Junior) subsidize risk-averse capital (Senior) within the same
pool, rather than forcing a single blended slash/ratio setting for
everyone.

## References

- `src/loan_priority.rs` — priority queue and default waterfall (Issue:
  loan priority / subordination)
- `src/large_loan_approval.rs` — multi-signature large-loan approval queue
- `docs/economic-security-model.md` — attacker-cost-focused companion
  document
- `src/credit_score.rs`, `src/maturity.rs`, `vouch::get_portfolio_risk` —
  supporting risk and reputation mechanisms referenced above
