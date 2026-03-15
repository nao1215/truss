export function trussConfig() {
  const publicBaseUrl = process.env.TRUSS_PUBLIC_BASE_URL;
  const keyId = process.env.TRUSS_KEY_ID;
  const secret = process.env.TRUSS_KEY_SECRET;

  if (!publicBaseUrl || !keyId || !secret) {
    throw new Error(
      "Missing truss configuration. Set TRUSS_PUBLIC_BASE_URL, TRUSS_KEY_ID, and TRUSS_KEY_SECRET environment variables.",
    );
  }

  const ttlSeconds = parseTtl(process.env.TRUSS_URL_TTL_SECONDS);
  return { publicBaseUrl, keyId, secret, ttlSeconds };
}

/**
 * Compute a stable `expires` timestamp that only changes once per TTL window.
 * All URLs generated within the same window share the same expiry, which lets
 * browsers and CDNs reuse cached responses instead of treating every render as
 * a cache-busting unique URL.
 */
export function stableExpires(ttlSeconds: number): number {
  const nowSec = Math.floor(Date.now() / 1000);
  const window = Math.ceil(nowSec / ttlSeconds) * ttlSeconds;
  return window + ttlSeconds;
}

const DEFAULT_TTL = 3600;
const MAX_TTL = 86400;

function parseTtl(value: string | undefined): number {
  if (!value) return DEFAULT_TTL;
  const n = Number(value);
  if (!Number.isInteger(n) || n < 1 || n > MAX_TTL) {
    throw new Error(
      `TRUSS_URL_TTL_SECONDS must be an integer between 1 and ${MAX_TTL}, got: ${value}`,
    );
  }
  return n;
}
