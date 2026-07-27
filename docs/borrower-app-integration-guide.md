# Borrower App Integration Guide

Complete guide for building applications on top of QuorumCredit protocol.

## Table of Contents

- [Overview](#overview)
- [Prerequisites](#prerequisites)
- [Contract Basics](#contract-basics)
- [Core Operations](#core-operations)
- [Error Handling](#error-handling)
- [Fee Calculation](#fee-calculation)
- [Common Use Cases](#common-use-cases)
- [Code Examples](#code-examples)

## Overview

QuorumCredit enables borrowers to request microloans backed by social collateral (vouches). This guide covers how to integrate with the smart contract from borrower-facing applications.

All monetary amounts use **stroops** (Stellar's smallest unit):
- 1 XLM = 10,000,000 stroops
- Display conversions: `amount_stroops / 10_000_000 = amount_xlm`

## Prerequisites

1. **Soroban SDK** installed and configured
2. **Stellar testnet account** with sufficient XLM
3. **QuorumCredit contract** deployed on your network
4. **Contract ABI** from the QuorumCredit repository

## Contract Basics

### Core Entrypoints

| Function | Purpose | Caller |
|----------|---------|--------|
| `request_loan(amount, threshold, duration_ledgers)` | Request a loan | Borrower |
| `repay(loan_id, amount)` | Repay loan principal/interest | Borrower |
| `query_loan(loan_id)` | Get loan details | Anyone |
| `query_borrower_loans(borrower_id)` | List borrower's loans | Anyone |
| `check_default(loan_id)` | Check if loan defaulted | Anyone |

### Loan States

```
ACTIVE          -> REPAID (full repayment received)
ACTIVE          -> DEFAULTED (expiration + no repayment)
ACTIVE          -> CANCELLED (cancelled before approval/funding)
```

## Core Operations

### 1. Request a Loan

```typescript
// Request a 100 XLM loan with 3 vouchers minimum, 30-day duration
const loanRequest = {
  amount: 100n * 10_000_000n,           // 100 XLM in stroops
  threshold: 3n,                        // Require 3 vouchers
  duration_ledgers: 1_296_000n          // ~30 days (60 ledgers/min)
};
```

**Response:**
```json
{
  "loan_id": "0x1234...",
  "borrower": "GBXYZ...",
  "amount": 1000000000,
  "threshold": 3,
  "status": "PENDING_VOUCHES",
  "created_at": 50000000
}
```

**Success Criteria:**
- Request amount >= `min_loan_amount` (default: 100,000 stroops = 0.01 XLM)
- Request amount <= `max_loan_amount` (default: 1,000,000,000 stroops = 100 XLM)
- Borrower has no active/defaulted loans
- Threshold > 0

### 2. Monitor Vouch Collection

Once a loan is requested, monitor its status:

```typescript
const loan = await contract.query_loan({ loan_id: "0x1234..." });

console.log(`Loan status: ${loan.status}`);
console.log(`Vouches received: ${loan.vouch_count}/${loan.threshold}`);
console.log(`Total staked: ${loan.total_stake / 10_000_000} XLM`);

if (loan.vouch_count >= loan.threshold) {
  console.log("Loan is fully backed! Ready to withdraw.");
}
```

### 3. Withdraw Loan Funds

Once threshold is met, funds are immediately available:

```typescript
const loan = await contract.query_loan({ loan_id: "0x1234..." });

if (loan.vouch_count >= loan.threshold && loan.status === "ACTIVE") {
  // Withdraw to borrower's account
  const txBuilder = new TransactionBuilder(...)
    .addOperation(Operation.invokeContractFunction({
      contract: CONTRACT_ID,
      method: "withdraw_loan",
      args: [xdr.ScVal.scvSymbol("loan_id"), xdr.ScVal.scvBytes(Buffer.from(loanId, 'hex'))]
    }));
  
  const tx = txBuilder.build();
  // Sign and submit...
}
```

### 4. Repay the Loan

Borrowers must repay by the expiration ledger or face default slashing:

```typescript
// Pay 50 XLM of the 100 XLM loan (partial repayment allowed)
const repaymentAmount = 50n * 10_000_000n;

const repayTx = new TransactionBuilder(...)
  .addOperation(Operation.invokeContractFunction({
    contract: CONTRACT_ID,
    method: "repay",
    args: [
      xdr.ScVal.scvSymbol("loan_id"),
      xdr.ScVal.scvBytes(Buffer.from(loanId, 'hex')),
      xdr.ScVal.scvI128(xdr.Int64.fromString(repaymentAmount.toString()))
    ]
  }))
  .build();
```

**Partial Repayments:**
- Borrowers can repay in multiple transactions
- Yield is recalculated based on remaining principal
- Example: Borrow 100, repay 30, then repay 70 later (both allowed)

### 5. Query Loan Status

```typescript
const loan = await contract.query_loan({ loan_id: "0x1234..." });

console.log({
  id: loan.id,
  borrower: loan.borrower,
  amount: loan.amount / 10_000_000 + " XLM",
  amount_repaid: loan.amount_repaid / 10_000_000 + " XLM",
  total_yield: loan.total_yield / 10_000_000 + " XLM",
  status: loan.status,
  created_at: new Date(loan.created_at * 1000),
  expires_at: new Date((loan.created_at + loan.duration_ledgers * 5) * 1000),
  vouches: loan.vouch_count,
  threshold: loan.threshold
});
```

## Error Handling

### Contract Error Codes

| Error | Cause | Resolution |
|-------|-------|-----------|
| `LoanNotFound` | Loan ID doesn't exist | Verify loan ID |
| `BorrowedAlreadyHasActiveLoan` | Borrower has another active loan | Repay or wait for existing loan to close |
| `InvalidAmount` | Amount out of range | Request between min_loan_amount and max_loan_amount |
| `InsufficientVouches` | Not enough vouchers backed loan | Wait for more vouchers or retry |
| `LoanNotActive` | Operation only valid on active loans | Check loan status |
| `LoanAlreadyDefaulted` | Loan already in default | Verify with vouchers on slash process |
| `NotAuthorized` | Caller not allowed | Verify signer/auth context |
| `InvalidThreshold` | Threshold must be > 0 | Request with threshold >= 1 |
| `LoanExpired` | Loan past expiration | Repay immediately or contact vouchers |
| `RepaymentExceedsDebt` | Paying more than owed | Calculate exact remaining balance first |

### Handling Defaults

```typescript
// Check if loan is in default
const loan = await contract.query_loan({ loan_id: "0x1234..." });

if (loan.status === "DEFAULTED") {
  console.log("Loan defaulted! Vouchers will be slashed.");
  console.log(`Slashed amount per voucher: ${loan.slash_amount / 10_000_000} XLM`);
  
  // Notify user and stop operations on this loan
  displayErrorMessage("This loan defaulted on its maturity date. Your vouchers have been slashed.");
}
```

## Fee Calculation

### Yield Distribution

When a borrower successfully repays, vouchers earn yield:

```typescript
// Example: 100 XLM loan with 5 vouchers
const loanAmount = 100n * 10_000_000n;
const yieldRate = 2n;  // 2% (from protocol config)
const totalYield = (loanAmount * yieldRate) / 100n;  // 2 XLM total

// Distributed equally to 5 vouchers:
const yieldPerVoucher = totalYield / 5n;  // 0.4 XLM per voucher

// Voucher receives: stake + yield
const voucher1_return = 20n * 10_000_000n + yieldPerVoucher;
```

### Repayment Due = Loan Amount (No Interest From Borrower)

QuorumCredit is **interest-free for borrowers**:
- Borrower repays exactly the loan amount
- Yield is paid from the protocol's slash pool (borrowed from vouchers who get slashed)
- This makes microloans affordable for underserved communities

### Calculating Remaining Balance

```typescript
function getRemainingBalance(loan: LoanRecord): bigint {
  return loan.amount - loan.amount_repaid;
}

// Example:
const loan = await contract.query_loan({ loan_id: "0x1234..." });
const remaining = getRemainingBalance(loan);
console.log(`Remaining to repay: ${remaining / 10_000_000} XLM`);
```

## Common Use Cases

### Use Case 1: Apply for a Loan

```typescript
async function applyForLoan(borrower: string, amountXlm: number, voucherCount: number) {
  const amountStroops = BigInt(Math.round(amountXlm * 10_000_000));
  const durationLedgers = 1_296_000n;  // ~30 days
  
  const tx = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase: Networks.TESTNET_NETWORK_PASSPHRASE
  })
    .addOperation(Operation.invokeContractFunction({
      contract: CONTRACT_ID,
      method: "request_loan",
      args: [
        xdr.ScVal.scvI128(xdr.Int64.fromString(amountStroops.toString())),
        xdr.ScVal.scvI128(xdr.Int64.fromString(BigInt(voucherCount).toString())),
        xdr.ScVal.scvI128(xdr.Int64.fromString(durationLedgers.toString()))
      ]
    }))
    .setTimeout(30)
    .build();
  
  tx.sign(borrowerKeyPair);
  return submitTransaction(tx);
}
```

### Use Case 2: Check Loan Status and Display in UI

```typescript
async function displayLoanDashboard(loanId: string) {
  const loan = await contract.query_loan({ loan_id: loanId });
  const currentLedger = await server.ledgers().order('desc').limit(1).call();
  
  const expiresAt = loan.created_at + loan.duration_ledgers;
  const isExpired = currentLedger.sequence > expiresAt;
  
  return {
    status: loan.status,
    amountRequested: loan.amount / 10_000_000,
    amountRepaid: loan.amount_repaid / 10_000_000,
    amountRemaining: (loan.amount - loan.amount_repaid) / 10_000_000,
    vouchCount: `${loan.vouch_count}/${loan.threshold}`,
    isFullyBacked: loan.vouch_count >= loan.threshold,
    isExpired,
    daysUntilExpiry: Math.ceil((expiresAt - currentLedger.sequence) / 17_280),  // 17280 ledgers/day
    totalYield: loan.total_yield / 10_000_000
  };
}
```

### Use Case 3: Repay Loan with Progress Tracking

```typescript
async function repayLoan(loanId: string, borrowerKeyPair: Keypair) {
  const loan = await contract.query_loan({ loan_id: loanId });
  const remaining = loan.amount - loan.amount_repaid;
  
  console.log(`Loan Status:`);
  console.log(`  Original amount: ${loan.amount / 10_000_000} XLM`);
  console.log(`  Already repaid: ${loan.amount_repaid / 10_000_000} XLM`);
  console.log(`  Still owed: ${remaining / 10_000_000} XLM`);
  
  // User decides to pay 30 XLM
  const paymentAmount = 30n * 10_000_000n;
  
  if (paymentAmount > remaining) {
    throw new Error("Payment exceeds remaining balance");
  }
  
  const repayTx = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase: Networks.TESTNET_NETWORK_PASSPHRASE
  })
    .addOperation(Operation.invokeContractFunction({
      contract: CONTRACT_ID,
      method: "repay",
      args: [
        xdr.ScVal.scvBytes(Buffer.from(loanId, 'hex')),
        xdr.ScVal.scvI128(xdr.Int64.fromString(paymentAmount.toString()))
      ]
    }))
    .setTimeout(30)
    .build();
  
  repayTx.sign(borrowerKeyPair);
  
  const result = await submitTransaction(repayTx);
  console.log(`Payment of ${paymentAmount / 10_000_000} XLM submitted!`);
  console.log(`New remaining balance: ${(remaining - paymentAmount) / 10_000_000} XLM`);
  return result;
}
```

### Use Case 4: Handle Default Scenario

```typescript
async function handleDefaultedLoan(loanId: string) {
  const loan = await contract.query_loan({ loan_id: loanId });
  
  if (loan.status === "DEFAULTED") {
    console.warn("LOAN DEFAULTED!");
    
    return {
      message: "Your loan has been marked as defaulted",
      details: {
        originalAmount: loan.amount / 10_000_000,
        repaidAmount: loan.amount_repaid / 10_000_000,
        unpaidAmount: (loan.amount - loan.amount_repaid) / 10_000_000,
        slashAmount: loan.slash_amount / 10_000_000,
        vouches: loan.vouch_count
      },
      nextSteps: [
        "Contact your vouchers to resolve the default",
        "Payment may still be accepted after default",
        "Vouchers can vote to forgive the default"
      ]
    };
  }
}
```

## Code Examples

### TypeScript/JavaScript Example (Complete)

```typescript
import { 
  Keypair, 
  TransactionBuilder, 
  Operation, 
  Networks,
  xdr,
  SorobanRpc
} from '@stellar/js-stellar-sdk';

const CONTRACT_ID = 'CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4';
const BASE_FEE = '100';
const server = new SorobanRpc.Server('https://soroban-testnet.stellar.org');

async function quickstartBorrower() {
  const borrower = Keypair.random();
  
  // 1. Request a 50 XLM loan
  console.log('1. Requesting 50 XLM loan...');
  const account = await server.getAccount(borrower.publicKey());
  
  const requestTx = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase: Networks.TESTNET_NETWORK_PASSPHRASE
  })
    .addOperation(Operation.invokeContractFunction({
      contract: CONTRACT_ID,
      method: 'request_loan',
      args: [
        xdr.ScVal.scvI128(xdr.Int64.fromString((50n * 10_000_000n).toString())),
        xdr.ScVal.scvI128(xdr.Int64.fromString('3')),  // 3 vouchers
        xdr.ScVal.scvI128(xdr.Int64.fromString('1296000'))  // 30 days
      ]
    }))
    .setTimeout(30)
    .build();
  
  requestTx.sign(borrower);
  const result = await server.submitTransaction(requestTx);
  console.log('Loan requested successfully!');
  console.log('Transaction ID:', result.id);
  
  // 2. Wait for vouches and check status
  console.log('\n2. Waiting for vouches...');
  const loanId = result.events[0].contract_id;  // Extract from event
  
  for (let i = 0; i < 12; i++) {
    const loan = await server.request('soroban_simulateTransaction', {
      transaction: requestTx.toXDR(),
      resourceLeeway: 15
    });
    
    if (loan.vouch_count >= 3) {
      console.log('Loan fully backed!');
      break;
    }
    console.log(`Vouches: ${loan.vouch_count}/3`);
    await new Promise(r => setTimeout(r, 5000));  // Wait 5 seconds
  }
  
  // 3. Repay the loan
  console.log('\n3. Repaying loan...');
  const repayTx = new TransactionBuilder(account, {
    fee: BASE_FEE,
    networkPassphrase: Networks.TESTNET_NETWORK_PASSPHRASE
  })
    .addOperation(Operation.invokeContractFunction({
      contract: CONTRACT_ID,
      method: 'repay',
      args: [
        xdr.ScVal.scvBytes(Buffer.from(loanId, 'hex')),
        xdr.ScVal.scvI128(xdr.Int64.fromString((50n * 10_000_000n).toString()))
      ]
    }))
    .setTimeout(30)
    .build();
  
  repayTx.sign(borrower);
  await server.submitTransaction(repayTx);
  console.log('Loan repaid successfully!');
}

quickstartBorrower().catch(console.error);
```

### Python Example

```python
from stellar_sdk import Keypair, TransactionBuilder, Network, Operation
from stellar_sdk.soroban import SorobanServer
import time

CONTRACT_ID = 'CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4'
BASE_FEE = '100'
server = SorobanServer('https://soroban-testnet.stellar.org')

def borrow_loan():
    borrower = Keypair.random()
    print(f"Borrower: {borrower.public_key}")
    
    # 1. Request loan
    print("Requesting 50 XLM loan...")
    account = server.load_account(borrower.public_key)
    
    transaction = (
        TransactionBuilder(account, BASE_FEE, Network.TESTNET_NETWORK_PASSPHRASE)
        .add_text_memo("QuorumCredit Loan Request")
        .append_invoke_soroban_contract_op(
            contract_id=CONTRACT_ID,
            method='request_loan',
            parameters=[
                50 * 10_000_000,  # 50 XLM in stroops
                3,                # 3 vouchers needed
                1_296_000         # 30 days
            ]
        )
        .set_timeout(30)
        .build()
    )
    
    transaction.sign(borrower)
    response = server.submit_transaction(transaction)
    print(f"Loan request submitted: {response['id']}")
    
    # 2. Monitor vouch collection
    time.sleep(10)  # Give vouchers time to respond
    loan_status = server.rpc.request('soroban_rpc', {
        'method': 'getLoanStatus',
        'params': [response['id']]
    })
    print(f"Vouches received: {loan_status['vouch_count']}/3")
    
    # 3. Repay the loan
    print("Repaying 50 XLM...")
    repay_tx = (
        TransactionBuilder(account, BASE_FEE, Network.TESTNET_NETWORK_PASSPHRASE)
        .append_invoke_soroban_contract_op(
            contract_id=CONTRACT_ID,
            method='repay',
            parameters=[
                response['loan_id'],
                50 * 10_000_000
            ]
        )
        .set_timeout(30)
        .build()
    )
    
    repay_tx.sign(borrower)
    server.submit_transaction(repay_tx)
    print("Loan repaid successfully!")

if __name__ == '__main__':
    borrow_loan()
```

## Testing Your Integration

Use these queries to validate your implementation:

```bash
# Check loan status
curl -X POST https://soroban-testnet.stellar.org/ \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "soroban_call",
    "params": [
      {
        "contract": "'$CONTRACT_ID'",
        "method": "query_loan",
        "args": ["'$LOAN_ID'"]
      }
    ],
    "id": 1
  }'

# List borrower loans
curl -X POST https://soroban-testnet.stellar.org/ \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "soroban_call",
    "params": [
      {
        "contract": "'$CONTRACT_ID'",
        "method": "query_borrower_loans",
        "args": ["'$BORROWER_ADDRESS'"]
      }
    ],
    "id": 1
  }'
```

## Troubleshooting

**Q: Transaction rejected with "InvalidAmount"**
- A: Ensure amount is between min_loan_amount (100,000 stroops) and max_loan_amount

**Q: "BorrowerAlreadyHasActiveLoan" error**
- A: You already have an active or recently defaulted loan. Repay it first.

**Q: How long do vouchers have to respond?**
- A: Loan requests remain open until the expiration ledger or voucher threshold is met. Default is 30 days.

**Q: Can I repay early?**
- A: Yes! Partial and early repayments are fully supported.

**Q: What happens if I don't repay by the deadline?**
- A: Your vouchers will be slashed at the expiration ledger. This is non-recoverable without community governance vote.

## References

- [Soroban JS SDK](https://github.com/stellar/js-stellar-sdk)
- [Soroban Python SDK](https://github.com/stellar/py-stellar-sdk)
- [QuorumCredit Contract ABI](../openapi.yaml)
- [Security Best Practices](./security-best-practices.md)
- [API Error Codes](./api-error-codes.md)
