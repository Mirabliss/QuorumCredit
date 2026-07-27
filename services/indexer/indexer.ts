#!/usr/bin/env node

/**
 * QuorumCredit Event Indexer Service
 * 
 * #1084: Implement On-Chain Event Indexing
 * 
 * This service indexes QuorumCredit contract events by type, timestamp, and participant,
 * and exposes indexed queries via API for efficient event querying.
 */

import express from 'express';
import { SorobanRpc, TransactionBuilder, scValToNative, xdr } from '@stellar/stellar-sdk';
import { config } from 'dotenv';
import { EventEmitter } from 'events';
import fs from 'fs';
import path from 'path';

// Load environment variables
config();

interface EventData {
  id: string;
  type: string;
  timestamp: number;
  participant: string;
  contractId: string;
  data: any;
  blockNumber: number;
  transactionHash: string;
}

interface IndexQuery {
  type?: string;
  startDate?: number;
  endDate?: number;
  participant?: string;
  contractId?: string;
  limit?: number;
  offset?: number;
}

class EventIndexer extends EventEmitter {
  private rpcUrl: string;
  private contractId: string;
  private networkPassphrase: string;
  private database: EventData[] = [];
  private dbPath: string;
  private isIndexing = false;

  constructor() {
    super();
    
    this.rpcUrl = process.env.RPC_URL || 'https://soroban-testnet.stellar.org:443';
    this.contractId = process.env.CONTRACT_ID || '';
    this.networkPassphrase = process.env.NETWORK_PASSPHRASE || 'Test SDF Network ; September 2015';
    this.dbPath = path.join(__dirname, 'events.db.json');
    
    this.loadDatabase();
  }

  /**
   * Load events database from file
   */
  private loadDatabase(): void {
    try {
      if (fs.existsSync(this.dbPath)) {
        const data = fs.readFileSync(this.dbPath, 'utf8');
        this.database = JSON.parse(data);
        console.log(`Loaded ${this.database.length} events from database`);
      }
    } catch (error) {
      console.error('Error loading database:', error);
      this.database = [];
    }
  }

  /**
   * Save events database to file
   */
  private saveDatabase(): void {
    try {
      fs.writeFileSync(this.dbPath, JSON.stringify(this.database, null, 2));
    } catch (error) {
      console.error('Error saving database:', error);
    }
  }

  /**
   * Start indexing events from the latest block
   */
  async startIndexing(fromLedger?: number): Promise<void> {
    if (this.isIndexing) {
      console.log('Indexer is already running');
      return;
    }

    this.isIndexing = true;
    console.log('Starting event indexer...');

    try {
      const server = new SorobanRpc.Server(this.rpcUrl);
      
      // Get latest ledger
      const latestLedger = await server.getLatestLedger();
      let startLedger = fromLedger || Math.max(1, latestLedger.sequence - 1000); // Last 1000 ledgers

      console.log(`Starting from ledger ${startLedger}, latest is ${latestLedger.sequence}`);

      // Main indexing loop
      while (this.isIndexing) {
        try {
          // Get events for the current ledger
          const events = await server.getEvents({
            startLedger,
            filters: [
              {
                contractIds: [this.contractId],
              },
            ],
            limit: 1000,
          });

          if (events.events && events.events.length > 0) {
            const newEvents = this.processEvents(events.events);
            
            // Add to database
            this.database.push(...newEvents);
            this.saveDatabase();
            
            console.log(`Indexed ${newEvents.length} new events from ledger ${startLedger}`);
            
            // Emit event for real-time processing
            this.emit('newEvents', newEvents);
          }

          // Move to next ledger
          startLedger++;
          
          // Wait before next poll (5 seconds)
          await new Promise(resolve => setTimeout(resolve, 5000));
          
        } catch (error) {
          console.error('Error indexing ledger', startLedger, error);
          // Wait longer on error
          await new Promise(resolve => setTimeout(resolve, 10000));
        }
      }
    } catch (error) {
      console.error('Failed to start indexer:', error);
      this.isIndexing = false;
    }
  }

  /**
   * Process raw events from Soroban RPC
   */
  private processEvents(rawEvents: any[]): EventData[] {
    return rawEvents.map(event => {
      try {
        const parsedEvent = this.parseEvent(event);
        return {
          id: `${event.ledger}-${event.id}`,
          type: parsedEvent.type,
          timestamp: new Date(event.ledgerClosedAt).getTime(),
          participant: parsedEvent.participant,
          contractId: this.contractId,
          data: parsedEvent.data,
          blockNumber: parseInt(event.ledger),
          transactionHash: event.txHash,
        };
      } catch (error) {
        console.error('Error parsing event:', error);
        return null;
      }
    }).filter(event => event !== null) as EventData[];
  }

