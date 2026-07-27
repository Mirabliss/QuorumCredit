# Credit Score Calculation Algorithm

This document explains, in full detail, how QuorumCredit computes an on-chain
credit score for a borrower. It is intended for borrowers, integrators, and
auditors who want to understand *exactly* how a score is produced, not just
what the score means. For a higher-level product overview see
[`credit-score-guide.md`](./credit-score-guide.md); for migration notes on
score-version changes see [`credit-score-migration.md`](./credit-score-migration.md).

The implementation lives in [`src/credit_score.rs`](../src/credit_score.rs),
primarily in `calculate_credit_score`, `calculate_tier`, and the individual
`calculate_*_score` helper functions referenced below.

## 1. Overview

A credit score is a single `u32` value produced by combining five independent
sub-scores ("factors"), each weighted in basis points (bps) that must sum to
`10000` (100%). The weighted sum determines both the numeric score and the
`CreditTier` the borrower falls into, which in turn drives yield, max loan
size, minimum stake requirements, loan duration, and protocol fee via the
`apply_tier_rewards_to_*` functions.

```
score = (repayment_history_score * repayment_history_weight
       +  loan_count_score        * loan_count_weight
       +  account_age_score       * account_age_weight
       +  vouching_score          * vouching_weight
       +  timeliness_score        * timeliness_weight) / 10000
```

Each component score is itself normalized to a 0–100 range before weighting,
so the final score is also bounded to roughly 0–100 regardless of how the
weights are configured. Weights are governance-configurable via
`set_credit_score_config`, which enforces the `total_weight == 10000`
invariant before accepting a new configuration.

## 2. Scoring Components

### 2.1 Repayment History (`calculate_repayment_history_score`)

Measures the borrower's track record of repaying loans in full versus
defaulting or being slashed. Conceptually:

- Borrowers with no history start at a neutral baseline (new borrowers are
  not penalized for lack of history — that's covered separately by account
  age and loan count).
- Each successfully repaid loan increases the score; each default/slash
  event decreases it, with defaults weighted more heavily than a single
  repayment increases it (asymmetric penalty, consistent with how
  traditional bureaus treat delinquency).
- The score saturates — a borrower with a long clean history and one with an
  extremely long clean history both approach the same ceiling, since
  marginal trust gained from the 50th consecutive repayment is small
  compared to the 5th.

**Default weight:** this is typically the single largest weight in the
default configuration, because repayment behavior is the most direct signal
of future repayment behavior.

### 2.2 Loan Count (`calculate_loan_count_score`)

Rewards borrowers for having completed a meaningful number of loans, since a
single repaid loan is a much weaker signal than a dozen. The function maps
`total_loans` to a 0–100 score with diminishing returns — the jump from 0 to
1 completed loans matters more than the jump from 20 to 21. This prevents a
borrower from being permanently capped at "unproven" after their first loan
while also not letting sheer loan *volume* alone dominate the score (that is
what repayment history and timeliness are for).

### 2.3 Account Age (`calculate_account_age_score`)

Uses `account_age` (time since the account/address was first seen by the
protocol) to produce a 0–100 score. Older accounts score higher, again with
diminishing returns, because account longevity is a weak but useful
anti-Sybil signal on its own — freshly created addresses are cheap to mint,
but an address with months or years of on-chain history is comparatively
more expensive for an attacker to fabricate at scale.

### 2.4 Vouching (`calculate_vouching_score`) — Sybil-Resistant Stake-Time Weighting

This is the most sophisticated component and the primary Sybil-resistance
mechanism in the scoring system. Rather than simply counting how many
vouches a borrower has, or summing raw stake, the algorithm computes a
**stake-time weight** per vouch:

1. A vouch only counts if the voucher's stake is at least
   `SYBIL_MIN_STAKE_FOR_CREDIT` (1,000,000 stroops) and the vouch itself is
   at least `SYBIL_MIN_VOUCH_AGE_SECS` (24 hours) old. This closes the
   obvious attack of spinning up many freshly-funded, freshly-created
   accounts to vouch for one borrower right before a loan request.
