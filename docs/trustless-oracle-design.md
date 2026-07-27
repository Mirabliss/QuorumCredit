# Design: Trustless Oracle Integration for Price Feeds

## Overview

QuorumCredit currently supports single-asset (USDC) loans. Multi-asset expansion requires reliable, decentralized price feeds. This design proposes leveraging Stellar's native price feeds while maintaining trustlessness and defining fallback mechanisms.

## Problem Statement

- Current oracle approach (if centralized) introduces counterparty risk: a single oracle operator can manipulate prices.
- Multi-asset loans require pricing for volatile assets (e.g., native tokens, wrapped stablecoins).
- Oracle unavailability must not halt loan operations or liquidations.

## Proposed Solution: Stellar Native Price Feeds

### Architecture

1. **Price Ledger Entries**: Stellar's native `PRICE_POINT` ledger entries allow validators to publish price data.
   - Prices are anchored to the network consensus; no single entity can falsify without 51% attack.
   - Each price point includes: asset pair, price (rational number), timestamp, confidence bound.

2. **Multi-Source Aggregation**: Contracts query multiple price feeds and aggregate:
   - Time-weighted average price (TWAP) to smooth volatility.
   - Median price across publishers to resist outliers.
   - Discard prices older than configurable TTL (e.g., 5 minutes).

3. **On-Chain Verification**:
   - Prices are published by Stellar validators; verification is cryptographic.
   - Smart contracts read prices directly from ledger state; no external API calls.
   - Timestamp and confidence checks prevent stale or unreliable data.

### Asset Support

- **Stablecoins** (e.g., USDC, EUROC): Rely on 1:1 peg; low confidence requirements.
- **Native Assets**: Published via Stellar Anchors (e.g., Lobstr, StellarChain) or in-house validators.
- **Cross-Asset Pairs**: If XYZ/USD unavailable, compute via XYZ/USDC + USDC/USD.

## Fallback Mechanism

When a price feed is unavailable or stale:

### Tier 1: Secondary Feed
- Query alternative validators or anchors for the same asset pair.
- If available and recent, use with reduced confidence (e.g., penalize interest rate or increase collateral requirement).

### Tier 2: Historical Reference
- Use the most recent valid price (within acceptable staleness, e.g., 1 hour).
- Apply a conservative adjustment (e.g., 5% haircut) to account for price drift.
- Log event and alert operators.

### Tier 3: Halt Operations
- If no valid price exists, pause new loan originations for that asset.
- Allow continued collection of yield on existing loans.
- Liquidations may proceed using the most recent price with a large haircut (e.g., 10%).

### Configuration

```
PRICE_FEED_CONFIG = {
  "USDC": {
    "primary_sources": ["stellar.validator1", "stellar.validator2"],
    "max_staleness_sec": 300,  // 5 minutes
    "aggregation": "median",
    "fallback_tier2_staleness_sec": 3600,  // 1 hour
    "fallback_tier2_haircut_bps": 500,  // 5%
    "fallback_tier3_haircut_bps": 1000,  // 10%
  },
  "NATIVE_XYZ": {
    "primary_sources": ["stellar.validator3"],
    "cross_pair_fallback": ["XYZ/USDC", "USDC/USD"],
    ...
  }
}
```

## Implementation Considerations

1. **Price Caching**: Cache prices on-chain for 30–60 seconds to reduce redundant queries.
2. **Confidence Scoring**: Assign a confidence score (0–100) to each price based on:
   - Age (newer is better).
   - Source reputation (established validators score higher).
   - Agreement across sources (consensus raises confidence).
3. **Liquidation Safety**: Liquidations use a conservative price (e.g., 5–10% haircut) to avoid flash-loan attacks or oracle manipulation.
4. **Governance**: Admin multisig can adjust fallback thresholds and asset support without contract upgrade.

## Security Assumptions

- Stellar consensus remains secure; 51% attack is economically infeasible.
- Price publishers are incentivized to publish accurate data (reputation, fees).
- Smart contracts correctly parse and validate ledger entries.

## Future Extensions

- Integration with Chainlink or Band Protocol for cross-chain assets.
- Machine learning models to detect price anomalies and trigger alerts.
- Insurance pool to cover oracle-related losses.
