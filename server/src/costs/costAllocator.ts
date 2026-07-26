import type { IndexedEvent } from "../types.js";

/**
 * Feature buckets costs get allocated to (issue #1227). Mapped from the
 * indexer's event `category` (see tools/indexer/src/indexer.rs::decode_event):
 * loan events are lending activity, vouch events are vouching activity, admin
 * events (pause/config/admin-set changes) are governance activity. `contract`
 * category events (deploy/upgrade/health) and anything undecoded fall into
 * `other` — they're real activity but don't belong to a single product feature.
 */
export type FeatureKey = "lending" | "vouching" | "governance" | "other";

const FEATURE_KEYS: FeatureKey[] = ["lending", "vouching", "governance", "other"];

const CATEGORY_TO_FEATURE: Record<string, FeatureKey> = {
  loan: "lending",
  vouch: "vouching",
  admin: "governance",
};

export function featureForCategory(category: string): FeatureKey {
  return CATEGORY_TO_FEATURE[category] ?? "other";
}

export interface CostAllocatorConfig {
  /** Estimated Soroban resource-fee cost per on-chain transaction, in stroops.
   * Soroban fees vary with resource usage; this is a flat operator-supplied estimate,
   * not derived from actual per-tx fee data (the indexer doesn't decode fee-charged
   * amounts today — see tools/indexer/src/indexer.rs). */
  contractFeeStroopsPerTx: number;
  /** Total API server hosting cost for the current billing month, in USD cents. */
  apiServerMonthlyCostCents: number;
  /** Total data storage cost (indexer DB + backup storage) for the current billing
   * month, in USD cents. */
  storageMonthlyCostCents: number;
  /** Optional stroops→USD-cents conversion rate, so contract fees can be blended into
   * totalCostCents alongside the off-chain infra costs above. Left undefined by
   * default: without an oracle, fabricating an exchange rate would be worse than
   * reporting contract fees natively in stroops and excluding them from the blended
   * total (see CostReport.contractFeeStroopsUnconverted). */
  stroopsToCentsRate?: number;
}

export interface FeatureCost {
  feature: FeatureKey;
  transactionCount: number;
  transactionShare: number;
  contractFeeStroops: number;
  apiServerCostCents: number;
  storageCostCents: number;
  /** apiServerCostCents + storageCostCents + (contract fee, only if stroopsToCentsRate configured) */
  totalCostCents: number;
  costPerTransactionCents: number;
}

export interface CostReport {
  generatedAt: number;
  /** Month key (YYYY-MM) this report's counts are drawn from, or "all-time" for a
   * cumulative snapshot. */
  period: string;
  totalTransactionCount: number;
  totalCostCents: number;
  /** True only when stroopsToCentsRate was configured — otherwise contract fees are
   * tracked in contractFeeStroops per feature but not folded into totalCostCents. */
  contractFeesIncludedInTotal: boolean;
  features: FeatureCost[];
  optimizationHints: string[];
}

