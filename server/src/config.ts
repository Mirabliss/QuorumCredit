export interface ServerConfig {
  port: number;
  redisUrl: string | undefined;
  indexerDbPath: string;
  authSecret: string;
  tokenTtlSeconds: number;
  /** Bounded per-connection outgoing queue capacity before drop-oldest kicks in. */
  connectionQueueMax: number;
  /** How often the bridge polls the indexer DB for newly-inserted rows. */
  bridgePollIntervalMs: number;
  /** How long a bridge leader lock is held before it must be renewed. */
  leaderLockTtlMs: number;
  instanceId: string;
  /** Deployed version identifier, surfaced on /health for canary release monitoring
   * (issue #1231) — e.g. a git SHA or semver tag set by the deploy pipeline. */
  serviceVersion: string;
  /** Cost allocation inputs (issue #1227) — see server/src/costs/costAllocator.ts. */
  costAllocation: {
    contractFeeStroopsPerTx: number;
    apiServerMonthlyCostCents: number;
    storageMonthlyCostCents: number;
    stroopsToCentsRate: number | undefined;
  };
  /** Network partition detection/recovery (issue #1229) — see server/src/resilience/partitionGuard.ts. */
  partitionGuard: {
    failureThreshold: number;
    maxQueuedWrites: number;
  };
}

function envInt(name: string, fallback: number): number {
  const raw = process.env[name];
  if (!raw) return fallback;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function envFloat(name: string): number | undefined {
  const raw = process.env[name];
  if (!raw) return undefined;
  const parsed = Number.parseFloat(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): ServerConfig {
  return {
    port: envInt("PORT", 4000),
    redisUrl: env.REDIS_URL,
    indexerDbPath: env.INDEXER_DB_PATH ?? "indexer.db",
    authSecret: env.AUTH_SECRET ?? "dev-insecure-secret-change-me",
    tokenTtlSeconds: envInt("TOKEN_TTL_SECONDS", 300),
    connectionQueueMax: envInt("CONN_QUEUE_MAX", 500),
    bridgePollIntervalMs: envInt("BRIDGE_POLL_INTERVAL_MS", 250),
    leaderLockTtlMs: envInt("LEADER_LOCK_TTL_MS", 5000),
    instanceId: env.INSTANCE_ID ?? `inst-${process.pid}-${Math.random().toString(36).slice(2, 8)}`,
    serviceVersion: env.SERVICE_VERSION ?? "dev",
    costAllocation: {
      contractFeeStroopsPerTx: envInt("CONTRACT_FEE_STROOPS_PER_TX", 100_000),
      apiServerMonthlyCostCents: envInt("API_SERVER_MONTHLY_COST_CENTS", 0),
      storageMonthlyCostCents: envInt("STORAGE_MONTHLY_COST_CENTS", 0),
      stroopsToCentsRate: envFloat("STROOPS_TO_CENTS_RATE"),
    },
    partitionGuard: {
      failureThreshold: envInt("PARTITION_FAILURE_THRESHOLD", 5),
      maxQueuedWrites: envInt("PARTITION_MAX_QUEUED_WRITES", 500),
    },
  };
}
