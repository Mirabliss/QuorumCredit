import type { IncomingMessage, ServerResponse } from "node:http";
import { issueToken } from "../auth/tokens.js";
import { metrics } from "./metricsRegistry.js";
import { expenseStore, isExpenseCategory } from "../expenses/expenseStore.js";
import { recurringPaymentStore } from "../recurring/recurringPaymentStore.js";

export interface RouteContext {
  authSecret: string;
  tokenTtlSeconds: number;
  webhookSecret?: string; // Optional: secret for receiving webhooks
}

interface TokenRequestBody {
  apiKey?: string;
  borrower?: string;
}

interface ExpenseRequestBody {
  category?: string;
  amount?: number;
  description?: string;
  declaredPurpose?: string;
}

interface RecurringPaymentRequestBody {
  amount?: number;
  frequencySeconds?: number;
  startDate?: number;
}

/** Minimal router for the handful of REST endpoints this service exposes — not
 * pulling in Express for three routes. */
export function handleHttpRequest(
  req: IncomingMessage,
  res: ServerResponse,
  ctx: RouteContext
): void {
  const url = new URL(req.url ?? "", "http://internal");

  // Health check
  if (req.method === "GET" && url.pathname === "/health") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ status: "ok" }));
    return;
  }

  // Metrics endpoint
  if (req.method === "GET" && url.pathname === "/metrics") {
    res.writeHead(200, { "content-type": "text/plain; version=0.0.4" });
    res.end(metrics.toPrometheusText());
    return;
  }

  const expensesMatch = url.pathname.match(/^\/loans\/([^/]+)\/expenses$/);
  if (expensesMatch) {
    const loanId = decodeURIComponent(expensesMatch[1] as string);

    if (req.method === "POST") {
      readJsonBody<ExpenseRequestBody>(req)
        .then((body) => {
          if (!isExpenseCategory(body.category)) {
            res.writeHead(400, { "content-type": "application/json" });
            res.end(JSON.stringify({ error: "category must be one of business, education, healthcare, other" }));
            return;
          }
          if (typeof body.amount !== "number" || !Number.isFinite(body.amount) || body.amount <= 0) {
            res.writeHead(400, { "content-type": "application/json" });
            res.end(JSON.stringify({ error: "amount must be a positive number" }));
            return;
          }
          if (body.declaredPurpose) {
            expenseStore.setDeclaredPurpose(loanId, body.declaredPurpose);
          }
          const expense = expenseStore.addExpense(loanId, body.category, body.amount, body.description ?? "");
          metrics.incCounter("qc_expenses_recorded_total");
          res.writeHead(201, { "content-type": "application/json" });
          res.end(JSON.stringify(expense));
        })
        .catch(() => {
          res.writeHead(400, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: "invalid request body" }));
        });
      return;
    }

    if (req.method === "GET") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(expenseStore.listExpenses(loanId)));
      return;
    }
  }

  const expenseBreakdownMatch = url.pathname.match(/^\/loans\/([^/]+)\/expense-breakdown$/);
  if (expenseBreakdownMatch && req.method === "GET") {
    const loanId = decodeURIComponent(expenseBreakdownMatch[1] as string);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(expenseStore.breakdown(loanId)));
    return;
  }

  const recurringPaymentMatch = url.pathname.match(/^\/loans\/([^/]+)\/recurring-payment$/);
  if (recurringPaymentMatch) {
    const loanId = decodeURIComponent(recurringPaymentMatch[1] as string);

    if (req.method === "POST") {
      readJsonBody<RecurringPaymentRequestBody>(req)
        .then((body) => {
          if (typeof body.amount !== "number" || !Number.isFinite(body.amount) || body.amount <= 0) {
            res.writeHead(400, { "content-type": "application/json" });
            res.end(JSON.stringify({ error: "amount must be a positive number" }));
            return;
          }
          if (typeof body.frequencySeconds !== "number" || body.frequencySeconds <= 0) {
            res.writeHead(400, { "content-type": "application/json" });
            res.end(JSON.stringify({ error: "frequencySeconds must be a positive number" }));
            return;
          }
          const startDate = typeof body.startDate === "number" ? body.startDate : Date.now();
          const schedule = recurringPaymentStore.setup(loanId, body.amount, body.frequencySeconds, startDate);
          metrics.incCounter("qc_recurring_payments_setup_total");
          res.writeHead(201, { "content-type": "application/json" });
          res.end(JSON.stringify(schedule));
        })
        .catch(() => {
          res.writeHead(400, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: "invalid request body" }));
        });
      return;
    }

    if (req.method === "GET") {
      const schedule = recurringPaymentStore.get(loanId);
      if (!schedule) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "no recurring payment schedule for this loan" }));
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({ ...schedule, successRateBps: recurringPaymentStore.successRateBps(loanId) })
      );
      return;
    }

    // Early termination (issue #1168).
    if (req.method === "DELETE") {
      const terminated = recurringPaymentStore.terminate(loanId);
      if (!terminated) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "no recurring payment schedule for this loan" }));
        return;
      }
      metrics.incCounter("qc_recurring_payments_terminated_total");
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ loanId, active: false }));
      return;
    }
  }

  const recurringExecuteMatch = url.pathname.match(/^\/loans\/([^/]+)\/recurring-payment\/execute$/);
  if (recurringExecuteMatch && req.method === "POST") {
    const loanId = decodeURIComponent(recurringExecuteMatch[1] as string);
    executeRecurringPayment(loanId)
      .then((result) => {
        metrics.incCounter(result.ok ? "qc_recurring_payments_success_total" : "qc_recurring_payments_failed_total");
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(result));
      })
      .catch(() => {
        res.writeHead(500, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "recurring payment execution failed unexpectedly" }));
      });
    return;
  }

  if (req.method === "POST" && url.pathname === "/api/auth/token") {
    readJsonBody<TokenRequestBody>(req)
      .then((body) => {
        // NOTE: this issues a token to anyone who asks with any apiKey string — real
        // deployments must swap in a genuine credential check (e.g. verifying apiKey
        // against a provisioned-keys store) before going to production. Wiring that
        // check is intentionally left as a single, obvious seam (this block) rather
        // than left implicit, since this repo has no existing API-key store to
        // integrate against.
        if (!body.apiKey) {
          res.writeHead(400, { "content-type": "application/json" });
          res.end(JSON.stringify({ error: "apiKey required" }));
          return;
        }
        const issued = issueToken(ctx.authSecret, body.apiKey, ctx.tokenTtlSeconds, body.borrower);
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(issued));
      })
      .catch(() => {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: "invalid request body" }));
      });
    return;
  }

  // Webhook endpoints
  if (url.pathname.startsWith("/api/webhooks") || url.pathname === "/webhook") {
    const webhookCtx: WebhookRoutesContext = {
      webhookSecret: ctx.webhookSecret,
    };
    handleWebhookRequest(req, res, webhookCtx);
    return;
  }

  // Not found
  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: "not found" }));
}

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

/**
 * Executes a due recurring payment with retry-with-backoff and borrower
 * notification on exhaustion, per issue #1168.
 */
async function executeRecurringPayment(loanId: string) {
  return recurringPaymentStore.executeWithRetry(
    loanId,
    () => submitOnChainRecurringPayment(loanId),
    (id) => notifyBorrowerOfMissedPayment(id)
  );
}

// NOTE: this always reports success — a real deployment must swap in a
// Soroban RPC call to QuorumCreditContract.execute_recurring_payment
// (src/recurring_payment.rs) here. Wiring that call is intentionally left as
// a single, obvious seam (this function) since this service has no chain
// client wired in yet (see EventStore's read-only-indexer-tailing note).
async function submitOnChainRecurringPayment(_loanId: string): Promise<boolean> {
  return true;
}

function notifyBorrowerOfMissedPayment(loanId: string): void {
  console.warn(
    `[quorum-credit-broadcast-server] recurring payment for loan ${loanId} failed after retries; notifying borrower`
  );
  metrics.incCounter("qc_recurring_payments_notifications_total");
}
