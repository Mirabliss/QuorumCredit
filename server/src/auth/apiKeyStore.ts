/**
 * Issue #1290: Provisioned API-key store with SHA-256 hashed key validation.
 *
 * Keys are stored as SHA-256 hex digests so the plaintext never lives in
 * memory or on disk.  The set of valid key hashes is loaded once from the
 * `API_KEY_HASHES` environment variable (comma-separated hex strings) and
 * supplemented by any keys added at runtime via `registerKey`.
 *
 * Usage:
 *   API_KEY_HASHES="sha256hex1,sha256hex2" node dist/index.js
 *
 * To hash a key for provisioning:
 *   node -e "const c=require('node:crypto');console.log(c.createHash('sha256').update('YOUR_KEY').digest('hex'))"
 */

import { createHash, timingSafeEqual } from "node:crypto";

/** SHA-256 hex digest of a raw API key. */
export type KeyHash = string;

export interface ApiKeyStore {
  /** Returns true when the provided raw key matches a provisioned entry. */
  isValid(rawKey: string): boolean;
  /** Register an additional hashed key at runtime (e.g. for tests). */
  registerHash(hash: KeyHash): void;
}

function sha256hex(input: string): string {
  return createHash("sha256").update(input, "utf8").digest("hex");
}

/** Constant-time hex comparison to avoid timing leaks. */
function safeHexEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  return timingSafeEqual(Buffer.from(a, "hex"), Buffer.from(b, "hex"));
}

/** Build the store from a comma-separated list of SHA-256 hex digests. */
export function buildApiKeyStore(hashList: string): ApiKeyStore {
  const hashes = new Set<string>(
    hashList
      .split(",")
      .map((h) => h.trim().toLowerCase())
      .filter((h) => h.length === 64) // only valid SHA-256 hex strings
  );

  return {
    isValid(rawKey: string): boolean {
      const digest = sha256hex(rawKey);
      for (const stored of hashes) {
        if (safeHexEqual(digest, stored)) return true;
      }
      return false;
    },
    registerHash(hash: KeyHash): void {
      const normalised = hash.trim().toLowerCase();
      if (normalised.length === 64) hashes.add(normalised);
    },
  };
}

/**
 * Load the default store from the environment.
 * Falls back to an empty store (all keys rejected) when the variable is absent.
 */
export function loadApiKeyStore(env: NodeJS.ProcessEnv = process.env): ApiKeyStore {
  return buildApiKeyStore(env.API_KEY_HASHES ?? "");
}
