export function trussConfig() {
  const publicBaseUrl = process.env.TRUSS_PUBLIC_BASE_URL;
  const keyId = process.env.TRUSS_KEY_ID;
  const secret = process.env.TRUSS_KEY_SECRET;

  if (!publicBaseUrl || !keyId || !secret) {
    throw new Error(
      "Missing truss configuration. Set TRUSS_PUBLIC_BASE_URL, TRUSS_KEY_ID, and TRUSS_KEY_SECRET environment variables.",
    );
  }

  return {
    publicBaseUrl,
    keyId,
    secret,
    ttlSeconds: parseTtl(process.env.TRUSS_URL_TTL_SECONDS),
  };
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
