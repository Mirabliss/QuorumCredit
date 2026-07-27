# Governance Manual

## Purpose

This manual standardizes how governance decisions are made in QuorumCredit: who can propose
and vote, how proposals move through their lifecycle, how long each stage takes, what
thresholds apply, and where past decisions are recorded. It complements, rather than
replaces, the mechanics documented in [`governance-queue-guide.md`](./governance-queue-guide.md)
(the admin action queue) and [ADR 0005](./adr/0005-multisig-admin-and-governance.md) (why the
protocol uses a multisig/governance model at all). Where this manual and the code disagree,
the code (`src/governance.rs`, `src/admin.rs`, `src/types.rs`) is authoritative — file an
issue to reconcile the docs.

## Governance Roles and Permissions

| Role | How assigned | Permissions |
|---|---|---|
| **Admin** | Included in `Config.admins` at `initialize()`, or added via `add_admin` (requires existing admin-threshold approval) | Propose and approve/reject governance actions; execute proposals once approved and past timelock; participate in slash votes; hold one signing key toward `admin_threshold` |
| **Deployer** | The address that called `initialize()`; stored once in `DataKey::Deployer` | Historical/audit marker only — no standing privileged capability beyond what `Config.admins` grants. Not a bypass of the admin threshold. |
| **Oracle** | Set via `Config.oracle_address` (an admin-gated config value) | Call `verify_repayment` and `set_oracle_price`. Not part of the admin set unless separately added as one. |
| **Voucher** | Anyone who has an active vouch on a borrower | Vote on slash proposals affecting a borrower they've vouched for (`SlashVote`); no admin/config authority |
| **Borrower** | Anyone with an active or historical loan | No governance authority; subject to governance outcomes (config changes, slashing) |
| **RBAC permission holders** | Granted fine-grained capability bits via `src/rbac.rs` (independent of the admin set) | Scoped capabilities (e.g. `can_request_loan`) that are not governance actions but are also access-controlled |

**Key distinction:** "Admin" is a set of addresses with a **threshold** (`Config.admin_threshold`),
not a single privileged key. Every governance action below requires that many admin approvals,
not just one — see [ADR 0005](./adr/0005-multisig-admin-and-governance.md) for why.

## Proposal Creation and Voting Procedure

