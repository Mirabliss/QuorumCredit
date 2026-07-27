# Issue: Centralize FAQ (docs, support, usability)

## What was implemented

`docs/faq.md` already existed with general/borrower/voucher/developer/operator
sections, but lacked a dedicated deep dive on credit scores (the single most
common support question) and had no explicit place for cross-topic support
issues collected from Discord/GitHub/email.

This change adds two new sections to `docs/faq.md`:

1. **Credit Scores Deep Dive** — what inputs feed the score and their
   relative weight, why a score can drop right after a repayment (default
   decay outweighing repayment boost), whether requesting-but-not-taking a
   loan affects score, score flooring behavior, recalculation timing, and a
   checklist to run before filing a "my score looks wrong" ticket.
2. **Common Support Issues** — answers to the three questions that recur
   most in support channels: "my transaction succeeded but nothing
   changed," "I can't vouch for this borrower" (cooldown / duplicate vouch /
   paused contract, in order of frequency), and "the borrower defaulted but
   I wasn't slashed" (slashing requires an explicit vote + quorum, it is not
   automatic).

A **Keeping This FAQ Updated** section was also added, documenting the
process for adding new entries (promote a question once it's been asked
more than twice in support channels, link out to detailed docs rather than
duplicating them, and update default-value answers in the same PR as any
contract upgrade that changes those defaults).

## Why this approach

All answers link to the existing detailed guides (`credit-score-guide.md`,
`credit-score-migration.md`, `troubleshooting-guide.md`) rather than
duplicating their content, keeping the FAQ skimmable while still centralizing
the actual questions users ask.

## Follow-ups not included here

- Wiring a bot to auto-suggest FAQ entries from new support tickets.
- Localizing the FAQ into other languages.