function monthKeyOf(timestampMs: number): string {
  const d = new Date(timestampMs);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}`;
}

/**
 * Activity-based cost allocator (issue #1227): allocates the API-server and
 * storage cost inputs across features in proportion to each feature's share of
 * on-chain transaction volume. This is a simple, defensible allocation
 * methodology — genuine per-request cost tracing isn't wired up anywhere in
 * this service — documented here rather than left implicit.
 */
export class CostAllocator {
  private readonly config: CostAllocatorConfig;
  /** feature -> month key -> transaction count */
  private readonly countsByMonth = new Map<FeatureKey, Map<string, number>>();

  constructor(config: CostAllocatorConfig) {
    this.config = config;
    for (const feature of FEATURE_KEYS) {
      this.countsByMonth.set(feature, new Map());
    }
  }

  /** Call once per indexed on-chain event, e.g. from Bridge.tick alongside MetricsAggregator. */
  recordEvent(event: IndexedEvent): void {
    const feature = featureForCategory(event.category);
    const timestampMs = Date.parse(event.ledgerClosedAt) || Date.now();
    const key = monthKeyOf(timestampMs);
    const monthCounts = this.countsByMonth.get(feature)!;
    monthCounts.set(key, (monthCounts.get(key) ?? 0) + 1);
  }

  /** All month keys with at least one recorded transaction, oldest first. */
  recordedMonths(): string[] {
    const months = new Set<string>();
    for (const monthCounts of this.countsByMonth.values()) {
      for (const key of monthCounts.keys()) months.add(key);
    }
    return [...months].sort();
  }

  /** Cumulative (life-of-service) snapshot, allocated using the currently configured
   * monthly cost inputs. Suitable for a live dashboard, not a monthly report. */
  currentReport(): CostReport {
    const totals = new Map<FeatureKey, number>();
    for (const [feature, monthCounts] of this.countsByMonth) {
      let sum = 0;
      for (const count of monthCounts.values()) sum += count;
      totals.set(feature, sum);
    }
    return this.buildReport("all-time", totals);
  }

  /** Report for a single billing month (YYYY-MM), allocated using that same month's
   * counts against the configured monthly cost inputs. */
  monthlyReport(monthKey: string): CostReport {
    const totals = new Map<FeatureKey, number>();
    for (const [feature, monthCounts] of this.countsByMonth) {
      totals.set(feature, monthCounts.get(monthKey) ?? 0);
    }
    return this.buildReport(monthKey, totals);
  }

  /** One report per recorded month — "Generate monthly cost reports" (issue #1227). */
  generateMonthlyReports(): CostReport[] {
    return this.recordedMonths().map((month) => this.monthlyReport(month));
  }

  private buildReport(period: string, totalsByFeature: Map<FeatureKey, number>): CostReport {
    const totalTransactionCount = [...totalsByFeature.values()].reduce((a, b) => a + b, 0);
    const { contractFeeStroopsPerTx, apiServerMonthlyCostCents, storageMonthlyCostCents, stroopsToCentsRate } =
      this.config;

    const features: FeatureCost[] = FEATURE_KEYS.map((feature) => {
      const transactionCount = totalsByFeature.get(feature) ?? 0;
      const transactionShare = totalTransactionCount > 0 ? transactionCount / totalTransactionCount : 0;
      const contractFeeStroops = transactionCount * contractFeeStroopsPerTx;
      const apiServerCostCents = transactionShare * apiServerMonthlyCostCents;
      const storageCostCents = transactionShare * storageMonthlyCostCents;
      const contractFeeCents = stroopsToCentsRate ? contractFeeStroops * stroopsToCentsRate : 0;
      const totalCostCents = apiServerCostCents + storageCostCents + contractFeeCents;

      return {
        feature,
        transactionCount,
        transactionShare,
        contractFeeStroops,
        apiServerCostCents,
        storageCostCents,
        totalCostCents,
        costPerTransactionCents: transactionCount > 0 ? totalCostCents / transactionCount : 0,
      };
    });

    const totalCostCents = features.reduce((a, f) => a + f.totalCostCents, 0);

    return {
      generatedAt: Date.now(),
      period,
      totalTransactionCount,
      totalCostCents,
      contractFeesIncludedInTotal: Boolean(stroopsToCentsRate),
      features,
      optimizationHints: buildOptimizationHints(features, totalTransactionCount),
    };
  }
}

/** Flags features whose per-transaction cost or infra allocation looks out of line —
 * a starting point for investment decisions, not a verdict (issue #1227). */
function buildOptimizationHints(features: FeatureCost[], totalTransactionCount: number): string[] {
  const hints: string[] = [];
  const active = features.filter((f) => f.transactionCount > 0);
  if (active.length === 0) return hints;

  const avgCostPerTx = active.reduce((a, f) => a + f.costPerTransactionCents, 0) / active.length;

  for (const f of features) {
    if (f.transactionCount === 0 && f.totalCostCents > 0) {
      hints.push(
        `${f.feature}: allocated $${(f.totalCostCents / 100).toFixed(2)} of infra cost with zero recorded ` +
          `transactions this period — check for stranded capacity.`
      );
      continue;
    }
    if (avgCostPerTx > 0 && f.costPerTransactionCents > avgCostPerTx * 2) {
      hints.push(
        `${f.feature}: cost per transaction ($${(f.costPerTransactionCents / 100).toFixed(4)}) is ` +
          `${(f.costPerTransactionCents / avgCostPerTx).toFixed(1)}x the cross-feature average — investigate ` +
          `whether volume is unusually low or the feature is disproportionately resource-heavy.`
      );
    }
  }

  if (totalTransactionCount === 0) {
    hints.push("No on-chain transactions recorded for this period — infra cost is entirely fixed overhead right now.");
  }

  return hints;
}
