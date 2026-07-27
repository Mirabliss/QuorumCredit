export type ExpenseCategory = "business" | "education" | "healthcare" | "other";

const CATEGORIES: ExpenseCategory[] = ["business", "education", "healthcare", "other"];

export function isExpenseCategory(value: unknown): value is ExpenseCategory {
  return typeof value === "string" && (CATEGORIES as string[]).includes(value);
}

export interface Expense {
  id: number;
  loanId: string;
  category: ExpenseCategory;
  amount: number;
  description: string;
  recordedAt: number;
}

export interface ExpenseBreakdown {
  loanId: string;
  expenseCount: number;
  totalRecorded: number;
  byCategory: Record<ExpenseCategory, number>;
  declaredPurpose: string | null;
  /** Whether recorded expenses appear consistent with the declared loan
   * purpose. `null` when no purpose has been registered for this loan. */
  matchesDeclaredPurpose: boolean | null;
}

/**
 * In-memory expense ledger for borrower loan-use tracking (issue #1170).
 * Not persisted across restarts — this service otherwise only tails the
 * read-only indexer database (see EventStore); expenses are borrower-
 * submitted metadata with no on-chain equivalent, so a bespoke store here
 * (rather than the indexer's sqlite file) keeps write ownership clear.
 */
export class ExpenseStore {
  private readonly byLoan = new Map<string, Expense[]>();
  private readonly declaredPurpose = new Map<string, string>();
  private nextId = 1;

  setDeclaredPurpose(loanId: string, purpose: string): void {
    this.declaredPurpose.set(loanId, purpose);
  }

  addExpense(loanId: string, category: ExpenseCategory, amount: number, description: string): Expense {
    const expense: Expense = {
      id: this.nextId++,
      loanId,
      category,
      amount,
      description,
      recordedAt: Date.now(),
    };
    const list = this.byLoan.get(loanId);
    if (list) {
      list.push(expense);
    } else {
      this.byLoan.set(loanId, [expense]);
    }
    return expense;
  }

  listExpenses(loanId: string): Expense[] {
    return this.byLoan.get(loanId) ?? [];
  }

  breakdown(loanId: string): ExpenseBreakdown {
    const expenses = this.listExpenses(loanId);
    const byCategory: Record<ExpenseCategory, number> = {
      business: 0,
      education: 0,
      healthcare: 0,
      other: 0,
    };
    let totalRecorded = 0;
    for (const expense of expenses) {
      byCategory[expense.category] += expense.amount;
      totalRecorded += expense.amount;
    }

    const purpose = this.declaredPurpose.get(loanId) ?? null;
    return {
      loanId,
      expenseCount: expenses.length,
      totalRecorded,
      byCategory,
      declaredPurpose: purpose,
      matchesDeclaredPurpose: purpose ? matchesPurpose(purpose, byCategory) : null,
    };
  }
}

/** Flags a loan whose recorded expenses skew heavily toward a category that
 * doesn't appear anywhere in the borrower's declared purpose text. This is a
 * simple keyword heuristic, not a verification workflow. */
function matchesPurpose(purpose: string, byCategory: Record<ExpenseCategory, number>): boolean {
  const lowerPurpose = purpose.toLowerCase();
  const [dominantCategory] = (Object.entries(byCategory) as [ExpenseCategory, number][]).sort(
    (a, b) => b[1] - a[1]
  )[0] ?? ["other", 0];

  if (dominantCategory === "other") return true;
  return lowerPurpose.includes(dominantCategory);
}

export const expenseStore = new ExpenseStore();