1. **Draft.** Any admin identifies a needed change (config update, admin add/remove/rotate,
   pause/unpause, upgrade, etc.) and drafts the corresponding `GovernanceAction` (see the
   full list in [`governance-queue-guide.md`](./governance-queue-guide.md#governance-actions)).
2. **Propose.** The drafting admin submits the action on-chain, creating a
   `GovernanceProposal` in `GovernanceProposalStatus::Pending` with a `description` explaining
   the rationale. Off-chain, the same rationale should be posted wherever the team tracks
   governance discussion (issue tracker, forum, etc.) so voting admins have context before
   approving.
3. **Approve or reject.** Other admins call the approve/reject entry point. Approvals and
   rejections are recorded on the proposal (`approvals`, `rejections`). A proposal becomes
   `Approved` once `approvals.len() >= Config.admin_threshold`.
4. **Timelock wait.** An `Approved` proposal is not immediately executable — it must wait
   until `executable_at` (creation time + the governance timelock delay; see Timelines below).
   This gives affected parties (vouchers, borrowers) visibility before the change takes
   effect.
5. **Execute.** Any admin may call execute once `now >= executable_at` and
   `now <= expires_at`. Execution applies the `GovernanceAction` to contract state and marks
   the proposal `Executed`.
6. **Cancel (optional, any stage before execution).** The original proposer, or any admin,
   may cancel a `Pending` or `Approved` proposal, moving it to `Cancelled`. Use this when
   circumstances change before execution (e.g. the underlying need disappears, or a better
   proposal supersedes it).
7. **Expire (automatic).** If a proposal is not executed by `expires_at`, it lapses to
   `Expired` and must be re-proposed from scratch if still needed.

### Special case: Slash proposals

Slashing a borrower's vouches is high-impact and time-sensitive, so it has its own
threshold and cooldown rather than following the general admin-threshold proposal flow
one-for-one:

- A slash vote is tracked per-borrower (`SlashVote`) and passes at `SlashVoteQuorum` (basis
  points of eligible voucher stake), not the admin threshold.
- Once a borrower has been slashed, a new slash proposal against the same borrower cannot be
  initiated again until `DEFAULT_SLASH_PROPOSAL_COOLDOWN_SECS` (7 days) has elapsed — this
  prevents repeated harassment slashing of the same account.
- `auto_slash` is permissionless-but-deadline-gated: anyone may call it, but only after
  `loan.deadline` has passed, so it does not require a governance vote at all.

### Special case: Repayment confirmation

When `Config.confirmation_required` is enabled, borrowers must call `confirm_repayment`
before `repay` — this is a borrower-side procedural step, not a governance vote, but is
documented here because it is a config value (`SetConfirmationRequired`) that itself goes
through the standard governance flow to enable/disable.

## Timelines by Proposal Type

| Proposal type | Approval requirement | Timelock delay before executable | Execution window | Source |
|---|---|---|---|---|
| Standard governance action (config changes, pause/unpause, admin add/remove/rotate, upgrade, token allow-list, etc.) | `admin_threshold` approvals | `DEFAULT_GOVERNANCE_TIMELOCK_DELAY` = 24 hours | `DEFAULT_GOVERNANCE_EXECUTION_WINDOW` = 7 days after becoming executable | `src/types.rs` |
| Admin action queue entries (`src/admin.rs` timelocked actions) | `admin_threshold` approvals | `TIMELOCK_DELAY` = 24 hours | 7 days (see `governance-queue-guide.md`) | `src/types.rs` |
| Slash proposal (per-borrower) | `SlashVoteQuorum` bps of eligible voucher stake | None beyond vote collection; executes once quorum is met | Re-proposal cooldown: `DEFAULT_SLASH_PROPOSAL_COOLDOWN_SECS` = 7 days after a completed slash | `src/types.rs` |
| Auto-slash (deadline default) | None (permissionless) | Loan `deadline` must have passed | N/A | `src/lib.rs` |
| Withdrawal requests (voucher-side, not admin governance but timelocked similarly) | N/A | `WITHDRAWAL_TIMELOCK_DELAY` = 24 hours | N/A | `src/types.rs` |
| Vouch cooldown between successive vouches by the same voucher | N/A | `DEFAULT_VOUCH_COOLDOWN_SECS` = 24 hours | N/A | `src/types.rs` |
| Voting period referenced by `Config.voting_period_seconds` (general-purpose governance vote window, where applicable) | N/A | `DEFAULT_VOTING_PERIOD_SECONDS` = 7 days | N/A | `src/types.rs` |

**Operational guidance:** treat the 24-hour timelock as a minimum, not a target. For
high-impact changes (yield/slash rate changes, admin set changes, upgrades), plan for the
full 7-day execution window to allow time for community/stakeholder review between approval
and execution, even though the action becomes technically executable after 24 hours.

## Quorum and Vote Thresholds

- **Admin governance quorum:** `Config.admin_threshold` — an absolute count of admin
  approvals (not a percentage of the admin set). Set at `initialize()` and itself changeable
  only via a `SetAdminThreshold` governance action.
- **Admin removal:** governed separately by `Config.removal_vote_threshold`, so that removing
  a (potentially compromised or unresponsive) admin does not require that admin's own
  cooperation, while still requiring meaningful consensus among the rest.
- **Slash vote quorum:** `SlashVoteQuorum`, expressed in basis points (e.g. `5000` = 50%) of
  eligible voucher-weighted stake for the affected borrower, not a fixed admin count.
- **Confirmation threshold for repayment (`confirmation_required`):** binary, not a quorum —
  either the borrower's confirmation is required before `repay` succeeds, or it isn't.

**Choosing thresholds:** `admin_threshold` should always be strictly less than the total
number of admins (never require unanimity in a set where losing one key means permanent
lockout) and should be high enough that no minority collusion (e.g. 1-of-5) can act alone.
A common pattern is `ceil(2/3 * N)` admins for `N` total admins.

## Assumptions and Trust Model

Governance in this protocol assumes admin-set honesty-in-aggregate below the threshold, and
oracle honesty for oracle-gated actions — see the "Trust Model and Assumptions" section of
[`threat-model.md`](./threat-model.md#trust-model-and-assumptions) for the full statement of
what is and is not cryptographically enforced.

## Historical Governance Decisions Log

This section is the durable record of governance decisions that changed protocol behavior or
parameters. Append new entries chronologically; do not edit or delete past entries — if a
decision is reversed, log the reversal as a new entry that references the original.

| Date | Decision | Rationale | Reference |
|---|---|---|---|
| 2026-04-25 | Adopted multisig admin + governance model for critical actions (pause, upgrade, slashing, config) | Single-key administration was a centralization/single-point-of-compromise risk | [ADR 0005](./adr/0005-multisig-admin-and-governance.md) |
| — | Adopted Admin Governance Queue with 24-hour timelock and 7-day execution window for all standard admin actions | Gives vouchers/borrowers visibility into pending changes before they take effect | [`governance-queue-guide.md`](./governance-queue-guide.md) |
| — | Adopted per-borrower slash vote (quorum in basis points of voucher stake) rather than folding slashing into the standard admin proposal flow | Slashing needed to be responsive to voucher sentiment on a specific borrower, not gated purely by admin count | `src/vouch.rs`, `SlashVote` |
| — | Added 7-day cooldown between successive slash proposals against the same borrower | Prevent repeated/harassment slash attempts against the same account | `DEFAULT_SLASH_PROPOSAL_COOLDOWN_SECS`, `src/types.rs` |
| — | Added two-step (successor) admin transfer instead of direct reassignment | Prevent accidental lockout from a single faulty admin-transfer transaction | `Config.successor_admin`, `src/admin.rs` |

> Entries without a specific date above predate this manual's creation and are recorded from
> the existing code/ADR history; backfill exact dates from `git log` on the referenced files
> where precision matters for an audit.

## Amending This Manual

Changes to governance **procedure** (this document) do not themselves require an on-chain
governance action — they're a documentation change like any other PR. Changes to governance
**parameters** (thresholds, timelock durations, quorum) do require the corresponding
`GovernanceAction` (e.g. `SetAdminThreshold`) to actually take effect on-chain; update this
manual's tables in the same PR that changes the on-chain default, so the two never drift.

## References

- [Admin Governance Queue Guide](./governance-queue-guide.md)
- [ADR 0005: Multisig Admin and Governance](./adr/0005-multisig-admin-and-governance.md)
- [Threat Model](./threat-model.md)
- [Security Best Practices](./security-best-practices.md)