  /**
   * Parse event based on QuorumCredit event structure
   */
  private parseEvent(event: any): { type: string; participant: string; data: any } {
    const topics = event.topics.map((topic: any) => scValToNative(topic));
    
    // QuorumCredit events follow pattern: [event_type, participant_address, ...data]
    let type = 'unknown';
    let participant = '';
    let data = {};
    
    if (topics.length >= 2) {
      type = topics[0] as string;
      participant = topics[1] as string;
      
      // Parse additional data based on event type
      switch (type) {
        case 'vouch/create':
          data = {
            voucher: participant,
            borrower: topics[2],
            stake: topics[3],
            token: topics[4],
          };
          break;
          
        case 'loan/request':
          data = {
            borrower: participant,
            amount: topics[2],
            threshold: topics[3],
            loanPurpose: topics[4],
            token: topics[5],
          };
          break;
          
        case 'loan/repay':
          data = {
            borrower: participant,
            payment: topics[2],
          };
          break;
          
        case 'loan/slash':
          data = {
            borrower: participant,
            slashedAmount: topics[2],
          };
          break;
          
        default:
          data = topics.slice(2);
      }
    }
    
    return { type, participant, data };
  }

  /**
   * Stop indexing
   */
  stopIndexing(): void {
    this.isIndexing = false;
    console.log('Event indexer stopped');
  }

  /**
   * Query indexed events
   */
  queryEvents(query: IndexQuery): EventData[] {
    let results = this.database;

    // Apply filters
    if (query.type) {
      results = results.filter(event => event.type === query.type);
    }
    
    if (query.startDate) {
      results = results.filter(event => event.timestamp >= query.startDate);
    }
    
    if (query.endDate) {
      results = results.filter(event => event.timestamp <= query.endDate);
    }
    
    if (query.participant) {
      results = results.filter(event => event.participant === query.participant);
    }
    
    if (query.contractId) {
      results = results.filter(event => event.contractId === query.contractId);
    }

    // Apply pagination
    const offset = query.offset || 0;
    const limit = query.limit || 100;
    
    results.sort((a, b) => b.timestamp - a.timestamp); // Newest first
    
    return results.slice(offset, offset + limit);
  }

  /**
   * Get event statistics
   */
  getStatistics(): any {
    const totalEvents = this.database.length;
    const eventsByType = this.database.reduce((acc, event) => {
      acc[event.type] = (acc[event.type] || 0) + 1;
      return acc;
    }, {} as Record<string, number>);

    const eventsByDay = this.database.reduce((acc, event) => {
      const date = new Date(event.timestamp).toISOString().split('T')[0];
      acc[date] = (acc[date] || 0) + 1;
      return acc;
    }, {} as Record<string, number>);

    const uniqueParticipants = new Set(this.database.map(event => event.participant)).size;

    return {
      totalEvents,
      eventsByType,
      eventsByDay: Object.entries(eventsByDay)
        .sort(([a], [b]) => b.localeCompare(a))
        .slice(0, 30), // Last 30 days
      uniqueParticipants,
    };
  }
}

// Create Express API server
function createAPIServer(indexer: EventIndexer): express.Application {
  const app = express();
  app.use(express.json());

  // Health check endpoint
  app.get('/health', (req, res) => {
    res.json({
      status: 'healthy',
      databaseSize: indexer['database'].length,
      isIndexing: indexer['isIndexing'],
    });
  });

  // Query events endpoint
  app.get('/events', (req, res) => {
    try {
      const query: IndexQuery = {
        type: req.query.type as string,
        startDate: req.query.start_date ? parseInt(req.query.start_date as string) : undefined,
        endDate: req.query.end_date ? parseInt(req.query.end_date as string) : undefined,
        participant: req.query.participant as string,
        contractId: req.query.contract_id as string,
        limit: req.query.limit ? parseInt(req.query.limit as string) : 100,
        offset: req.query.offset ? parseInt(req.query.offset as string) : 0,
      };

      const events = indexer.queryEvents(query);
      res.json({
        count: events.length,
        events,
        query,
      });
    } catch (error) {
      console.error('Error querying events:', error);
      res.status(500).json({ error: 'Failed to query events' });
    }
  });

  // Statistics endpoint
  app.get('/stats', (req, res) => {
    try {
      const stats = indexer.getStatistics();
      res.json(stats);
    } catch (error) {
      console.error('Error getting statistics:', error);
      res.status(500).json({ error: 'Failed to get statistics' });
    }
  });

  // Example: GET /events?type=loan/slash&start_date=1672531200000&end_date=1672617600000&participant=GABC...
  // Example: GET /events?type=vouch/create&limit=10&offset=0

  return app;
}

// Main function
async function main() {
  console.log('QuorumCredit Event Indexer Service');
  console.log('===================================');

  // Create indexer
  const indexer = new EventIndexer();

  // Create API server
  const app = createAPIServer(indexer);
  const PORT = process.env.PORT || 3000;

  // Start API server
  app.listen(PORT, () => {
    console.log(`API server running on port ${PORT}`);
    console.log(`Health check: http://localhost:${PORT}/health`);
    console.log(`Events query: http://localhost:${PORT}/events`);
    console.log(`Statistics: http://localhost:${PORT}/stats`);
  });

  // Start indexing
  await indexer.startIndexing();

  // Graceful shutdown
  process.on('SIGINT', () => {
    console.log('Shutting down...');
    indexer.stopIndexing();
    process.exit(0);
  });

  process.on('SIGTERM', () => {
    console.log('Shutting down...');
    indexer.stopIndexing();
    process.exit(0);
  });
}

// Run if this file is executed directly
if (require.main === module) {
  main().catch(console.error);
}

export { EventIndexer, createAPIServer };
export type { EventData, IndexQuery };