import type {
  InsuranceProvider,
  InsuranceProduct,
  InsuranceQuote,
  InsuranceClaim,
  InsuranceMarketplaceStats,
} from "./types.js";

/**
 * Issue #1174: Insurance Marketplace Module
 * Aggregates insurance products from multiple third-party providers
 * and provides a unified interface for comparing and purchasing coverage.
 */

// In-memory storage for insurance data (in production, use a database)
const providers: Map<string, InsuranceProvider> = new Map();
const products: Map<string, InsuranceProduct> = new Map();
const quotes: Map<string, InsuranceQuote> = new Map();
const claims: Map<string, InsuranceClaim> = new Map();

// Performance tracking
const stats: InsuranceMarketplaceStats = {
  totalProviders: 0,
  activeProducts: 0,
  quotesGenerated: 0,
  claimsSubmitted: 0,
  claimsApproved: 0,
  totalCoveragePremiums: 0,
  totalClaimsPaid: 0,
  averagePremiumBps: 0,
  productPerformance: [],
  timestamp: Date.now(),
};

/**
 * Register a new insurance provider
 */
export function registerProvider(provider: InsuranceProvider): void {
  providers.set(provider.id, provider);
  stats.totalProviders = providers.size;
  stats.timestamp = Date.now();
}

/**
 * Add an insurance product from a provider
 */
export function addProduct(product: InsuranceProduct): void {
  products.set(product.id, product);
  stats.activeProducts = Array.from(products.values()).filter((p) => p.active).length;
  stats.timestamp = Date.now();
}

/**
 * Get all active insurance providers
 */
export function getActiveProviders(): InsuranceProvider[] {
  return Array.from(providers.values()).filter((p) => p.active);
}

/**
 * Get all active insurance products
 */
export function getActiveProducts(): InsuranceProduct[] {
  return Array.from(products.values()).filter((p) => p.active);
}

/**
 * Generate insurance quotes for a loan
 * Fetches quotes from all active providers and returns sorted by premium
 */
export async function generateInsuranceQuotes(
  loanId: number,
  borrower: string,
  loanAmount: number,
  token: string
): Promise<InsuranceQuote[]> {
  const generatedQuotes: InsuranceQuote[] = [];

  // Filter providers that support this token
  const activeProviders = getActiveProviders().filter((p) =>
    p.supportedTokens.includes(token) &&
    loanAmount >= p.minLoanAmount &&
    loanAmount <= p.maxLoanAmount
  );

  for (const provider of activeProviders) {
    // Get products from this provider
    const providerProducts = Array.from(products.values()).filter(
      (p) => p.providerId === provider.id && p.active
    );

    for (const product of providerProducts) {
      try {
        // Call third-party API or use static rates
        const quote = await fetchProviderQuote(
          provider,
          product,
          loanId,
          borrower,
          loanAmount
        );

        if (quote) {
          generatedQuotes.push(quote);
        }
      } catch (error) {
        console.error(`Failed to fetch quote from ${provider.name}:`, error);
      }
    }
  }

  // Update stats
  stats.quotesGenerated += generatedQuotes.length;
  stats.timestamp = Date.now();

  // Sort by premium (lowest first)
  return generatedQuotes.sort((a, b) => a.premiumAmount - b.premiumAmount);
}

/**
 * Fetch a quote from a specific third-party insurance provider
 */
async function fetchProviderQuote(
  provider: InsuranceProvider,
  product: InsuranceProduct,
  loanId: number,
  borrower: string,
  loanAmount: number
): Promise<InsuranceQuote | null> {
  try {
    // In production, call actual third-party API
    // For now, calculate based on static product rates
    const coverageAmount = Math.floor((loanAmount * product.coveragePercentage) / 100);
    const premiumAmount = Math.floor((loanAmount * product.premiumBps) / 10000);
    const premiumBpsAnnual = product.premiumBps;
    const quoteId = `quote_${Date.now()}_${Math.random().toString(36).substring(7)}`;
    const expiresAt = Date.now() + 24 * 60 * 60 * 1000; // 24 hour quote validity

    const quote: InsuranceQuote = {
      id: quoteId,
      loanId,
      borrower,
      loanAmount,
      productId: product.id,
      providerId: provider.id,
      providerName: provider.name,
      productName: product.name,
      coverageAmount,
      premiumAmount,
      premiumBpsAnnual,
      expiresAt,
      quotedAt: Date.now(),
    };

    // Store quote
    quotes.set(quoteId, quote);

    return quote;
  } catch (error) {
    console.error(`Error fetching quote from provider ${provider.id}:`, error);
    return null;
  }
}

/**
 * Get a specific quote by ID
 */
export function getQuote(quoteId: string): InsuranceQuote | undefined {
  return quotes.get(quoteId);
}

/**
 * Get all quotes for a loan
 */
export function getQuotesForLoan(loanId: number): InsuranceQuote[] {
  return Array.from(quotes.values()).filter((q) => q.loanId === loanId);
}

/**
 * Submit an insurance claim
 */
export function submitClaim(
  loanId: number,
  borrower: string,
  productId: string,
  claimAmount: number
): InsuranceClaim {
  const product = products.get(productId);
  if (!product) {
    throw new Error("Product not found");
  }

  const claimId = `claim_${Date.now()}_${Math.random().toString(36).substring(7)}`;

  // Validate claim amount doesn't exceed max coverage
  if (claimAmount > product.maxClaimAmount) {
    throw new Error("Claim amount exceeds maximum coverage");
  }

  const claim: InsuranceClaim = {
    id: claimId,
    loanId,
    borrower,
    providerId: product.providerId,
    productId,
    claimAmount,
    status: "pending",
    submittedAt: Date.now(),
  };

  claims.set(claimId, claim);
  stats.claimsSubmitted += 1;
  stats.timestamp = Date.now();

  return claim;
}

