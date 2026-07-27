/**
 * Webhook Delivery Service for QuorumCredit
 *
 * #1XXX: Real-time webhook delivery with retry + delivery-rate tracking.
 *
 * Third-party integrators previously had to poll REST endpoints for loan
 * status changes. This module pushes events to registered subscriber URLs
 * as they happen, with exponential-backoff retries and per-webhook delivery
 * accounting so operators can see whether a subscriber endpoint is healthy.
 */

import { createSignedWebhookRequest, type WebhookRegistration } from "./signature.js";

/** Canonical event types third-party apps can subscribe to. The legacy
 * dot-separated event names (loan.disbursed, etc.) remain supported in
 * signature.ts's validEvents list for backward compatibility, but new
 * integrations should use these. */
export type WebhookEventType =
  | "loan_issued"
  | "payment_received"
  | "loan_completed"
  | "default_occurred";

export const SUBSCRIBABLE_EVENTS: WebhookEventType[] = [
  "loan_issued",
  "payment_received",
  "loan_completed",
  "default_occurred",
];

export interface DeliveryAttempt {
  attempt: number;
  atMs: number;
  ok: boolean;
  statusCode?: number;
  error?: string;
}

export interface DeliveryRecord {
  id: string;
  webhookId: string;
  event: string;
  createdAt: number;
  completedAt?: number;
  attempts: DeliveryAttempt[];
  status: "pending" | "delivered" | "failed";
}

export interface DeliveryStats {
  webhookId: string;
  totalDeliveries: number;
  delivered: number;
  failed: number;
  successRateBps: number;
  averageAttempts: number;
}

/** Retries after the initial attempt, before a delivery is marked failed. */
export const MAX_RETRIES = 5;

/** Base delay for exponential backoff between delivery attempts (ms). Actual
 * delay for retry N is BASE_DELAY_MS * 2^(N-1), e.g. 500ms, 1s, 2s, 4s, 8s. */
export const BASE_DELAY_MS = 500;

export function backoffDelayMs(retryNumber: number): number {
  return BASE_DELAY_MS * Math.pow(2, retryNumber - 1);
}

/** Function that actually performs the HTTP POST to a subscriber's URL.
 * Injected so this module has no hard dependency on a particular HTTP
 * client and remains easy to unit test. */
export type WebhookSender = (
  url: string,
  headers: Record<string, string>,
  body: unknown
) => Promise<{ ok: boolean; statusCode?: number; error?: string }>;

/**
 * Tracks webhook delivery attempts (with exponential-backoff retry) and
 * aggregate success rates per webhook subscription, in-memory. In production
 * this should be backed by a durable queue (e.g. a database table plus a
 * background worker) so retries survive process restarts, but the retry and
 * accounting logic here is deployment-agnostic.
 */
export class WebhookDeliveryService {
  private readonly deliveries = new Map<string, DeliveryRecord>();
  private readonly byWebhook = new Map<string, string[]>();
  private nextId = 1;

  /** Deliver an event to a single subscriber, retrying on failure with
   * exponential backoff up to MAX_RETRIES additional attempts. Returns the
   * final delivery record. */
  async deliver(
    registration: WebhookRegistration,
    event: string,
    data: unknown,
    send: WebhookSender,
    sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms))
  ): Promise<DeliveryRecord> {
    const signed = createSignedWebhookRequest(registration, event, data);

    const record: DeliveryRecord = {
      id: `dlv_${this.nextId++}_${Date.now()}`,
      webhookId: registration.id,
      event,
      createdAt: Date.now(),
      attempts: [],
      status: "pending",
    };
    this.deliveries.set(record.id, record);
    const list = this.byWebhook.get(registration.id);
    if (list) list.push(record.id);
    else this.byWebhook.set(registration.id, [record.id]);

    for (let attempt = 1; attempt <= MAX_RETRIES + 1; attempt++) {
      let result: { ok: boolean; statusCode?: number; error?: string };
      try {
        result = await send(signed.url, signed.headers, signed.payload);
      } catch (error) {
        result = { ok: false, error: error instanceof Error ? error.message : "unknown error" };
      }

      record.attempts.push({
        attempt,
        atMs: Date.now(),
        ok: result.ok,
        statusCode: result.statusCode,
        error: result.error,
      });

      if (result.ok) {
        record.status = "delivered";
        record.completedAt = Date.now();
        return record;
      }

      const isLastAttempt = attempt === MAX_RETRIES + 1;
      if (!isLastAttempt) {
        await sleep(backoffDelayMs(attempt));
      }
    }

    record.status = "failed";
    record.completedAt = Date.now();
    return record;
  }

  getDelivery(id: string): DeliveryRecord | undefined {
    return this.deliveries.get(id);
  }

  listDeliveries(webhookId: string): DeliveryRecord[] {
    const ids = this.byWebhook.get(webhookId) ?? [];
    return ids
      .map((id) => this.deliveries.get(id))
      .filter((d): d is DeliveryRecord => d !== undefined);
  }

  /** Success-rate and attempt accounting for a single webhook subscription,
   * per the "track webhook delivery success rates" requirement. */
  stats(webhookId: string): DeliveryStats {
    const records = this.listDeliveries(webhookId);
    const completed = records.filter((r) => r.status !== "pending");
    const delivered = completed.filter((r) => r.status === "delivered").length;
    const failed = completed.filter((r) => r.status === "failed").length;
    const totalAttempts = completed.reduce((sum, r) => sum + r.attempts.length, 0);

    return {
      webhookId,
      totalDeliveries: completed.length,
      delivered,
      failed,
      successRateBps: completed.length === 0 ? 0 : Math.round((delivered / completed.length) * 10_000),
      averageAttempts: completed.length === 0 ? 0 : totalAttempts / completed.length,
    };
  }
}

export const webhookDeliveryService = new WebhookDeliveryService();
