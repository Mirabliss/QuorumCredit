/**
 * Loan Forecasting Engine for QuorumCredit
 *
 * #1XXX: Borrowers can't predict loan outcomes today — this module produces
 * an amortization schedule plus scenario analysis (base/optimistic/
 * pessimistic) and early-repayment interest-savings estimates for the
 * GET /loans/{id}/forecast endpoint (server/src/http/forecastRoutes.ts).
 *
 * Interest accrues daily-compounding on the outstanding principal, mirroring
 * `calculate_daily_compound_interest` in src/loan.rs. This module does not
 * read on-chain state directly (the server has no chain client wired in
 * yet — see EventStore's read-only-indexer-tailing note); loan terms are
 * supplied by the caller (query params) or the in-memory registry below,
 * which real deployments should replace with a lookup against the indexer.
 */

const SECS_PER_DAY = 86_400;
const BPS_DENOMINATOR = 10_000;

export interface LoanTerms {
  loanId: string;
  principal: number;
  /** Annualized interest rate in basis points, applied as daily compound interest. */
  interestRateBps: number;
  /** Loan term length in days. */
  termDays: number;
  /** Scheduled payment frequency, in days. */
  paymentFrequencyDays: number;
  startTimestampMs: number;
}

export interface ScheduledPayment {
  paymentNumber: number;
  dueDateMs: number;
  principalPortion: number;
  interestPortion: number;
  totalPayment: number;
  remainingPrincipal: number;
}

export interface ScenarioForecast {
  scenario: "base" | "optimistic" | "pessimistic";
  description: string;
  effectiveRateBps: number;
  totalInterest: number;
  totalRepayment: number;
  schedule: ScheduledPayment[];
}

export interface EarlyRepaymentSavings {
  repayAtPaymentNumber: number;
  interestSaved: number;
  principalRemainingAtRepayment: number;
}

export interface LoanForecast {
  loanId: string;
  generatedAtMs: number;
  terms: LoanTerms;
  scenarios: ScenarioForecast[];
  earlyRepaymentSavings: EarlyRepaymentSavings[];
}

/** Optimistic/pessimistic scenarios adjust the effective rate to model
 * favorable or adverse conditions (e.g. credit-score-driven rate changes,
 * missed-payment penalty accrual) without needing real market data. */
const SCENARIO_RATE_ADJUSTMENTS: Record<ScenarioForecast["scenario"], number> = {
  optimistic: -0.15, // 15% lower effective rate (e.g. early good-standing discount)
  base: 0,
  pessimistic: 0.25, // 25% higher effective rate (e.g. penalty accrual on lateness)
};

const SCENARIO_DESCRIPTIONS: Record<ScenarioForecast["scenario"], string> = {
  base: "Scheduled payments made on time at the loan's stated interest rate.",
  optimistic: "Borrower maintains good standing; discounted effective rate applies.",
  pessimistic: "Borrower incurs late-payment penalties; elevated effective rate applies.",
};

/** Builds a full amortization schedule for the given terms and effective
 * rate, using daily-compounding interest accrued between payment dates
 * (mirrors the on-chain daily-compound calculation in src/loan.rs). */
function buildSchedule(terms: LoanTerms, effectiveRateBps: number): ScheduledPayment[] {
  const schedule: ScheduledPayment[] = [];
  let remainingPrincipal = terms.principal;
  const numPayments = Math.max(1, Math.ceil(terms.termDays / terms.paymentFrequencyDays));
  const principalPerPayment = terms.principal / numPayments;
  const dailyRate = effectiveRateBps / BPS_DENOMINATOR / 365;

  let dueDateMs = terms.startTimestampMs;
  for (let i = 1; i <= numPayments; i++) {
    dueDateMs += terms.paymentFrequencyDays * SECS_PER_DAY * 1000;

    const interestForPeriod =
      remainingPrincipal * (Math.pow(1 + dailyRate, terms.paymentFrequencyDays) - 1);
    const principalPortion = Math.min(principalPerPayment, remainingPrincipal);
    remainingPrincipal = Math.max(0, remainingPrincipal - principalPortion);

    schedule.push({
      paymentNumber: i,
      dueDateMs,
      principalPortion: round2(principalPortion),
      interestPortion: round2(interestForPeriod),
      totalPayment: round2(principalPortion + interestForPeriod),
      remainingPrincipal: round2(remainingPrincipal),
    });
  }

  return schedule;
}

