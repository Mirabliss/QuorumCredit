export interface CartItem {
  amount: number;
  tenureSeconds: number;
  addedAt: number;
}

export interface LoanCart {
  borrower: string;
  items: CartItem[];
  createdAt: number;
  lastUpdatedAt: number;
  submitted: boolean;
}

export interface BatchLoanRequestResult {
  itemIndex: number;
  requestedAmount: number;
  discountedAmount: number;
  tenureSeconds: number;
}

export interface CartAbandonmentStats {
  cartsCreated: number;
  cartsSubmitted: number;
  itemsAdded: number;
  itemsSubmitted: number;
}

const VOLUME_DISCOUNT_THRESHOLD = 3;
const VOLUME_DISCOUNT_BPS = 100; // 1%

/**
 * In-memory batch-loan-request "cart" (issue: cart system for batch loan
 * requests). Mirrors ExpenseStore's approach — this is borrower-submitted
 * staging state with no on-chain equivalent until submission, so it lives
 * here rather than in the indexer's read-only database. Submission itself
 * is expected to call through to the contract's
 * `submit_batch_loan_request` (src/loan_cart.rs) for each staged item; this
 * store only owns the pre-submission staging + funnel analytics.
 */
export class LoanCartStore {
  private readonly carts = new Map<string, LoanCart>();
  private stats: CartAbandonmentStats = {
    cartsCreated: 0,
    cartsSubmitted: 0,
    itemsAdded: 0,
    itemsSubmitted: 0,
  };

  addItem(borrower: string, amount: number, tenureSeconds: number): LoanCart {
    const now = Date.now();
    let cart = this.carts.get(borrower);
    if (!cart) {
      cart = {
        borrower,
        items: [],
        createdAt: now,
        lastUpdatedAt: now,
        submitted: false,
      };
      this.carts.set(borrower, cart);
      this.stats.cartsCreated += 1;
    }

    cart.items.push({ amount, tenureSeconds, addedAt: now });
    cart.lastUpdatedAt = now;
    cart.submitted = false;
    this.stats.itemsAdded += 1;
    return cart;
  }

  getCart(borrower: string): LoanCart | null {
    return this.carts.get(borrower) ?? null;
  }

  /** Applies the 1% volume discount for batches of 3+ staged loans. */
  private discountedAmount(amount: number, cartSize: number): number {
    if (cartSize >= VOLUME_DISCOUNT_THRESHOLD) {
      return amount - (amount * VOLUME_DISCOUNT_BPS) / 10_000;
    }
    return amount;
  }

  /**
   * Marks the cart submitted and returns the per-item discounted amounts
   * the caller should pass through to the on-chain
   * `submit_batch_loan_request` call. Clears staged items afterward.
   */
  submitBatch(borrower: string): BatchLoanRequestResult[] {
    const cart = this.carts.get(borrower);
    if (!cart || cart.items.length === 0) {
      return [];
    }

    const cartSize = cart.items.length;
    const results: BatchLoanRequestResult[] = cart.items.map((item, itemIndex) => ({
      itemIndex,
      requestedAmount: item.amount,
      discountedAmount: this.discountedAmount(item.amount, cartSize),
      tenureSeconds: item.tenureSeconds,
    }));

    this.stats.cartsSubmitted += 1;
    this.stats.itemsSubmitted += cartSize;

    cart.items = [];
    cart.submitted = true;
    cart.lastUpdatedAt = Date.now();

    return results;
  }

  /** Clears a cart without submitting it; counted as abandonment. */
  abandon(borrower: string): boolean {
    const cart = this.carts.get(borrower);
    if (!cart || cart.submitted || cart.items.length === 0) {
      this.carts.delete(borrower);
      return false;
    }
    this.carts.delete(borrower);
    return true;
  }

  /** Carts staged but neither submitted nor abandoned yet. */
  abandonedCartCount(): number {
    let count = 0;
    for (const cart of this.carts.values()) {
      if (!cart.submitted && cart.items.length > 0) {
        count += 1;
      }
    }
    return count;
  }

  getStats(): CartAbandonmentStats {
    return { ...this.stats };
  }
}

export const loanCartStore = new LoanCartStore();
