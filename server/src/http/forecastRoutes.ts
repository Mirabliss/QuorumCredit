/**
 * Loan Forecast Route for QuorumCredit
 *
 * Implements GET /loans/{id}/forecast (#1XXX) and the accuracy-tracking
 * endpoint used to compare forecasted vs. actual payments over time.
 */

import type { IncomingMessage, ServerResponse } from "node:http";
import {
  generateForecast,
  defaultTermsFor,
  forecastAccuracyTracker,
  type LoanTerms,
} from "../forecasting/forecastEngine.js";

interface RecordActualBody {
  paymentNumber?: number;
  forecastedAmount?: number;
  actualAmount?: number;
}

/**
 * GET /loans/{id}/forecast[?principal=&interestRateBps=&termDays=&paymentFrequencyDays=]
 *
 * Query params let a caller override the default loan terms until a real
 * loan-terms registry backs this endpoint (see forecastEngine.ts note).
 */
export function handleLoanForecastRequest(
  req: IncomingMessage,
  res: ServerResponse,
  loanId: string,
  url: URL
): void {
  if (req.method !== "GET") {
    res.writeHead(405, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "method not allowed" }));
    return;
  }

  try {
    const terms = parseTermsFromQuery(loanId, url);
    const forecast = generateForecast(terms);

    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        ...forecast,
        accuracy: {
          meanErrorBps: forecastAccuracyTracker.meanErrorBps(loanId),
          sampleCount: forecastAccuracyTracker.samples(loanId).length,
        },
      })
    );
  } catch (error) {
    console.error("Error generating loan forecast:", error);
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "internal server error" }));
  }
}

/**
 * POST /loans/{id}/forecast/accuracy
 *
 * Records an actual payment against a previously forecasted amount, so
 * forecast accuracy can be tracked over time.
 */
export function handleRecordForecastAccuracy(
  req: IncomingMessage,
  res: ServerResponse,
  loanId: string
): void {
  if (req.method !== "POST") {
    res.writeHead(405, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: "method not allowed" }));
    return;
  }

  readJsonBody<RecordActualBody>(req)
    .then((body) => {
      if (
        typeof body.paymentNumber !== "number" ||
        typeof body.forecastedAmount !== "number" ||
        typeof body.actualAmount !== "number"
      ) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(
          JSON.stringify({ error: "paymentNumber, forecastedAmount, and actualAmount are required numbers" })
        );
        return;
      }

      forecastAccuracyTracker.recordActual(
        loanId,
        body.paymentNumber,
        body.forecastedAmount,
        body.actualAmount
      );

      res.writeHead(201, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          loanId,
          meanErrorBps: forecastAccuracyTracker.meanErrorBps(loanId),
          sampleCount: forecastAccuracyTracker.samples(loanId).length,
        })
      );
    })
    .catch(() => {
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "invalid request body" }));
    });
}

function parseTermsFromQuery(loanId: string, url: URL): LoanTerms {
  const defaults = defaultTermsFor(loanId);
  const principal = parseNumberParam(url, "principal") ?? defaults.principal;
  const interestRateBps = parseNumberParam(url, "interestRateBps") ?? defaults.interestRateBps;
  const termDays = parseNumberParam(url, "termDays") ?? defaults.termDays;
  const paymentFrequencyDays = parseNumberParam(url, "paymentFrequencyDays") ?? defaults.paymentFrequencyDays;

  return {
    loanId,
    principal,
    interestRateBps,
    termDays,
    paymentFrequencyDays,
    startTimestampMs: defaults.startTimestampMs,
  };
}

function parseNumberParam(url: URL, key: string): number | undefined {
  const raw = url.searchParams.get(key);
  if (raw === null) return undefined;
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}

function readJsonBody<T>(req: IncomingMessage): Promise<T> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      try {
        resolve(chunks.length > 0 ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : ({} as T));
      } catch (e) {
        reject(e);
      }
    });
    req.on("error", reject);
  });
}
