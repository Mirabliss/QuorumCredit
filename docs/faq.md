# QuorumCredit FAQ

Frequently asked questions for users, developers, and operators.

## Table of Contents

- [General Questions](#general-questions)
- [For Borrowers](#for-borrowers)
- [For Vouchers](#for-vouchers)
- [For Developers](#for-developers)
- [For Operators](#for-operators)
- [Technical Questions](#technical-questions)

---

## General Questions

### What is QuorumCredit?

QuorumCredit is a decentralized microlending platform on Stellar Soroban that replaces traditional asset collateral with **social collateral**. Instead of locking up $100 to borrow $50, borrowers get vouched for by their trust network. Vouchers stake XLM to back borrowers they trust, earning 2% yield on successful repayment.

### How is QuorumCredit different from traditional lending?

| Aspect | Traditional | QuorumCredit |
|--------|-------------|--------------|
| Collateral | Asset-based (over-collateralized) | Social (trust-based) |
| Access | Credit score required | Trust network required |
| Fees | High (1-5% origination) | Low (protocol fees only) |
| Speed | Days to weeks | Minutes |
| Transparency | Opaque | On-chain, auditable |

### Is QuorumCredit safe?

QuorumCredit has undergone security audits and implements multiple safeguards:
- Multi-sig admin controls
- Pause mechanism for emergencies
- Slash mechanism to penalize defaults
- Yield reserve pre-funding to ensure solvency
- Comprehensive error handling

See [SECURITY.md](../SECURITY.md) for details.

### What blockchain does QuorumCredit use?

QuorumCredit is built on **Stellar Soroban**, a smart contract platform on the Stellar blockchain. Stellar offers:
- Near-zero transaction fees
- Fast finality (~5 seconds)
- Native XLM token
- Federated Byzantine Agreement (FBA) consensus

### What tokens does QuorumCredit support?

- **Primary token**: XLM (Stellar's native asset)
- **Additional tokens**: Any SEP-41-compliant token approved by admins

---

## For Borrowers

### How do I get a loan?

1. **Build your trust network**: Ask friends, family, or community members to vouch for you
2. **Recruit vouchers**: Each voucher stakes XLM to back your loan
3. **Meet the threshold**: Once total vouched stake meets your requested threshold, you're eligible
4. **Request the loan**: Call `request_loan()` with your desired amount
5. **Receive funds**: Loan is disbursed to your wallet immediately

### What's the minimum loan amount?

The default minimum is **100,000 stroops (0.01 XLM)**. Admins can adjust this via `set_min_loan_amount()`.

### What's the maximum loan amount?

The default maximum is **10,000,000,000 stroops (1,000 XLM)**. Admins can adjust this via `set_max_loan_amount()`.

### How long do I have to repay?

The default loan duration is **30 days**. After the deadline, the loan is considered in default and vouchers can vote to slash your collateral.

### What happens if I don't repay?

1. **After deadline**: Loan enters default status
2. **Vouchers vote**: Vouchers can vote to slash your collateral
3. **Slash executed**: 50% of each voucher's stake is burned
4. **Blacklist**: You may be blacklisted from future borrowing
5. **Credit impact**: Your default count increases, affecting future eligibility

### Can I repay early?

Yes! You can repay at any time before the deadline. Early repayment is encouraged and doesn't incur penalties.

### Can I repay partially?

Yes, you can make partial repayments. The loan remains active until fully repaid.

### What if I need more time?

Contact your vouchers to discuss options:
- **Refinance**: Request a new loan to pay off the old one
- **Extend deadline**: Vouchers may agree to extend (requires admin support)
- **Negotiate**: Work with vouchers on alternative arrangements

### How is my credit score calculated?

Your credit score is based on:
- **Repayment history**: On-time repayments increase score
- **Default count**: Defaults decrease score
- **Loan amount**: Larger loans increase score if repaid
- **Voucher count**: More vouchers increase score

See [Reputation System](../docs/reputation-system.md) for details.

---

## For Vouchers

### Why should I vouch for someone?

**Benefits**:
- **Earn yield**: 2% annual yield on staked XLM
- **Help your community**: Enable access to credit for underserved populations
- **Low risk**: Diversify across multiple borrowers
- **Transparent**: On-chain, auditable transactions

### How much should I stake?

This depends on:
- **Your risk tolerance**: Higher stake = higher potential loss if default
- **Borrower trustworthiness**: More trusted borrowers warrant higher stakes
- **Diversification**: Spread stakes across multiple borrowers
- **Minimum requirement**: At least 50 stroops to earn non-zero yield

**Example**: Stake 1 XLM (10,000,000 stroops) across 5 borrowers = 200,000 stroops each.

### What's the minimum stake?

**50 stroops (0.000005 XLM)** is the minimum to earn non-zero yield. Smaller stakes will not generate yield due to rounding.

### Can I increase my stake?

Yes! Use `increase_stake()` to add more XLM to an existing vouch. This increases the borrower's eligibility and your potential yield.

### Can I decrease my stake?

Yes! Use `decrease_stake()` to reduce your stake, but it must remain above the minimum (50 stroops). You cannot decrease below the minimum.

### Can I withdraw my vouch?

Yes! Use `withdraw_vouch()` to completely remove your vouch and get your stake back. However:
- **Cannot withdraw during active loan**: Your stake is locked while the borrower has an active loan
- **Can withdraw after repayment**: Once the loan is repaid, you can withdraw

### What happens if the borrower defaults?

1. **Loan enters default**: After deadline without repayment
2. **Vouchers vote**: You can vote to slash the borrower's collateral
3. **Slash executed**: If quorum is met, 50% of your stake is burned
4. **Remaining stake**: You keep 50% of your original stake

**Example**: You stake 1 XLM, borrower defaults, you lose 0.5 XLM.

### How do I vote on a slash?

1. **Initiate slash vote**: Admin or any voucher calls `initiate_slash_vote(borrower)`
2. **Cast your vote**: Call `vote_slash(voucher, borrower, approve)` with `approve=true` or `false`
3. **Quorum check**: Once 50% of total stake votes to approve, slash executes automatically

### When do I receive my yield?

Yield is distributed when the borrower repays the loan:
- **Timing**: Immediately upon successful repayment
- **Amount**: `stake * 2% = stake * 200 / 10_000`
- **Minimum**: Stake must be ≥ 50 stroops to receive non-zero yield

**Example**: You stake 1 XLM (10,000,000 stroops), borrower repays, you receive 10,000,000 + 200,000 = 10,200,000 stroops.

### Can I vouch for multiple borrowers?

Yes! You can vouch for as many borrowers as you want. Each vouch is independent.

### Can I vouch for the same borrower multiple times?

No. You can only have one active vouch per borrower per token. Use `increase_stake()` to add more to an existing vouch.

### Is there a cooldown between vouches?

Yes. By default, there's a **24-hour cooldown** between vouch calls for the same voucher. This prevents rapid stake changes.

---

## For Developers

### How do I integrate QuorumCredit into my app?

1. **Install SDK**: Use the Stellar JavaScript SDK
   ```bash
   npm install stellar-sdk
   ```

2. **Connect to contract**: Initialize contract client
   ```javascript
   const contract = new SorobanClient.Contract(CONTRACT_ID, networkPassphrase);
   ```

3. **Call functions**: Invoke contract methods
   ```javascript
   await contract.vouch(voucher, borrower, stake, token);
   ```

See [API Client Guide](../docs/api-client-guide.md) for detailed examples.

### What's the contract address?

- **Testnet**: Available after deployment (see [Deployment Guide](../docs/deployment-guide.md))
- **Mainnet**: TBD (not yet deployed)

### How do I test locally?

1. **Run tests**: `cargo test` in the QuorumCredit directory
2. **Use testnet**: Deploy to Stellar testnet for integration testing
3. **Mock contract**: Use Soroban SDK's mock environment for unit tests

See [Testing Guide](../docs/testing-guide.md) for details.

### What events does the contract emit?

The contract emits events for:
- `vouch/create` - New vouch created
- `vouch/increase` - Stake increased
- `vouch/decrease` - Stake decreased
- `vouch/withdraw` - Vouch withdrawn
- `loan/request` - Loan requested
- `loan/repay` - Loan repaid
- `loan/slash` - Loan slashed
- `admin/config` - Configuration updated
- `admin/pause` - Contract paused
- `admin/unpause` - Contract unpaused

See [Event Indexing Guide](../docs/event-indexing-guide.md) for details.

### How do I handle errors?

All errors are typed `ContractError` enums. Match on error codes:

```rust
match result {
    Err(ContractError::InsufficientFunds) => { /* handle */ },
    Err(ContractError::ActiveLoanExists) => { /* handle */ },
    // ... other cases
}
```

See [Error Reference](../README.md#error-reference) for all codes.

### Can I fork and modify the contract?

Yes! QuorumCredit is MIT-licensed. You can fork, modify, and deploy your own version. See [CONTRIBUTING.md](../CONTRIBUTING.md) for guidelines.

### How do I report security issues?

Report vulnerabilities privately via [GitHub Security Advisories](https://github.com/QuorumCredit/QuorumCredit/security/advisories/new). Do not open public issues.

---

## For Operators

### How do I deploy the contract?

See [Deployment Guide](../docs/deployment-guide.md) for step-by-step instructions.

### How do I set up multisig admin?

1. **Create admin addresses**: Generate or import multiple admin keypairs
2. **Initialize with admins**: Pass admin addresses and threshold during initialization
3. **Require signatures**: All admin functions require `admin_threshold` signatures

**Example**: 3-of-5 multisig requires 3 out of 5 admins to sign.

### How do I pause the contract?

```bash
stellar contract invoke --id $CONTRACT_ID --fn pause --network testnet \
  --source $ADMIN_SECRET_KEY -- --admin_signers '["'$ADMIN_ADDRESS'"]'
```

### How do I unpause the contract?

```bash
stellar contract invoke --id $CONTRACT_ID --fn unpause --network testnet \
  --source $ADMIN_SECRET_KEY -- --admin_signers '["'$ADMIN_ADDRESS'"]'
```

### How do I update configuration?

```bash
stellar contract invoke --id $CONTRACT_ID --fn update_config --network testnet \
  --source $ADMIN_SECRET_KEY -- \
  --admin_signers '["'$ADMIN_ADDRESS'"]' \
  --yield_bps 300 \
  --slash_bps 5000
```

### How do I upgrade the contract?

See [Deployment Guide - Upgrading](../docs/deployment-guide.md#upgrading-the-contract).

### How do I monitor contract health?

Use the monitoring commands in [Troubleshooting Guide - Monitoring](../docs/troubleshooting-guide.md#monitoring-contract-health).

### How do I handle a security incident?

1. **Pause the contract**: Immediately pause to halt user activity
2. **Investigate**: Determine the scope and impact
3. **Fix**: Deploy patched WASM via `upgrade()`
4. **Communicate**: Notify users and stakeholders
5. **Unpause**: Resume operations once fixed

See [SECURITY.md](../SECURITY.md) for incident response procedures.

### How do I manage the yield reserve?

1. **Pre-fund**: Transfer XLM to contract before loans are disbursed
2. **Monitor**: Check `get_fee_treasury()` regularly
3. **Replenish**: Add more XLM if reserve is depleted
4. **Withdraw**: Use `withdraw_slash_treasury()` to collect slashed funds

---

## Technical Questions

### What's a stroop?

A **stroop** is Stellar's smallest indivisible unit:
- 1 XLM = 10,000,000 stroops
- 1 stroop = 0.0000001 XLM (10⁻⁷ XLM)

All contract amounts are in stroops.

### How is yield calculated?

Yield is calculated as:
```
yield = stake * yield_bps / 10_000
```

Where `yield_bps` is basis points (default: 200 = 2%).

**Example**: 1 XLM (10,000,000 stroops) at 2% = 10,000,000 * 200 / 10_000 = 200,000 stroops.

### How is slash calculated?

Slash is calculated as:
```
slashed = stake * slash_bps / 10_000
```

Where `slash_bps` is basis points (default: 5000 = 50%).

**Example**: 1 XLM (10,000,000 stroops) at 50% = 10,000,000 * 5000 / 10_000 = 5,000,000 stroops lost.

### What's the max loan-to-stake ratio?

The default ratio is **2:1** (borrow up to 2x your total vouched stake). Admins can adjust via `set_max_loan_to_stake_ratio()`.

### Can I use multiple tokens?

Yes! Admins can approve additional SEP-41 tokens via `add_allowed_token()`. Each vouch and loan specifies its token.

### How does the contract handle overflow?

The contract uses `i128` for all amounts and checks for overflow:
- `StakeOverflow` error if summing vouches would overflow
- Safe arithmetic with explicit checks

### Is the contract upgradeable?

Yes! Admins can upgrade the contract WASM via `upgrade()`. This requires `admin_threshold` signatures.

### How do I verify contract integrity?

1. **Check source**: Verify contract source on GitHub
2. **Audit WASM**: Compare deployed WASM hash with built WASM
3. **Monitor events**: Subscribe to contract events for anomalies
4. **Verify state**: Periodically check contract storage for consistency

### What's the contract's audit status?

QuorumCredit has undergone security audits. See [SECURITY.md](../SECURITY.md) for audit reports and findings.

---

## Credit Scores Deep Dive

This section centralizes the questions support channels see most often about how credit scores work, since scoring is the topic borrowers ask about the most.

### What inputs feed into my credit score?

| Input | Effect | Weight |
|-------|--------|--------|
| On-time repayments | Increases score | High |
| Defaults | Decreases score, compounds with count | Very high |
| Loan size repaid | Larger repaid loans increase score more | Medium |
| Active voucher count | More distinct vouchers increases score | Medium |
| Account age | Older, active accounts score slightly higher | Low |

See [Credit Score Guide](credit-score-guide.md) for the full formula and [Credit Score Migration](credit-score-migration.md) for how historical scores are carried across upgrades.

### Why did my score drop after a repayment?

This is one of the most common support tickets. Two explanations account for almost all cases:
1. **A prior default is still in the lookback window.** Defaults have a longer decay period than repayments have a boost period, so a single default can outweigh several on-time repayments for weeks.
2. **You repaid a very small loan.** Small loans contribute a smaller positive delta than the negative delta from an unrelated default or missed deadline elsewhere in your history.

**Example**: You repay a 200,000-stroop loan on time (+small boost), but you defaulted on a separate 5,000,000-stroop loan 10 days ago (−large penalty still active). Net effect: score still trends down until the default ages out.

### Does requesting a loan and not taking it affect my score?

No. Only `request_loan()` calls that result in an actual disbursement are scored. Eligibility checks and quote calls are read-only and never touch your score.

### Can my score go negative or does it floor at zero?

Scores floor at zero. A borrower with repeated defaults will sit at the floor rather than go negative, but will remain ineligible for new loans until enough positive history accrues or the blacklist (if triggered) is cleared by an admin.

### How often does my score recalculate?

Scores are recalculated on-chain at the moment a scoring event occurs (repayment, default, new vouch) — there is no background batch job. If you don't see an update immediately after a transaction, check that the transaction actually confirmed (see [Troubleshooting Guide](troubleshooting-guide.md)) rather than assuming a delay.

### Do vouchers have a credit score too?

Vouchers don't have a borrower-style credit score, but their **vouch history** (number of successful vouches, slash participation, stake retained) is visible on-chain and often used informally by borrowers to judge which vouchers to recruit.

### My score looks wrong — what should I check before filing a ticket?

1. Confirm you're querying the right token — scores can be tracked per-token if your deployment uses multiple SEP-41 tokens.
2. Confirm the transaction that should have changed your score actually succeeded (check the emitted event, not just that you submitted it).
3. Re-read [How is my credit score calculated?](#how-is-my-credit-score-calculated) above — most "bug" reports are the default-decay behavior described there.
4. If it still looks wrong, open a GitHub issue with your account address, the transaction hash, and expected vs. actual score.

---

## Common Support Issues

Questions collected from support channels that don't fit neatly into a single topic above.

### "My transaction succeeded but nothing changed" — what happened?

Almost always one of: (a) you queried a stale RPC node/cache, (b) you're looking at the wrong contract address (testnet vs. mainnet), or (c) the transaction succeeded but hit a no-op branch (e.g., calling `increase_stake()` with the same value). Check the emitted event for the call, not just the transaction status.

### "I can't vouch for this borrower" — why?

The three most common causes, in order of frequency:
1. **Cooldown active** — you vouched/changed stake for this voucher within the last 24 hours (see [Is there a cooldown between vouches?](#is-there-a-cooldown-between-vouches)).
2. **Duplicate vouch** — you already have an active vouch for this borrower/token pair; use `increase_stake()` instead of a new vouch.
3. **Contract paused** — admins have paused the contract for maintenance or an incident; check [How do I pause the contract?](#how-do-i-pause-the-contract).

### "The borrower defaulted but I wasn't slashed" — is that a bug?

No — slashing is not automatic. Someone (an admin or any voucher) must call `initiate_slash_vote()`, and slashing only executes once quorum (50% of total stake voting to approve) is reached. If no one initiates a vote, no slash occurs.

### Where do I go for something not covered here?

- **Detailed guides**: [docs/](.) for deployment, monitoring, upgrades, and integration guides
- **Bug reports**: [GitHub Issues](https://github.com/QuorumCredit/QuorumCredit/issues)
- **Security issues**: See [How do I report security issues?](#how-do-i-report-security-issues) — do not file these publicly

---

## Keeping This FAQ Updated

This FAQ is a living document, not a one-time write-up. To keep it useful:

- When a question comes up more than twice in support channels (Discord, GitHub issues, email), it belongs here.
- New Q&A entries should link to the detailed doc that has the full explanation rather than duplicating it inline — this FAQ should stay skimmable.
- When a contract upgrade changes default values (loan limits, yield/slash bps, cooldown periods), search this file for the old numbers and update them in the same PR as the upgrade.
- Stale answers are worse than missing answers — if you're not sure an answer is still accurate, flag it for review rather than leaving it as-is.

---

## Still Have Questions?

- **Documentation**: https://github.com/QuorumCredit/QuorumCredit/tree/main/docs
- **GitHub Issues**: https://github.com/QuorumCredit/QuorumCredit/issues
- **Discord**: [Stellar Developer Discord](https://discord.gg/stellardev)
- **Email**: support@quorumcredit.dev
