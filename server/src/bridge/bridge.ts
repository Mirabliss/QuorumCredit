import type { PubSubBus } from "../pubsub/PubSubBus.js";
import type { EventStore } from "./eventStore.js";
import { MetricsAggregator } from "./metricsAggregator.js";
import { CostAllocator, type CostAllocatorConfig } from "../costs/costAllocator.js";
import { PartitionGuard, type PartitionGuardConfig } from "../resilience/partitionGuard.js";
import { EVENTS_CHANNEL, type BroadcastEvent } from "../types.js";

const LEADER_LOCK_KEY = "qc:bridge:leader";

export interface BridgeOptions {
  bus: PubSubBus;
  store: EventStore;
  instanceId: string;
  pollIntervalMs: number;
  leaderLockTtlMs: number;
  costAllocation: CostAllocatorConfig;
  partitionGuard: PartitionGuardConfig;
  onPublish?: (event: BroadcastEvent) => void;
}

/**
 * Tails the indexer's `events` table and republishes new rows onto the pub/sub bus so
 * every server instance's connected clients see them, regardless of which instance
 * they're attached to. Only one instance actually runs the tail loop at a time — the
 * others hold off via a bus-mediated lease so events aren't published N times for N
 * instances.
 *
 * Cursor note: the bridge does NOT persist its publish cursor across restarts. A newly
 * elected leader replays from event id 0 and republishes everything; this is safe
 * because (a) client hooks track their own lastEventId and ignore anything they've
 * already applied, and (b) a client's *initial* sync always comes from a direct
 * EventStore.getEventsSince(since) replay in the WS/socket.io layer, not from bus
 * traffic. The cost is a burst of already-seen messages on leader handover, which is
 * cheap at this protocol's event volume — documented here rather than adding a second
 * persistence mechanism for a cursor whose loss has no correctness impact.
 *
 * Cost allocation note: `costAllocator` (issue #1227) is fed from this same leader-only
 * event loop, so like `aggregator` its counts only accrue on whichever instance
 * currently holds the leader lock — read `/costs/report` from the leader, or treat
 * per-instance drift as expected in a multi-replica deployment without a shared store.
 *
 * Partition note: `partitionGuard` (issue #1229) watches this same tick loop's
 * success/failure outcome. `failureThreshold` consecutive bus/store errors here flip
 * this instance into read-only mode; the next successful tick clears it and replays
 * any writes queued in the meantime. See docs/network-partition-guide.md.
 */
export class Bridge {
  private readonly opts: BridgeOptions;
  private readonly aggregator = new MetricsAggregator();
  readonly costAllocator: CostAllocator;
  readonly partitionGuard: PartitionGuard;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private stopped = false;
  private isLeader = false;
  private lastPublishedId = 0;

  constructor(opts: BridgeOptions) {
    this.opts = opts;
    this.costAllocator = new CostAllocator(opts.costAllocation);
    this.partitionGuard = new PartitionGuard(opts.partitionGuard);
  }

  start(): void {
    this.stopped = false;
    void this.tick();
  }

  async stop(): Promise<void> {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    if (this.isLeader) {
      await this.opts.bus.releaseLock(LEADER_LOCK_KEY, this.opts.instanceId);
      this.isLeader = false;
    }
  }

  private async tick(): Promise<void> {
    if (this.stopped) return;

    try {
      this.isLeader = await this.opts.bus.tryAcquireLock(
        LEADER_LOCK_KEY,
        this.opts.leaderLockTtlMs,
        this.opts.instanceId
      );

      if (this.isLeader) {
        const rows = this.opts.store.getEventsSince(this.lastPublishedId);
        for (const event of rows) {
          const metrics = this.aggregator.applyEvent(event);
          this.costAllocator.recordEvent(event);
          const broadcast: BroadcastEvent = { eventId: event.id, event, metrics };
          await this.opts.bus.publish(EVENTS_CHANNEL, JSON.stringify(broadcast));
          this.opts.onPublish?.(broadcast);
          this.lastPublishedId = event.id;
        }
      }
      this.partitionGuard.recordSuccess();
    } catch {
      // Transient bus/store error — next tick retries; leadership lease expiring
      // naturally hands off to another instance if this one is unhealthy.
      this.partitionGuard.recordFailure();
    }

    if (!this.stopped) {
      this.timer = setTimeout(() => void this.tick(), this.opts.pollIntervalMs);
    }
  }
}
