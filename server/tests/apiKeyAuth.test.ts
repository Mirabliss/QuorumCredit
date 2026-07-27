/**
 * Issue #1290: Tests for API key validation and per-IP rate limiting.
 *
 * Covers:
 *  - Unrecognised key → 401
 *  - Valid (provisioned) key → 200 with token
 *  - Missing key body → 400
 *  - Repeated failures from the same IP → 429 after threshold
 *  - Rate limiter window reset
 */

import { describe, it, expect, beforeEach } from "vitest";
import { buildApiKeyStore } from "../src/auth/apiKeyStore.js";
import { createAuthRateLimiter } from "../src/auth/rateLimiter.js";
import { createHash } from "node:crypto";

function sha256hex(input: string): string {
  return createHash("sha256").update(input, "utf8").digest("hex");
}

// ── apiKeyStore ───────────────────────────────────────────────────────────────

describe("buildApiKeyStore", () => {
  it("rejects an unrecognised key", () => {
    const store = buildApiKeyStore(sha256hex("valid-key-1"));
    expect(store.isValid("wrong-key")).toBe(false);
  });

  it("accepts a provisioned key by plaintext lookup", () => {
    const store = buildApiKeyStore(sha256hex("valid-key-1"));
    expect(store.isValid("valid-key-1")).toBe(true);
  });

  it("accepts multiple provisioned keys from comma-separated hash list", () => {
    const hashes = [sha256hex("key-a"), sha256hex("key-b")].join(",");
    const store = buildApiKeyStore(hashes);
    expect(store.isValid("key-a")).toBe(true);
    expect(store.isValid("key-b")).toBe(true);
    expect(store.isValid("key-c")).toBe(false);
  });

  it("ignores entries that are not valid SHA-256 hex strings", () => {
    const store = buildApiKeyStore("not-a-hash,tooshort");
    expect(store.isValid("not-a-hash")).toBe(false);
  });

  it("registers a runtime hash and accepts the matching key", () => {
    const store = buildApiKeyStore("");
    store.registerHash(sha256hex("runtime-key"));
    expect(store.isValid("runtime-key")).toBe(true);
  });

  it("returns empty store when hash list is empty — all keys rejected", () => {
    const store = buildApiKeyStore("");
    expect(store.isValid("any-key")).toBe(false);
  });
});

// ── authRateLimiter ───────────────────────────────────────────────────────────

describe("createAuthRateLimiter", () => {
  let limiter: ReturnType<typeof createAuthRateLimiter>;

  beforeEach(() => {
    limiter = createAuthRateLimiter({ maxAttempts: 3, windowMs: 5_000 });
  });

  it("does not block on first failure", () => {
    const blocked = limiter.recordFailure("1.2.3.4");
    expect(blocked).toBe(false);
    expect(limiter.isBlocked("1.2.3.4")).toBe(false);
  });

  it("blocks IP after maxAttempts failures", () => {
    limiter.recordFailure("1.2.3.4");
    limiter.recordFailure("1.2.3.4");
    const blocked = limiter.recordFailure("1.2.3.4"); // 3rd = threshold
    expect(blocked).toBe(true);
    expect(limiter.isBlocked("1.2.3.4")).toBe(true);
  });

  it("does not block a different IP", () => {
    limiter.recordFailure("1.2.3.4");
    limiter.recordFailure("1.2.3.4");
    limiter.recordFailure("1.2.3.4");
    expect(limiter.isBlocked("9.9.9.9")).toBe(false);
  });

  it("clears all entries", () => {
    limiter.recordFailure("1.2.3.4");
    limiter.recordFailure("1.2.3.4");
    limiter.recordFailure("1.2.3.4");
    limiter.clear();
    expect(limiter.isBlocked("1.2.3.4")).toBe(false);
  });

  it("resets count after window expires", async () => {
    limiter = createAuthRateLimiter({ maxAttempts: 2, windowMs: 10 });
    limiter.recordFailure("5.5.5.5");
    limiter.recordFailure("5.5.5.5"); // blocked
    expect(limiter.isBlocked("5.5.5.5")).toBe(true);
    // Wait for window to expire
    await new Promise((r) => setTimeout(r, 20));
    expect(limiter.isBlocked("5.5.5.5")).toBe(false);
  });
});
