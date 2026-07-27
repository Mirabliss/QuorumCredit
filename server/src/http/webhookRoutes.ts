/**
 * Webhook Routes for QuorumCredit
 * 
 * Implements webhook registration and verification endpoints.
 */

import type { IncomingMessage, ServerResponse } from "node:http";
import { 
  webhookRegistry, 
  validateIncomingWebhook,
  createSignedWebhookRequest,
} from "../webhooks/signature.js";

export interface WebhookRoutesContext {
  webhookSecret?: string; // Secret for receiving webhooks (if this service receives webhooks)
}

interface RegisterWebhookBody {
  url: string;
  events: string[];
}

interface UpdateWebhookBody {
  url?: string;
  events?: string[];
  enabled?: boolean;
}

interface TestWebhookBody {
  event: string;
  data: any;
}

/**
 * Handle webhook-related HTTP requests
 */
export function handleWebhookRequest(
  req: IncomingMessage,
  res: ServerResponse,
  ctx: WebhookRoutesContext
): void {
  const url = new URL(req.url ?? "", "http://internal");

  // Webhook registration endpoints (protected in production)
  if (req.method === "POST" && url.pathname === "/api/webhooks/register") {
    handleRegisterWebhook(req, res);
    return;
  }

  if (req.method === "GET" && url.pathname === "/api/webhooks") {
    handleListWebhooks(req, res);
    return;
  }

  if (req.method === "GET" && url.pathname.startsWith("/api/webhooks/")) {
    const id = url.pathname.split("/").pop();
    if (id && id !== "register") {
      handleGetWebhook(req, res, id);
      return;
    }
  }

  if (req.method === "PUT" && url.pathname.startsWith("/api/webhooks/")) {
    const id = url.pathname.split("/").pop();
    if (id && id !== "register") {
      handleUpdateWebhook(req, res, id);
      return;
    }
  }

  if (req.method === "DELETE" && url.pathname.startsWith("/api/webhooks/")) {
    const id = url.pathname.split("/").pop();
    if (id && id !== "register") {
      handleDeleteWebhook(req, res, id);
      return;
    }
  }

  if (req.method === "POST" && url.pathname.startsWith("/api/webhooks/") && url.pathname.endsWith("/test")) {
    const id = url.pathname.split("/")[3]; // /api/webhooks/{id}/test
    if (id) {
      handleTestWebhook(req, res, id);
      return;
    }
  }

  // Webhook verification endpoint (for receiving webhooks)
  if (req.method === "POST" && url.pathname === "/webhook") {
    handleIncomingWebhook(req, res, ctx);
    return;
  }

  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: "not found" }));
}

/**
 * Register a new webhook
 */