function round2(n: number): number {
  return Math.round(n * 100) / 100;
}

function buildScenario(terms: LoanTerms, scenario: ScenarioForecast["scenario"]): ScenarioForecast {
  const adjustment = SCENARIO_RATE_ADJUSTMENTS[scenario];
  const effectiveRateBps = Math.max(0, Math.round(terms.interestRateBps * (1 + adjustment)));
  const schedule = buildSchedule(terms, effectiveRateBps);
  const totalInterest = round2(schedule.reduce((sum, p) => sum + p.interestPortion, 0));
  const totalRepayment = round2(schedule.reduce((sum, p) => sum + p.totalPayment, 0));

  return {
    scenario,
    description: SCENARIO_DESCRIPTIONS[scenario],
    effectiveRateBps,
    totalInterest,
    totalRepayment,
    schedule,
  };
}

/** Interest saved by repaying in full at each payment point in the base
 * schedule, vs. carrying the loan to term. */
function computeEarlyRepaymentSavings(baseSchedule: ScenarioForecast): EarlyRepaymentSavings[] {
  const savings: EarlyRepaymentSavings[] = [];
  const totalInterest = baseSchedule.totalInterest;

  let interestAccrued = 0;
  for (const payment of baseSchedule.schedule) {
    interestAccrued += payment.interestPortion;
    const remainingInterestIfCarried = round2(totalInterest - interestAccrued);
    savings.push({
      repayAtPaymentNumber: payment.paymentNumber,
      interestSaved: Math.max(0, remainingInterestIfCarried),
      principalRemainingAtRepayment: payment.remainingPrincipal,
    });
  }
  return savings;
}

export function generateForecast(terms: LoanTerms): LoanForecast {
  const scenarios: ScenarioForecast[] = [
    buildScenario(terms, "base"),
    buildScenario(terms, "optimistic"),
    buildScenario(terms, "pessimistic"),
  ];

  const baseScenario = scenarios.find((s) => s.scenario === "base")!;

  return {
    loanId: terms.loanId,
    generatedAtMs: Date.now(),
    terms,
    scenarios,
    earlyRepaymentSavings: computeEarlyRepaymentSavings(baseScenario),
  };
}

export interface ForecastAccuracySample {
  recordedAtMs: number;
  paymentNumber: number;
  forecastedAmount: number;
  actualAmount: number;
  errorBps: number;
}

/**
 * Tracks forecast accuracy over time by comparing a scenario's predicted
 * payment amounts against what a borrower actually paid, per the "track
 * forecast accuracy over time" requirement.
 */
export class ForecastAccuracyTracker {
  private readonly samplesByLoan = new Map<string, ForecastAccuracySample[]>();

  recordActual(loanId: string, paymentNumber: number, forecastedAmount: number, actualAmount: number): void {
    const errorBps =
      forecastedAmount === 0
        ? 0
        : Math.round((Math.abs(actualAmount - forecastedAmount) / forecastedAmount) * BPS_DENOMINATOR);

    const sample: ForecastAccuracySample = {
      recordedAtMs: Date.now(),
      paymentNumber,
      forecastedAmount,
      actualAmount,
      errorBps,
    };

    const list = this.samplesByLoan.get(loanId);
    if (list) list.push(sample);
    else this.samplesByLoan.set(loanId, [sample]);
  }

  samples(loanId: string): ForecastAccuracySample[] {
    return this.samplesByLoan.get(loanId) ?? [];
  }

  /** Mean absolute percentage error across all recorded samples for a loan,
   * in basis points. Lower is more accurate. */
  meanErrorBps(loanId: string): number {
    const samples = this.samples(loanId);
    if (samples.length === 0) return 0;
    return Math.round(samples.reduce((sum, s) => sum + s.errorBps, 0) / samples.length);
  }
}

export const forecastAccuracyTracker = new ForecastAccuracyTracker();

/** Default loan terms used when the caller doesn't supply query params and
 * no registered terms exist for the loan — keeps the endpoint usable
 * without requiring a full loan-terms registry in this iteration. */
export function defaultTermsFor(loanId: string): LoanTerms {
  return {
    loanId,
    principal: 1_000,
    interestRateBps: 1_200, // 12% annualized
    termDays: 180,
    paymentFrequencyDays: 30,
    startTimestampMs: Date.now(),
  };
}
