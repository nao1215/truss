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
    ttlSeconds: Number(process.env.TRUSS_URL_TTL_SECONDS ?? "3600"),
  };
}
