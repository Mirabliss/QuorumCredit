import { createServer } from "node:http";
import { loadConfig } from "./config.js";
import { LocalBus } from "./pubsub/LocalBus.js";
import { RedisBus } from "./pubsub/RedisBus.js";
import type { PubSubBus } from "./pubsub/PubSubBus.js";
import { EventStore } from "./bridge/eventStore.js";
import { Bridge } from "./bridge/bridge.js";
import { attachLoanSocketServer } from "./ws/loanSocketServer.js";
import { attachMetricsWsServer } from "./ws/metricsWsServer.js";
import { handleHttpRequest } from "./http/routes.js";
import * as insuranceMarketplace from "./insurance-marketplace.js";

export function buildBus(redisUrl: string | undefined): PubSubBus {
  if (redisUrl) return new RedisBus(redisUrl);
  console.warn(
    "[quorum-credit-broadcast-server] REDIS_URL not set — using an in-process pub/sub bus. " +
      "This is NOT multi-instance-safe and must not be used with more than one replica in production."
  );
  return new LocalBus();
}

async function main(): Promise<void> {
  // Issue #1174: Initialize insurance marketplace with default providers and products
  insuranceMarketplace.initializeDefaults();

  const config = loadConfig();
  const bus = buildBus(config.redisUrl);
  const store = new EventStore(config.indexerDbPath);

  const httpServer = createServer((req, res) =>
    handleHttpRequest(req, res, { authSecret: config.authSecret, tokenTtlSeconds: config.tokenTtlSeconds })
  );

  attachLoanSocketServer({
    httpServer,
    bus,
    store,
    authSecret: config.authSecret,
    connectionQueueMax: config.connectionQueueMax,
  });

  attachMetricsWsServer({
    httpServer,
    bus,
    store,
    authSecret: config.authSecret,
    connectionQueueMax: config.connectionQueueMax,
  });

  const bridge = new Bridge({
    bus,
    store,
    instanceId: config.instanceId,
    pollIntervalMs: config.bridgePollIntervalMs,
    leaderLockTtlMs: config.leaderLockTtlMs,
  });
  bridge.start();

  httpServer.listen(config.port, () => {
    console.log(
      `[quorum-credit-broadcast-server] instance=${config.instanceId} listening on :${config.port} ` +
        `(redis=${config.redisUrl ? "on" : "off"})`
    );
  });

  const shutdown = async (): Promise<void> => {
    await bridge.stop();
    httpServer.close();
    await bus.close();
    process.exit(0);
  };
  process.on("SIGINT", () => void shutdown());
  process.on("SIGTERM", () => void shutdown());
}

const isMain = process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
if (isMain) {
  main().catch((err) => {
    console.error("[quorum-credit-broadcast-server] fatal startup error", err);
    process.exit(1);
  });
}
