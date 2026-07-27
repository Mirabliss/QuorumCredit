# ADR 0007: Loan Syndication Model

Date: 2026-07-25

## Context

As QuorumCredit grows, larger loans may exceed the capacity or risk tolerance of a single voucher group. Loan syndication allows multiple groups to collectively back a single loan, distributing both risk and yield across a consortium.

## Decision

We implement a loan syndication model that allows:
- Multiple voucher groups to jointly originate and hold a loan
- Proportional risk sharing based on each group's committed capital
- Yield distribution according to each group's stake
- Collective voting rights for loan governance decisions

## Rationale

- **Risk Distribution**: No single group bears full default risk; spreads exposure across multiple entities.
- **Capital Efficiency**: Larger loans become fundable without requiring one group to over-commit.
- **Incentive Alignment**: Each syndicate member's yield is proportional to their stake, aligning risk and reward.
- **Governance Clarity**: Clear rules for voting (e.g., majority, consensus, weighted) prevent deadlock and ensure group autonomy.

## Risk Sharing

- Each syndicate member commits a fixed amount (principal) to the loan.
- Default loss is allocated proportionally to each member's stake.
- Example: If a group holds 40% and the loan defaults at 50%, that group absorbs 40% of the 50% loss.

## Yield Distribution

- Loan yield (interest + fees) is distributed to syndicate members in proportion to their capital commitment.
- Example: If a group contributes $100k of a $250k syndicated loan (40%), it receives 40% of all accrued yield.

## Voting Rights

- **Loan Origination**: All syndicate members must approve (or majority consensus, TBD by governance).
- **Restructuring/Modification**: Weighted vote proportional to capital stake (1 share = 1 vote).
- **Default/Liquidation**: Majority consensus triggers default proceedings.

## Consequences

- Smart contracts must track syndicate membership, stakes, and yield accrual.
- Loan origination flow becomes multi-party; requires coordinated signing or escrow during syndication phase.
- Governance overhead increases; a mechanism for reaching consensus across groups is required.
- Default handling must account for partial recovery and distribute recoveries pro-rata.
