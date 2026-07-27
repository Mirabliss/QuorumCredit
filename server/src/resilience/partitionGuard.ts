import { metrics } from "../http/metricsRegistry.js";

export type QueuedWrite = () => Promise<void> | void;

export interface PartitionGuardConfig {
  /** Consecutive failed bridge ticks (bus/store errors — see Bridge.tick) before this
   * instance declares itself partitioned from the validator/indexer network and enters
   * read-only mode. */
  failureThreshold: number;
  /** Bounded in-memory queue capacity for writes deferred during a partition. Beyond this,
   * the OLDEST queued write is dropped to make room (mirrors ws/connectionQueue.ts's
   * drop-oldest policy) — a documented tradeoff, not a guarantee that every write survives
   * an extended partition. See docs/network-partition-guide.md. */
  maxQueuedWrites: number;
}

export interface PartitionStatus {
  partitioned: boolean;
  partitionedSince: number | undefined;
  consecutiveFailures: number;
  queueDepth: number;
  droppedWrites: number;
}

/**
 * Detects a network partition between this server instance and the
 * validator/indexer network (via consecutive `Bridge` tick failures — see
 * `Bridge.tick`'s try/catch), flips the instance into read-only mode, and
 * buffers mutating HTTP requests for replay once connectivity recovers
 * (issue #1229).
 *
 * Guarantees documented explicitly rather than implied:
 * - Reads (GET) are never blocked — this instance keeps serving whatever
 *   state it last had from the indexer DB regardless of partition status.
 * - Writes accepted while partitioned are queued in-memory only; they do
 *   NOT survive a process restart, and the queue is bounded — a partition
 *   that outlasts `maxQueuedWrites` accepted writes drops the oldest ones
 *   (counted in `droppedWrites` and exposed via the `qc_partition_writes_dropped_total`
 *   metric so an operator can see it happened).
 * - Recovery is automatic: the next successful bridge tick clears
 *   partitioned state and replays every still-queued write, in the order
 *   they were accepted.
 */
export class PartitionGuard {
  private readonly config: PartitionGuardConfig;
  private readonly queue: QueuedWrite[] = [];
  private consecutiveFailures = 0;
  private partitioned = false;
  private partitionedSince: number | undefined;
  private droppedWrites = 0;

  constructor(config: PartitionGuardConfig) {
    this.config = config;
  }

  /** Call after a bridge tick that successfully reached the bus/store. */
  recordSuccess(): void {
    this.consecutiveFailures = 0;
    if (this.partitioned) {
      this.partitioned = false;
      this.partitionedSince = undefined;
      metrics.incCounter("qc_partition_recovered_total");
      void this.flush();
    }
  }

  /** Call after a bridge tick that failed to reach the bus/store. */
  recordFailure(): void {
    this.consecutiveFailures += 1;
    if (!this.partitioned && this.consecutiveFailures >= this.config.failureThreshold) {
      this.partitioned = true;
      this.partitionedSince = Date.now();
      metrics.incCounter("qc_partition_detected_total");
    }
  }

  isPartitioned(): boolean {
    return this.partitioned;
  }

  status(): PartitionStatus {
    return {
      partitioned: this.partitioned,
      partitionedSince: this.partitionedSince,
      consecutiveFailures: this.consecutiveFailures,
      queueDepth: this.queue.length,
      droppedWrites: this.droppedWrites,
    };
  }

  /** Queues a write for replay once the partition clears. Only meaningful to call while
   * `isPartitioned()` is true — callers should execute writes directly otherwise. */
  enqueue(write: QueuedWrite): void {
    if (this.queue.length >= this.config.maxQueuedWrites) {
      this.queue.shift();
      this.droppedWrites += 1;
      metrics.incCounter("qc_partition_writes_dropped_total");
    }
    this.queue.push(write);
    metrics.setGauge("qc_partition_queue_depth", this.queue.length);
  }

  private async flush(): Promise<void> {
    const pending = this.queue.splice(0, this.queue.length);
    metrics.setGauge("qc_partition_queue_depth", 0);
    for (const write of pending) {
      try {
        await write();
        metrics.incCounter("qc_partition_writes_replayed_total");
      } catch {
        // Best-effort replay: a write whose replay itself fails is dropped rather than
        // requeued, to avoid an unbounded retry loop across future partitions.
        metrics.incCounter("qc_partition_writes_replay_failed_total");
      }
    }
  }
}