/**
 * Approve an insurance claim (called by provider/admin)
 */
export function approveClaim(claimId: string): InsuranceClaim {
  const claim = claims.get(claimId);
  if (!claim) {
    throw new Error("Claim not found");
  }

  claim.status = "approved";
  claim.decidedAt = Date.now();
  claims.set(claimId, claim);

  stats.claimsApproved += 1;
  stats.totalClaimsPaid += claim.claimAmount;
  stats.timestamp = Date.now();

  return claim;
}

/**
 * Reject an insurance claim
 */
export function rejectClaim(claimId: string, reason: string): InsuranceClaim {
  const claim = claims.get(claimId);
  if (!claim) {
    throw new Error("Claim not found");
  }

  claim.status = "rejected";
  claim.decidedAt = Date.now();
  claim.rejectionReason = reason;
  claims.set(claimId, claim);

  stats.timestamp = Date.now();

  return claim;
}

/**
 * Mark a claim as paid
 */
export function markClaimAsPaid(claimId: string): InsuranceClaim {
  const claim = claims.get(claimId);
  if (!claim) {
    throw new Error("Claim not found");
  }

  if (claim.status !== "approved") {
    throw new Error("Only approved claims can be paid");
  }

  claim.status = "paid";
  claim.paidAt = Date.now();
  claims.set(claimId, claim);

  stats.timestamp = Date.now();

  return claim;
}

/**
 * Get claim by ID
 */
export function getClaim(claimId: string): InsuranceClaim | undefined {
  return claims.get(claimId);
}

/**
 * Get all claims for a borrower
 */
export function getClaimsForBorrower(borrower: string): InsuranceClaim[] {
  return Array.from(claims.values()).filter((c) => c.borrower === borrower);
}

/**
 * Get insurance marketplace statistics
 */
export function getMarketplaceStats(): InsuranceMarketplaceStats {
  // Calculate average premium
  const activeQuotes = Array.from(quotes.values()).filter(
    (q) => q.expiresAt > Date.now()
  );
  const avgPremium = activeQuotes.length > 0
    ? activeQuotes.reduce((sum, q) => sum + q.premiumBpsAnnual, 0) / activeQuotes.length
    : 0;

  // Calculate product performance
  const productPerformance = Array.from(products.values())
    .filter((p) => p.active)
    .map((product) => {
      const productClaims = Array.from(claims.values()).filter(
        (c) => c.productId === product.id
      );
      const approvedClaims = productClaims.filter((c) => c.status === "approved");
      const approvalRate = productClaims.length > 0
        ? approvedClaims.length / productClaims.length
        : 0;

      const processingTimes = productClaims
        .filter((c) => c.decidedAt)
        .map((c) => ((c.decidedAt || 0) - c.submittedAt) / (24 * 60 * 60 * 1000));
      const avgProcessingDays = processingTimes.length > 0
        ? processingTimes.reduce((a, b) => a + b, 0) / processingTimes.length
        : 0;

      return {
        productId: product.id,
        claimsApprovalRate: Math.round(approvalRate * 100),
        averageClaimProcessingDays: Math.round(avgProcessingDays),
      };
    });

  return {
    ...stats,
    averagePremiumBps: Math.round(avgPremium),
    productPerformance,
    timestamp: Date.now(),
  };
}

/**
 * Initialize with default providers and products
 */
export function initializeDefaults(): void {
  // Provider 1: Basic Coverage Inc
  registerProvider({
    id: "provider_basic",
    name: "Basic Coverage Inc",
    apiEndpoint: "https://api.basiccoverage.com",
    active: true,
    supportedTokens: ["USDC", "XLM"],
    minLoanAmount: 100000,
    maxLoanAmount: 10000000000,
    createdAt: Date.now(),
    updatedAt: Date.now(),
  });

  // Provider 2: Premium Protection Ltd
  registerProvider({
    id: "provider_premium",
    name: "Premium Protection Ltd",
    apiEndpoint: "https://api.premiumprotection.com",
    active: true,
    supportedTokens: ["USDC", "XLM", "EUR"],
    minLoanAmount: 50000,
    maxLoanAmount: 50000000000,
    createdAt: Date.now(),
    updatedAt: Date.now(),
  });

  // Products
  addProduct({
    id: "product_basic_50",
    providerId: "provider_basic",
    name: "Basic 50% Coverage",
    description: "Covers 50% of loan amount with standard processing",
    coveragePercentage: 50,
    premiumBps: 150, // 1.5% premium
    maxClaimAmount: 500000000, // 50 XLM
    claimsProcessingDays: 7,
    active: true,
    createdAt: Date.now(),
  });

  addProduct({
    id: "product_basic_75",
    providerId: "provider_basic",
    name: "Basic 75% Coverage",
    description: "Covers 75% of loan amount with express processing",
    coveragePercentage: 75,
    premiumBps: 250, // 2.5% premium
    maxClaimAmount: 750000000, // 75 XLM
    claimsProcessingDays: 3,
    active: true,
    createdAt: Date.now(),
  });

  addProduct({
    id: "product_premium_100",
    providerId: "provider_premium",
    name: "Premium Full Coverage",
    description: "100% loan amount coverage with priority claims processing",
    coveragePercentage: 100,
    premiumBps: 400, // 4% premium
    maxClaimAmount: 1000000000, // 100 XLM
    claimsProcessingDays: 1,
    active: true,
    createdAt: Date.now(),
  });
}
