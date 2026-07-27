# QuorumCredit Event Indexer Service

Event indexing service for QuorumCredit smart contract (#1084).

## Overview

This service indexes on-chain events emitted by the QuorumCredit contract and provides a REST API for querying events by type, timestamp, and participant.

## Features

- Real-time event indexing from Soroban RPC
- Indexes events by type, timestamp, and participant
- REST API for querying indexed events
- Event statistics and analytics
- Persistent storage (JSON file)
- Health monitoring

## Quick Start

### Prerequisites

- Node.js 18+
- npm or yarn

### Installation

```bash
cd services/indexer
npm install
```

### Configuration

1. Copy `.env.example` to `.env`:
   ```bash
   cp .env.example .env
   ```

2. Edit `.env` with your configuration:
   ```env
   RPC_URL=https://soroban-testnet.stellar.org:443
   CONTRACT_ID=YOUR_CONTRACT_ID_HERE
   NETWORK_PASSPHRASE=Test SDF Network ; September 2015
   PORT=3000
   ```

### Running the Service

```bash
# Development mode
npm run dev

# Production mode
npm start
```

## API Endpoints

### Health Check
```
GET /health
```

Returns service health status.

### Query Events
```
GET /events
```

Query parameters:
- `type`: Filter by event type (e.g., `vouch/create`, `loan/repay`)
- `start_date`: Start timestamp in milliseconds
- `end_date`: End timestamp in milliseconds
- `participant`: Filter by participant address
- `contract_id`: Filter by contract ID
- `limit`: Maximum number of results (default: 100)
- `offset`: Pagination offset (default: 0)

Example:
```
GET /events?type=loan/slash&start_date=1672531200000&end_date=1672617600000&limit=10
```

### Statistics
```
GET /stats
```

Returns event statistics including:
- Total events indexed
- Events by type
- Events by day (last 30 days)
- Unique participants

## Event Types

The indexer recognizes the following QuorumCredit event types:

- `vouch/create`: New vouch created
- `vouch/increase`: Vouch stake increased
- `vouch/decrease`: Vouch stake decreased
- `vouch/withdraw`: Vouch withdrawn
- `loan/request`: Loan requested
- `loan/repay`: Loan repaid
- `loan/slash`: Loan slashed
- `admin/config`: Configuration updated
- `admin/pause`: Contract paused
- `admin/unpause`: Contract unpaused

## Database

Events are stored in a JSON file (`events.db.json`) by default. The file is automatically created and updated as new events are indexed.

## Monitoring

The service logs indexing progress and errors to the console. Health checks are available via the `/health` endpoint.

## Architecture

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   Soroban RPC   │───▶│   Event Indexer  │───▶│     JSON DB      │
└─────────────────┘    └─────────────────┘    └─────────────────┘
                              │
                              ▼
                       ┌─────────────────┐
                       │   REST API      │
                       └─────────────────┘
```

## Development

### Building
```bash
npm run build
```

### Testing
```bash
# No tests implemented yet
```

## Deployment

### Docker
```bash
docker build -t quorumcredit-indexer .
docker run -p 3000:3000 quorumcredit-indexer
```

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `RPC_URL` | Soroban RPC URL | `https://soroban-testnet.stellar.org:443` |
| `CONTRACT_ID` | QuorumCredit contract ID | Required |
| `NETWORK_PASSPHRASE` | Network passphrase | `Test SDF Network ; September 2015` |
| `PORT` | API server port | `3000` |
| `START_LEDGER` | Starting ledger number | Latest - 1000 |
| `DB_PATH` | Database file path | `./events.db.json` |

## Troubleshooting

### Common Issues

1. **No events being indexed**
   - Verify contract ID is correct
   - Check RPC URL connectivity
   - Ensure contract is deployed and emitting events

2. **API not responding**
   - Check if service is running
   - Verify port is not in use
   - Check logs for errors

3. **Database not persisting**
   - Check file permissions
   - Verify disk space
   - Check for JSON parsing errors

### Logs

Check console output for:
- Indexing progress
- RPC connection errors
- Event parsing errors
- Database save errors

## License

MIT