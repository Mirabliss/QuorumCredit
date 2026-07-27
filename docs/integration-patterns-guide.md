# Integration Patterns Guide

Developers integrating with QuorumCredit have generally had to reverse-engineer
which calls matter for their use case from the full function reference in
[api-client-guide.md](./api-client-guide.md). This guide is organized the
other way around: by the role you're building for. Pick your pattern, copy
the example, and follow the links out to the deeper reference docs.

## Table of Contents

- [Choosing your integration pattern](#choosing-your-integration-pattern)
- [Pattern: Borrower](#pattern-borrower)
- [Pattern: Voucher](#pattern-voucher)
- [Pattern: Auditor](#pattern-auditor)
- [Pattern: Admin](#pattern-admin)
- [Error Handling and Retry](#error-handling-and-retry)
- [Rate Limit Guidance](#rate-limit-guidance)
- [Performance Optimization Tips](#performance-optimization-tips)

---

## Choosing your integration pattern

| Pattern | You're building | Primary contract calls | Primary API calls |
|---|---|---|---|
| [Borrower](#pattern-borrower) | A wallet/app that requests and repays loans | `request_loan`, `repay_loan` | `/loans/{id}/forecast`, `/loans/{id}/expenses` |
| [Voucher](#pattern-voucher) | A tool for social-collateral stakers | `vouch`, `withdraw_vouch` | `/webhooks/subscribe` (for vouch/slash events) |
| [Auditor](#pattern-auditor) | Read-only analytics/compliance tooling | none (read-only) | indexer queries, `/metrics`, `GET /api/webhooks/{id}/stats` |
| [Admin](#pattern-admin) | Governance/ops tooling | `initialize`, governance queue calls | `/webhooks/subscribe` (for `default_occurred`) |

All patterns share the [error handling](#error-handling-and-retry) and
[rate limit](#rate-limit-guidance) sections below — read those once
regardless of which pattern you're building.

---

## Pattern: Borrower

Borrower integrations request loans, track repayment, and want to predict
what they'll owe before committing. See
[borrower-app-integration-guide.md](./borrower-app-integration-guide.md) for
the full UX-level walkthrough; this section covers the minimal request/
forecast/repay loop.

### 1. Request a loan (JS)

```javascript
import { Keypair, TransactionBuilder, BASE_FEE, Networks } from "@stellar/stellar-sdk";

async function requestLoan(server, contract, keypair, amount) {
  const account = await server.getAccount(keypair.publicKey());
  const tx = new TransactionBuilder(account, { fee: BASE_FEE, networkPassphrase: Networks.TESTNET })
    .addOperation(contract.call("request_loan", keypair.publicKey(), amount))
    .setTimeout(30)
    .build();
  tx.sign(keypair);
  return server.sendTransaction(tx);
}
```

### 2. Request a loan (Python)

```python
from stellar_sdk import SorobanServer, TransactionBuilder, Network

def request_loan(server: SorobanServer, contract, keypair, amount: int):
    account = server.load_account(keypair.public_key)
    tx = (
        TransactionBuilder(account, network_passphrase=Network.TESTNET_NETWORK_PASSPHRASE)
        .append_invoke_contract_function_op(contract.address, "request_loan", [keypair.public_key, amount])
        .set_timeout(30)
        .build()
    )
    tx.sign(keypair)
    return server.send_transaction(tx)
```

### 3. Request a loan (Rust)

```rust
use soroban_sdk::{Env, Address};

fn request_loan(env: &Env, contract_id: &Address, borrower: &Address, amount: i128) -> Result<(), ContractError> {
    env.invoke_contract(contract_id, &"request_loan".into_val(env), (borrower.clone(), amount).into_val(env))
}
```

### 4. Forecast before committing to a repayment plan

Once a loan is issued, call the forecast endpoint (`server/src/http/forecastRoutes.ts`)
before showing the borrower a repayment schedule:

```javascript
const res = await fetch(`https://api.quorumcredit.example/loans/${loanId}/forecast`);
const { scenarios, earlyRepaymentSavings } = await res.json();
const base = scenarios.find(s => s.scenario === "base");
console.log(`Total repayment under base case: ${base.totalRepayment}`);
```

```python
import requests

res = requests.get(f"https://api.quorumcredit.example/loans/{loan_id}/forecast")
forecast = res.json()
base = next(s for s in forecast["scenarios"] if s["scenario"] == "base")
print(f"Total repayment under base case: {base['totalRepayment']}")
```

### 5. Subscribe to repayment lifecycle events

Rather than polling, register a webhook once (see
[webhook-signature-verification-guide.md](./webhook-signature-verification-guide.md)
for verifying deliveries):

```bash
curl -X POST https://api.quorumcredit.example/webhooks/subscribe \
  -H "content-type: application/json" \
  -d '{"url": "https://yourapp.example/hooks/quorumcredit", "events": ["loan_issued", "payment_received", "loan_completed"]}'
```

---

## Pattern: Voucher

Vouchers stake tokens as social collateral for a borrower and want visibility
into slash risk. Primary calls are `vouch` / `withdraw_vouch`; the relevant
webhook events are `default_occurred` (a vouched-for loan defaulted) plus the
legacy `slash.executed` event.

```javascript
// Subscribe to default events so a voucher UI can surface slash risk in real time
await fetch("https://api.quorumcredit.example/webhooks/subscribe", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ url: "https://yourapp.example/hooks/vouch", events: ["default_occurred"] }),
});
```

```python
import requests

requests.post(
    "https://api.quorumcredit.example/webhooks/subscribe",
    json={"url": "https://yourapp.example/hooks/vouch", "events": ["default_occurred"]},
)
```

See [cross-chain-trust-model.md](./cross-chain-trust-model.md) and
`docs/adr/0003-fba-inspired-trust-model.md` for the trust semantics behind
vouching before building slash-risk UI on top of these events.

---

## Pattern: Auditor

Auditor/compliance tooling is read-only: it consumes indexed events and
service metrics, and never signs transactions. Query the indexer directly
(`services/indexer`) rather than the contract for historical data — replaying
every read through Soroban RPC does not scale for reporting workloads.

```python
import requests

# Scrape service-level metrics (Prometheus text format)
metrics = requests.get("https://api.quorumcredit.example/metrics").text

# Check delivery health for a monitored webhook subscription
stats = requests.get(f"https://api.quorumcredit.example/api/webhooks/{webhook_id}/stats").json()
if stats["successRateBps"] < 9000:
    alert("webhook delivery degraded", stats)
```

```rust
// Rust auditors typically query the indexer's exposed event store directly
// rather than going through the HTTP API — see services/indexer for the schema.
```

See [event-indexing-guide.md](./event-indexing-guide.md) for the indexer
schema and [monitoring-guide.md](./monitoring-guide.md) for what each metric
means.

---

## Pattern: Admin

Admin/governance tooling calls `initialize` once at deployment and thereafter
interacts through the governance queue rather than direct admin calls — see
[governance-queue-guide.md](./governance-queue-guide.md). Admin tooling
should subscribe to `default_occurred` and the legacy `config.updated` event
to track parameter changes and defaults across the whole pool, not a single
loan.

```javascript
await fetch("https://api.quorumcredit.example/webhooks/subscribe", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({
    url: "https://ops.internal.example/hooks/quorumcredit",
    events: ["default_occurred", "loan_completed"],
  }),
});
```

Admin tooling is also the audience for
[incident-response-playbook.md](./incident-response-playbook.md) — if you're
building ops dashboards, wire them to the same signals that playbook uses for
detection (indexer lag, invariant-check failures, webhook success rate).

---

## Error Handling and Retry

All contract calls return `Result<T, ContractError>` with a numeric `code` —
see [api-error-codes.md](./api-error-codes.md) for the full list. HTTP API
errors return `{ "error": string }` with a 4xx/5xx status.

**Retry only on transient failures.** Retrying a `2 ActiveLoanExists` or `1
InsufficientFunds` error will never succeed — surface those to the user
immediately. Retry network-level failures (timeouts, 5xx, RPC connection
resets) with backoff:

```javascript
async function withRetry(fn, { maxRetries = 5, baseDelayMs = 500 } = {}) {
  for (let attempt = 1; attempt <= maxRetries + 1; attempt++) {
    try {
      return await fn();
    } catch (err) {
      const retryable = err.status === undefined || err.status >= 500 || err.code === "ETIMEDOUT";
      if (!retryable || attempt === maxRetries + 1) throw err;
      await new Promise(r => setTimeout(r, baseDelayMs * 2 ** (attempt - 1)));
    }
  }
}
```

```python
import time

def with_retry(fn, max_retries=5, base_delay=0.5):
    for attempt in range(1, max_retries + 2):
        try:
            return fn()
        except TransientError:
            if attempt == max_retries + 1:
                raise
            time.sleep(base_delay * (2 ** (attempt - 1)))
```

This is the same backoff shape (base delay, doubling, capped attempts) that
`server/src/webhooks/delivery.ts` uses for outbound webhook delivery — reuse
it on the client side too, rather than inventing a different curve, so
retries from both ends of a webhook conversation behave predictably.

---

## Rate Limit Guidance

- The broadcast server does not currently enforce per-client rate limits at
  the application layer; treat any reverse-proxy/CDN limit in front of your
  deployment as authoritative and check your deployment's config
  (`production-deployment-guide.md`) for the actual figure.
- Soroban RPC endpoints (testnet and third-party mainnet providers) apply
  their own limits independent of this service — batch reads instead of
  issuing one RPC call per loan/vouch when building list views.
- Webhook subscribers are expected to respond within a reasonable timeout;
  slow subscriber endpoints reduce the effective throughput of the retry
  queue for *all* events destined to that subscriber, not just the slow one.
  Keep webhook receiver handlers fast (enqueue-and-return, don't do
  synchronous processing in the request handler).
- For polling fallbacks (if you can't run a webhook receiver), poll `/loans/{id}/forecast`
  and status endpoints at minute-plus intervals, not sub-second — nothing
  about loan state changes fast enough to justify tighter polling, and it's
  the exact load pattern webhooks exist to replace.

---

## Performance Optimization Tips

1. **Prefer webhooks over polling.** See [webhook-signature-verification-guide.md](./webhook-signature-verification-guide.md)
   for verifying deliveries and the [Pattern: Borrower](#pattern-borrower) example above for subscribing.
2. **Batch indexer reads.** Query the indexer for a range of events rather
   than looping per-entity; see [event-indexing-guide.md](./event-indexing-guide.md).
3. **Cache forecast responses client-side** for the loan's current term —
   `GET /loans/{id}/forecast` recomputes the full amortization schedule on
   every call; it's cheap, but a UI re-rendering a schedule on every
   keystroke of an unrelated form should still cache rather than re-fetch.
4. **Reuse Soroban RPC connections** rather than opening a new one per
   request; connection setup dominates latency for small calls.
5. **Verify webhook signatures cheaply.** HMAC verification
   (`server/src/webhooks/signature.ts`) is O(payload size) and uses
   constant-time comparison — don't add your own additional signature
   scheme on top "for safety"; it adds latency without added protection.

---

## See Also

- [api-client-guide.md](./api-client-guide.md) — full function-level reference
- [borrower-app-integration-guide.md](./borrower-app-integration-guide.md) — deeper borrower UX walkthrough
- [api-client-integration-guide.md](./api-client-integration-guide.md) — broader client integration notes
- [api-error-codes.md](./api-error-codes.md) — full contract error code reference
- [webhook-signature-verification-guide.md](./webhook-signature-verification-guide.md) — verifying inbound webhook signatures
- [incident-response-playbook.md](./incident-response-playbook.md) — what to do when an integration starts failing in production
