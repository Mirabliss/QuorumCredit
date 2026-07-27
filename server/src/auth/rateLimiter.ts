/**
 * Issue #1290: In-process per-IP rate limiter for failed authentication attempts.
 *
 * Tracks the number of failed `/api/auth/token` calls per source IP within a
 * rolling window.  Once the limit is exceeded the IP is blocked for the
 * remainder of that window, slowing credential-stuffing attacks.
 *
 * The store is intentionally in-process (no Redis dependency) — it is cleared
 * on restart, which is an acceptable trade-off for a single-instance server.
 * A multi-instance deployment should back this with a shared store (e.g. the
 * existing RedisBus) but that wiring is outside the scope of this fix.
 */

export interface RateLimitEntry {
  count: number;
  windowStart: number; // ms
}

export interface AuthRateLimiter {
  /** Record a failed attempt; returns true when the IP should now be blocked. */
  recordFailure(ip: string): boolean;
  /** Returns true when the IP is currently blocked. */
  isBlocked(ip: string): boolean;
  /** Remove all entries (useful in tests). */
  clear(): void;
}

export interface RateLimiterOptions {
  /** Max failed attempts before the IP is blocked. Default 5. */
  maxAttempts?: number;
  /** Window duration in milliseconds. Default 60_000 (1 minute). */
  windowMs?: number;
}

export function createAuthRateLimiter(opts: RateLimiterOptions = {}): AuthRateLimiter {
  const maxAttempts = opts.maxAttempts ?? 5;
  const windowMs = opts.windowMs ?? 60_000;
  const store = new Map<string, RateLimitEntry>();

  function getEntry(ip: string, now: number): RateLimitEntry {
    const existing = store.get(ip);
    if (!existing || now - existing.windowStart >= windowMs) {
      const fresh: RateLimitEntry = { count: 0, windowStart: now };
      store.set(ip, fresh);
      return fresh;
    }
    return existing;
  }

  return {
    recordFailure(ip: string): boolean {
      const now = Date.now();
      const entry = getEntry(ip, now);
      entry.count += 1;
      store.set(ip, entry);
      return entry.count >= maxAttempts;
    },

    isBlocked(ip: string): boolean {
      const now = Date.now();
      const existing = store.get(ip);
      if (!existing) return false;
      if (now - existing.windowStart >= windowMs) {
        store.delete(ip);
        return false;
      }
      return existing.count >= maxAttempts;
    },

    clear(): void {
      store.clear();
    },
  };
}

/** Default singleton rate limiter (5 failures per minute per IP). */
export const defaultAuthRateLimiter = createAuthRateLimiter();