2. For each qualifying vouch, compute
   `weight = stake_xlm * age_days` (stake in whole-XLM units multiplied by
   the vouch's age in days).
3. Apply diminishing returns via `sqrt(weight)` rather than using the raw
   weight. This means a single voucher with a huge stake cannot single-
   handedly max out the vouching score — trust from *many independent*
   vouchers is worth more than the same total stake concentrated in one
   voucher, which mirrors how vouching/trust networks are meant to work.
4. Each vouch's contribution is capped at `SYBIL_STAKE_TIME_SATURATION`
   (100) before being summed, and the total is capped again at an overall
   saturation ceiling. This bounds the maximum influence of both a single
   large voucher and of an unbounded number of vouchers.
5. `integer_sqrt_u64` implements an integer (non-floating-point) square root
   so the calculation stays fully deterministic on-chain — a requirement
   for smart contract execution, where floating point is unavailable and
   non-determinism across nodes is unacceptable.

A legacy code path (`env: None`) exists for backward compatibility with
scores computed before the stake-time weighting model was introduced; see
`credit-score-migration.md` for how existing scores are handled across that
transition.

### 2.5 Timeliness (`calculate_timeliness_score`)

Uses `avg_repayment_time` (how early/late, on average, a borrower repays
relative to the due date) to produce a 0–100 score. Repaying early or
on-time scores highest; the score degrades as the average repayment time
moves later, distinct from repayment *history* (which only tracks
repaid-vs-defaulted, not *when* within the loan term the repayment happened).
This lets the protocol distinguish a borrower who always repays right before
the deadline from one who repays consistently early, even if both have a
perfect repayment history score.

## 3. Worked Examples

The examples below use illustrative weights of:
`repayment_history_weight = 3500`, `loan_count_weight = 1500`,
`account_age_weight = 1500`, `vouching_weight = 2500`,
`timeliness_weight = 1000` (sums to 10000). Actual on-chain weights are
whatever governance has configured via `set_credit_score_config` — check
`get_credit_score_config_view` for the live values.

### Example A — New borrower, well-vouched

- Repayment history: no loans yet → baseline score `40`
- Loan count: 0 loans → `0`
- Account age: 10 days old → `5`
- Vouching: two vouchers, each staking 5,000 XLM for 30 days
  (`weight = 5000 * 30 = 150000`, `sqrt(150000) ≈ 387`, capped at 100 per
  vouch) → summed and capped → `95`
- Timeliness: no repayments yet → baseline `40`

```
score = (40*3500 + 0*1500 + 5*1500 + 95*2500 + 40*1000) / 10000
      = (140000 + 0 + 7500 + 237500 + 40000) / 10000
      = 425000 / 10000 = 42
```

Result: **score 42** — a new but well-vouched borrower sits in a low-middle
tier, reflecting that strong vouching alone cannot substitute for a proven
repayment record.

### Example B — Established, reliable borrower

- Repayment history: 15 loans repaid, 0 defaults → `92`
- Loan count: 15 loans → `78`
- Account age: 400 days → `70`
- Vouching: one long-standing voucher, 2,000 XLM staked for 200 days
  (`weight = 2000*200 = 400000`, sqrt capped at 100) → `88` (fewer
  independent vouchers than Example A, so slightly lower)
- Timeliness: consistently repays 2 days early on average → `90`

```
score = (92*3500 + 78*1500 + 70*1500 + 88*2500 + 90*1000) / 10000
      = (322000 + 117000 + 105000 + 220000 + 90000) / 10000
      = 854000 / 10000 = 85
```

Result: **score 85** — a mature, reliable borrower with a strong but not
maximal vouching network lands in a high tier.

### Example C — Borrower with a recent default

- Repayment history: 8 loans repaid, 1 recent default → `55` (defaults
  penalized more heavily than an equivalent repayment credits)
- Loan count: 9 loans → `62`
- Account age: 250 days → `55`
- Vouching: three vouchers, moderate stake/age → `70`
- Timeliness: last few repayments were late → `35`

```
score = (55*3500 + 62*1500 + 55*1500 + 70*2500 + 35*1000) / 10000
      = (192500 + 93000 + 82500 + 175000 + 35000) / 10000
      = 578000 / 10000 = 58
```

Result: **score 58** — a single recent default combined with late
repayments pulls a previously-established borrower down into a
mid-tier score, illustrating that the score reacts to *recent* behavior
rather than only lifetime totals.

## 4. Score Ranges and Tier Implications

`calculate_tier` maps the final numeric score into a `CreditTier`. Illustrative
ranges (exact cutoffs are defined in `calculate_tier` and may be tuned by
governance):

| Score range | Tier        | Typical implications via `apply_tier_rewards_to_*` |
|-------------|-------------|-----------------------------------------------------|
| 0–29        | Untrusted   | Highest minimum stake required, smallest max loan, shortest duration, highest protocol fee, no yield boost |
| 30–49       | Emerging    | Elevated stake requirement, modest max loan increase |
| 50–69       | Established | Standard stake requirement, standard loan terms, small yield boost |
| 70–89       | Trusted     | Reduced stake requirement, larger max loan, longer duration, reduced fee |
| 90–100      | Elite       | Lowest stake requirement, largest max loan, longest duration, lowest fee, highest yield boost |

Practical implications for a borrower moving up a tier:
- **Lower required collateral/stake** to request the same size loan.
- **Larger maximum loan size** available without additional vouching.
- **Longer repayment windows**, reducing repayment pressure.
- **Lower protocol fees**, improving effective borrowing cost.
- **Yield boosts** for lenders/stakers associated with higher-tier borrower pools.

## 5. Benchmarking vs. Traditional Credit Scores

| Dimension | Traditional (e.g. FICO) | QuorumCredit |
|---|---|---|
| Data source | Off-chain bureau data: credit cards, mortgages, inquiries, public records | Fully on-chain: this protocol's own loan history, vouching graph, account age |
| Score range | 300–850 | 0–100 (normalized) |
| Update latency | Reporting cycles, typically 30+ days | Recomputed on-demand per transaction (`update_credit_score`), effectively real-time |
| Sybil/identity assumptions | Backed by legal identity (SSN, KYC) | No identity assumption; Sybil resistance comes from stake-time-weighted vouching and account-age costs, not KYC |
| Portability | Siloed per bureau/country | Fully portable and composable on-chain; readable by any integrating protocol via `get_credit_score` |
| Transparency of formula | Proprietary, largely undisclosed weighting | Fully open-source and on-chain; weights are queryable via `get_credit_score_config_view` and changeable only through governance |
| Manipulation resistance | Identity theft, synthetic identities | Economic cost via minimum stake + time-locks on vouches (`SYBIL_MIN_STAKE_FOR_CREDIT`, `SYBIL_MIN_VOUCH_AGE_SECS`) rather than identity verification |
| Governance | Regulatory bodies, bureau policy | On-chain governance vote required to change weights (`set_credit_score_config`) |

The key structural difference is that traditional scores rely on
**identity-backed** data collected passively over months, while
QuorumCredit relies on **economically-backed, real-time** on-chain data:
trust is earned by staking capital and time, not by a third party
vouching for your legal identity. This makes the score faster to build for
active protocol participants, but means it cannot (by design) reflect a
borrower's off-chain financial standing.
