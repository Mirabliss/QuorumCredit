export interface RecurringPaymentSchedule {
  loanId: string;
  amount: number;
  frequencySeconds: number;
  startDate: number;
  nextPaymentDue: number;
  active: boolean;
  successCount: number;
  failureCount: number;
  retryCount: number;
  createdAt: number;
}

export interface RecurringPaymentAttemptResult {
  ok: boolean;
  retriesUsed: number;
  notifiedBorrower: boolean;
}

/** Number of retry attempts after an initial failed transfer, before the
 * schedule gives up on that period and notifies the borrower. */
const MAX_RETRIES = 3;

/**
 * In-memory recurring-payment scheduler for issue #1168. This service owns
 * the off-chain orchestration (schedule bookkeeping, retry-with-backoff,
 * borrower notification); actual fund movement happens on-chain via
 * `QuorumCreditContract.execute_recurring_payment`
 * (src/recurring_payment.rs), which the `transfer` callback passed to
 * `executeWithRetry` is expected to invoke over Soroban RPC.
 */
export class RecurringPaymentStore {
  private readonly byLoan = new Map<string, RecurringPaymentSchedule>();

  setup(loanId: string, amount: number, frequencySeconds: number, startDate: number): RecurringPaymentSchedule {
    const schedule: RecurringPaymentSchedule = {
      loanId,
      amount,
      frequencySeconds,
      startDate,
      nextPaymentDue: startDate,
      active: true,
      successCount: 0,
      failureCount: 0,
      retryCount: 0,
      createdAt: Date.now(),
    };
    this.byLoan.set(loanId, schedule);
    return schedule;
  }

  get(loanId: string): RecurringPaymentSchedule | undefined {
    return this.byLoan.get(loanId);
  }

  /** Early termination, per issue #1168. Returns false if no schedule exists. */
  terminate(loanId: string): boolean {
    const schedule = this.byLoan.get(loanId);
    if (!schedule) return false;
    schedule.active = false;
    return true;
  }

  successRateBps(loanId: string): number {
    const schedule = this.byLoan.get(loanId);
    if (!schedule) return 0;
    const attempts = schedule.successCount + schedule.failureCount;
    return attempts === 0 ? 0 : Math.round((schedule.successCount / attempts) * 10_000);
  }

  /**
   * Attempt a due payment, retrying up to `MAX_RETRIES` additional times on
   * failure before giving up for this period and notifying the borrower.
   * `transfer` performs the actual on-chain submission and resolves to
   * whether it succeeded.
   */
  async executeWithRetry(
    loanId: string,
    transfer: () => Promise<boolean>,
    notifyBorrower: (loanId: string, schedule: RecurringPaymentSchedule) => void
  ): Promise<RecurringPaymentAttemptResult> {
    const schedule = this.byLoan.get(loanId);
    if (!schedule || !schedule.active) {
      return { ok: false, retriesUsed: 0, notifiedBorrower: false };
    }

    let retriesUsed = 0;
    for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
      retriesUsed = attempt;
      const ok = await transfer();
      if (ok) {
        schedule.successCount += 1;
        schedule.retryCount = 0;
        schedule.nextPaymentDue += schedule.frequencySeconds;
        return { ok: true, retriesUsed, notifiedBorrower: false };
      }
    }

    schedule.retryCount = retriesUsed;
    schedule.failureCount += 1;
    notifyBorrower(loanId, schedule);
    return { ok: false, retriesUsed, notifiedBorrower: true };
  }
}

export const recurringPaymentStore = new RecurringPaymentStore();
