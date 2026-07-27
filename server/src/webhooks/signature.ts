/**
 * Webhook Signature Verification Module
 * 
 * #1082: Add Webhook Signature Verification
 * 
 * This module implements HMAC-SHA256 signature verification for webhook requests
 * to prevent spoofing attacks.
 */

import { createHmac, timingSafeEqual } from 'node:crypto';

export interface WebhookRegistration {
  id: string;
  url: string;
  secret: string;
  createdAt: Date;
  lastUsed?: Date;
  events: string[];
  enabled: boolean;
}

export interface WebhookPayload {
  event: string;
  data: any;
  timestamp: number;
  webhookId: string;
}

export interface SignedWebhookRequest {
  url: string;
  payload: WebhookPayload;
  headers: Record<string, string>;
}

/**
 * Generate a new webhook secret (32-byte random hex string)
 */
export function generateWebhookSecret(): string {
  return createHmac('sha256', Math.random().toString())
    .update(Date.now().toString())
    .digest('hex')
    .slice(0, 64); // 32 bytes in hex
}

/**
 * Sign a webhook payload with HMAC-SHA256
 */
export function signWebhookPayload(
  payload: WebhookPayload,
  secret: string
): string {
  const payloadString = JSON.stringify(payload);
  const hmac = createHmac('sha256', secret);
  hmac.update(payloadString);
  return hmac.digest('hex');
}

/**
 * Verify a webhook signature
 */
export function verifyWebhookSignature(
  payload: WebhookPayload,
  signature: string,
  secret: string
): boolean {
  try {
    const expectedSignature = signWebhookPayload(payload, secret);
    return timingSafeEqual(
      Buffer.from(signature, 'hex'),
      Buffer.from(expectedSignature, 'hex')
    );
  } catch (error) {
    return false;
  }
}

/**
 * Create a signed webhook request
 */
export function createSignedWebhookRequest(
  registration: WebhookRegistration,
  event: string,
  data: any
): SignedWebhookRequest {
  const payload: WebhookPayload = {
    event,
    data,
    timestamp: Date.now(),
    webhookId: registration.id,
  };

  const signature = signWebhookPayload(payload, registration.secret);

  return {
    url: registration.url,
    payload,
    headers: {
      'Content-Type': 'application/json',
      'X-Webhook-Event': event,
      'X-Webhook-Timestamp': payload.timestamp.toString(),
      'X-Webhook-Signature': signature,
      'X-Webhook-Signature-Version': 'hmac-sha256',
      'X-Webhook-Id': registration.id,
    },
  };
}

/**
 * Validate incoming webhook request signature
 */
export function validateIncomingWebhook(
  body: any,
  headers: Record<string, string | string[]>,
  secret: string
): { valid: boolean; payload?: WebhookPayload; error?: string } {
  try {
    // Extract signature from headers
    const signature = Array.isArray(headers['x-webhook-signature'])
      ? headers['x-webhook-signature'][0]
      : headers['x-webhook-signature'];

    const timestamp = Array.isArray(headers['x-webhook-timestamp'])
      ? headers['x-webhook-timestamp'][0]
      : headers['x-webhook-timestamp'];

    const webhookId = Array.isArray(headers['x-webhook-id'])
      ? headers['x-webhook-id'][0]
      : headers['x-webhook-id'];

    const event = Array.isArray(headers['x-webhook-event'])
      ? headers['x-webhook-event'][0]
      : headers['x-webhook-event'];

    if (!signature || !timestamp || !webhookId || !event) {
      return {
        valid: false,
        error: 'Missing required headers',
      };
    }

    // Parse timestamp
    const timestampNum = parseInt(timestamp, 10);
    if (isNaN(timestampNum)) {
      return {
        valid: false,
        error: 'Invalid timestamp',
      };
    }

    // Check timestamp freshness (reject requests older than 5 minutes)
    const now = Date.now();
    if (Math.abs(now - timestampNum) > 5 * 60 * 1000) {
      return {
        valid: false,
        error: 'Timestamp too old',
      };
    }

    // Reconstruct payload for verification
    const payload: WebhookPayload = {
      event,
      data: body,
      timestamp: timestampNum,
      webhookId,
    };

    // Verify signature
    if (!verifyWebhookSignature(payload, signature, secret)) {
      return {
        valid: false,
        error: 'Invalid signature',
      };
    }

    return {
      valid: true,
      payload,
    };
  } catch (error) {
    return {
      valid: false,
      error: error instanceof Error ? error.message : 'Unknown error',
    };
  }
}

/**
 * Simple in-memory webhook registration store
 * In production, this should be replaced with a database
 */
export class WebhookRegistry {
  private registrations: Map<string, WebhookRegistration> = new Map();

  /**
   * Register a new webhook
   */
  registerWebhook(url: string, events: string[]): WebhookRegistration {
    const id = `wh_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    const secret = generateWebhookSecret();
    
    const registration: WebhookRegistration = {
      id,
      url,
      secret,
      createdAt: new Date(),
      events,
      enabled: true,
    };

    this.registrations.set(id, registration);
    return registration;
  }

  /**
   * Get webhook registration by ID
   */
  getWebhook(id: string): WebhookRegistration | undefined {
    return this.registrations.get(id);
  }

  /**
   * Update webhook last used timestamp
   */
  updateLastUsed(id: string): void {
    const registration = this.registrations.get(id);
    if (registration) {
      registration.lastUsed = new Date();
      this.registrations.set(id, registration);
    }
  }

  /**
   * Disable a webhook
   */
  disableWebhook(id: string): void {
    const registration = this.registrations.get(id);
    if (registration) {
      registration.enabled = false;
      this.registrations.set(id, registration);
    }
  }

  /**
   * Enable a webhook
   */
  enableWebhook(id: string): void {
    const registration = this.registrations.get(id);
    if (registration) {
      registration.enabled = true;
      this.registrations.set(id, registration);
    }
  }

  /**
   * Delete a webhook
   */
  deleteWebhook(id: string): boolean {
    return this.registrations.delete(id);
  }

  /**
   * List all webhooks
   */
  listWebhooks(): WebhookRegistration[] {
    return Array.from(this.registrations.values());
  }

  /**
   * Get webhooks for a specific event
   */
  getWebhooksForEvent(event: string): WebhookRegistration[] {
    return Array.from(this.registrations.values()).filter(
      (reg) => reg.enabled && reg.events.includes(event)
    );
  }
}

// Export a singleton instance for convenience
export const webhookRegistry = new WebhookRegistry();