async function handleRegisterWebhook(req: IncomingMessage, res: ServerResponse): Promise<void> {
  try {
    const body = await readJsonBody<RegisterWebhookBody>(req);
    
    if (!body.url || !body.events || !Array.isArray(body.events)) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "url and events array are required" }));
      return;
    }

    // Validate URL
    try {
      new URL(body.url);
    } catch {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "invalid URL" }));
      return;
    }

    // Validate events
    const validEvents = [
      "loan.requested",
      "loan.disbursed",
      "loan.repaid",
      "loan.defaulted",
      "vouch.created",
      "vouch.withdrawn",
      "slash.executed",
      "config.updated",
    ];

    const invalidEvents = body.events.filter(event => !validEvents.includes(event));
    if (invalidEvents.length > 0) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ 
        error: "invalid events", 
        invalidEvents,
        validEvents 
      }));
      return;
    }

    // Register webhook
    const registration = webhookRegistry.registerWebhook(body.url, body.events);

    // Return registration (excluding secret for security)
    const { secret, ...safeRegistration } = registration;
    
    res.writeHead(201, { "content-type": "application/json" });
    res.end(JSON.stringify({
      ...safeRegistration,
      message: "Webhook registered successfully. Save the secret shown below - it will not be shown again.",
      secret, // Only returned once during registration
    }));
  } catch (error) {
    console.error("Error registering webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * List all webhooks
 */
function handleListWebhooks(_req: IncomingMessage, res: ServerResponse): void {
  try {
    const webhooks = webhookRegistry.listWebhooks();
    
    // Remove secrets from response
    const safeWebhooks = webhooks.map(({ secret, ...rest }) => rest);
    
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(safeWebhooks));
  } catch (error) {
    console.error("Error listing webhooks:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Get a specific webhook
 */
function handleGetWebhook(_req: IncomingMessage, res: ServerResponse, id: string): void {
  try {
    const webhook = webhookRegistry.getWebhook(id);
    
    if (!webhook) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook not found" }));
      return;
    }

    // Remove secret from response
    const { secret, ...safeWebhook } = webhook;
    
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(safeWebhook));
  } catch (error) {
    console.error("Error getting webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Update a webhook
 */
async function handleUpdateWebhook(req: IncomingMessage, res: ServerResponse, id: string): Promise<void> {
  try {
    const webhook = webhookRegistry.getWebhook(id);
    
    if (!webhook) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook not found" }));
      return;
    }

    const body = await readJsonBody<UpdateWebhookBody>(req);

    // Validate updates
    if (body.url !== undefined) {
      try {
        new URL(body.url);
      } catch {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "invalid URL" }));
        return;
      }
      webhook.url = body.url;
    }

    if (body.events !== undefined) {
      if (!Array.isArray(body.events)) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "events must be an array" }));
        return;
      }
      webhook.events = body.events;
    }

    if (body.enabled !== undefined) {
      if (body.enabled) {
        webhookRegistry.enableWebhook(id);
      } else {
        webhookRegistry.disableWebhook(id);
      }
    }

    // Update last used timestamp
    webhookRegistry.updateLastUsed(id);

    // Return updated webhook (excluding secret)
    const { secret, ...safeWebhook } = webhook;
    
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(safeWebhook));
  } catch (error) {
    console.error("Error updating webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Delete a webhook
 */
function handleDeleteWebhook(_req: IncomingMessage, res: ServerResponse, id: string): void {
  try {
    const deleted = webhookRegistry.deleteWebhook(id);
    
    if (!deleted) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook not found" }));
      return;
    }

    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ message: "webhook deleted" }));
  } catch (error) {
    console.error("Error deleting webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Test a webhook
 */
async function handleTestWebhook(req: IncomingMessage, res: ServerResponse, id: string): Promise<void> {
  try {
    const webhook = webhookRegistry.getWebhook(id);
    
    if (!webhook) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook not found" }));
      return;
    }

    if (!webhook.enabled) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook is disabled" }));
      return;
    }

    const body = await readJsonBody<TestWebhookBody>(req);
    
    if (!body.event || !body.data) {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "event and data are required" }));
      return;
    }

    // Create signed webhook request
    const signedRequest = createSignedWebhookRequest(webhook, body.event, body.data);

    // In a real implementation, you would send this request to the webhook URL
    // For now, we'll just return the signed request details
    
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      message: "Test webhook created",
      signedRequest: {
        url: signedRequest.url,
        headers: signedRequest.headers,
        payload: signedRequest.payload,
      },
      instructions: "Send a POST request to the URL with these headers and payload to test your webhook endpoint",
    }));
  } catch (error) {
    console.error("Error testing webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Handle incoming webhook (for services that receive webhooks)
 */
async function handleIncomingWebhook(
  req: IncomingMessage,
  res: ServerResponse,
  ctx: WebhookRoutesContext
): Promise<void> {
  try {
    if (!ctx.webhookSecret) {
      res.writeHead(501, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "webhook receiving not configured" }));
      return;
    }

    const body = await readJsonBody<any>(req);
    
    // Extract headers
    const headers: Record<string, string | string[]> = {};
    for (const [key, value] of Object.entries(req.headers)) {
      headers[key.toLowerCase()] = value || "";
    }

    // Validate webhook signature
    const validation = validateIncomingWebhook(body, headers, ctx.webhookSecret);
    
    if (!validation.valid) {
      console.warn("Invalid webhook received:", validation.error);
      res.writeHead(401, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "invalid webhook signature", details: validation.error }));
      return;
    }

    // Process valid webhook
    console.log("Received valid webhook:", validation.payload);
    
    // Here you would process the webhook payload based on the event type
    // For now, we'll just acknowledge receipt
    
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ 
      status: "accepted",
      event: validation.payload!.event,
      webhookId: validation.payload!.webhookId,
      timestamp: validation.payload!.timestamp,
    }));
  } catch (error) {
    console.error("Error handling incoming webhook:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * Helper function to read JSON request body
 */
function readJsonBody<T>(req: IncomingMessage): Promise<T> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      try {
        resolve(chunks.length > 0 ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : {} as T);
      } catch (e) {
        reject(e);
      }
    });
    req.on("error", reject);
  });
}