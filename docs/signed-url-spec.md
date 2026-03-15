# Signed URL Specification

This document defines the public signed-URL contract for truss.

It is the normative reference for:

- the public URL shape for `GET /images/by-path` and `GET /images/by-url`
- canonicalization rules used by signature generation and verification
- compatibility expectations for SDK, CDN, and reverse-proxy integrations

For the field-level transform schema, see [OpenAPI](openapi.yaml). For deployment guidance, see the [API Reference](api-reference.md#cdn--reverse-proxy-integration).

## Scope

This specification covers the public image endpoints authenticated by HMAC-signed query strings:

- `GET /images/by-path`
- `GET /images/by-url`

The primary external contract is `GET`. truss also accepts `HEAD` on these routes, using the same canonicalization rules with `HEAD` as the HTTP method, but `truss sign` generates `GET` URLs.

Private Bearer-token endpoints such as `POST /images` and `POST /images:transform` are out of scope for this document.

## Contract Surface

The following parts of the signed-URL format are part of the compatibility contract:

- Endpoint paths: `/images/by-path` and `/images/by-url`
- Authentication parameters: `keyId`, `expires`, `signature`
- Source selector parameters:
  - `/images/by-path`: `path`, optional `version`
  - `/images/by-url`: `url`, optional `version`
- Public transform parameters:
  - `width`, `height`, `fit`, `position`, `format`, `quality`
  - `optimize`, `targetQuality`
  - `background`, `rotate`
  - `autoOrient`, `stripMetadata`, `preserveExif`
  - `crop`, `blur`, `sharpen`
  - `watermarkUrl`, `watermarkPosition`, `watermarkOpacity`, `watermarkMargin`
  - `preset`
- Signature algorithm: HMAC-SHA256 over the canonical UTF-8 request string
- Signature encoding: lowercase hexadecimal

The following rules are also part of the contract:

- Query parameter names are case-sensitive.
- Query parameters must not be repeated.
- Unsupported query parameters are rejected with `400 Bad Request`.
- Query parameter order on the wire is not significant. truss canonicalizes parameters before verification.

Deployment data is not part of the cross-version compatibility promise. That includes concrete `keyId` values, shared secrets, preset names, source paths, source URLs, and the meaning of an application-specific `version` token.

## Authentication Parameters

| Parameter | Required | Meaning |
|-----------|----------|---------|
| `keyId` | Yes | Selects the shared secret used for verification |
| `expires` | Yes | Expiration time as a Unix timestamp in seconds |
| `signature` | Yes | Lowercase hex-encoded HMAC-SHA256 signature |

Expiration is evaluated as `expires < now`. In other words, a request is still accepted during the exact second identified by `expires`, and is rejected once the current Unix time is greater than that value.

## Canonicalization Rules

truss verifies the HMAC over this canonical form:

```text
METHOD
AUTHORITY
REQUEST_PATH
CANONICAL_QUERY
```

### 1. `METHOD`

Use the uppercase HTTP method. For the primary public contract this is `GET`.

### 2. `AUTHORITY`

Use the externally visible authority in `host[:port]` form:

- If the server is configured with `TRUSS_PUBLIC_BASE_URL`, truss uses that URL's authority for verification.
- Otherwise truss uses the incoming `Host` header.

Do not include the scheme, path, query string, or fragment in the canonical authority.

Examples:

- `images.example.com`
- `images.example.com:8443`

### 3. `REQUEST_PATH`

Use the literal public endpoint path:

- `/images/by-path`
- `/images/by-url`

### 4. `CANONICAL_QUERY`

Build the canonical query string as follows:

1. Start from decoded query parameter names and values.
2. Exclude `signature`.
3. Reject duplicates. A parameter may appear at most once.
4. Sort the remaining parameters lexicographically by parameter name.
5. Serialize the sorted parameters using `application/x-www-form-urlencoded` rules.

Important consequences:

- Sign the decoded value set, not the raw query substring from an incoming URL.
- Spaces are encoded as `+`.
- Reserved bytes are percent-encoded.
- Because parameters are sorted during canonicalization, callers do not need to preserve insertion order.

### Canonical Query Example

For this logical parameter set:

```text
path=image.png
width=800
format=webp
keyId=public-demo
expires=1900000000
```

the canonical query becomes:

```text
expires=1900000000&format=webp&keyId=public-demo&path=image.png&width=800
```

The canonical string is therefore:

```text
GET
images.example.com
/images/by-path
expires=1900000000&format=webp&keyId=public-demo&path=image.png&width=800
```

The signature is the lowercase hex digest of:

```text
HMAC-SHA256(secret, canonical_string_utf8_bytes)
```

## End-to-End Example

### With `truss sign`

Start the server:

```sh
TRUSS_SIGNING_KEYS='{"public-demo":"secret-value"}' \
TRUSS_PUBLIC_BASE_URL=https://images.example.com \
truss serve --storage-root ./images
```

Generate a signed URL:

```sh
truss sign \
  --base-url https://images.example.com \
  --path image.png \
  --key-id public-demo \
  --secret secret-value \
  --expires 1900000000 \
  --width 800 \
  --format webp
```

Fetch it:

```sh
curl -o image.webp 'https://images.example.com/images/by-path?...&signature=...'
```

### For SDK Implementers

For Node.js / TypeScript applications, you can use the official package:

```sh
npm install @nao1215/truss-url-signer
```

See [`packages/truss-url-signer`](../packages/truss-url-signer) for the package README and API reference.
The official signer validates the same request-invariant option matrix as the Rust server for public URL inputs such as `fit`, `position`, `quality`, `targetQuality`, watermark opacity, and crop syntax.

If you are implementing the signer yourself in another language, the equivalent flow in TypeScript is:

```ts
import { createHmac } from "node:crypto";

const params = new URLSearchParams([
  ["path", "image.png"],
  ["width", "800"],
  ["format", "webp"],
  ["keyId", "public-demo"],
  ["expires", "1900000000"],
]);

const canonicalParams = new URLSearchParams(
  [...params.entries()]
    .filter(([name]) => name !== "signature")
    .sort(([a], [b]) => a.localeCompare(b)),
);

const canonical = [
  "GET",
  "images.example.com",
  "/images/by-path",
  canonicalParams.toString(),
].join("\n");

const signature = createHmac("sha256", "secret-value")
  .update(canonical, "utf8")
  .digest("hex");

canonicalParams.set("signature", signature);

const signedUrl =
  `https://images.example.com/images/by-path?${canonicalParams.toString()}`;
```

This example intentionally signs the decoded parameter values and lets `URLSearchParams` produce the canonical wire encoding.

## Reverse Proxy and CDN Notes

For signed public traffic behind CloudFront, nginx, Envoy, or another proxy:

- Set `TRUSS_PUBLIC_BASE_URL` to the public origin such as `https://images.example.com`.
- Forward the full query string unchanged.
- Include all signed-URL query parameters in the cache key, or forward all query strings.
- If you rely on `Accept` negotiation by omitting `format`, also forward `Accept` and include it in the cache key because responses may vary on that header.
- If your CDN does not vary on `Accept`, prefer setting `format` explicitly or enable `TRUSS_DISABLE_ACCEPT_NEGOTIATION=true`.

## Compatibility Policy

truss treats the signed public URL format as a stable external contract even while the project is pre-1.0.

The following compatibility rules apply:

- Existing endpoint paths, parameter names, canonicalization rules, and HMAC algorithm will not change silently in patch or minor releases.
- Existing parameter meanings will not be repurposed under the same endpoint path.
- New optional query parameters may be added in future releases. Existing signed URLs that do not use them remain valid.
- If a breaking change is ever required, truss will introduce it through a documented migration path, such as a parallel endpoint, dual-format support during a transition window, or a clearly announced release-note break.
- Deprecations will be documented before removal. When practical, truss will accept both old and new forms during the deprecation window rather than invalidating existing signed URLs immediately.

For callers that need the most stable behavior across deployments, prefer:

- explicit `format` instead of `Accept` negotiation
- explicit transform parameters instead of deployment-defined `preset` names
- a configured `TRUSS_PUBLIC_BASE_URL` instead of relying on inbound proxy `Host` behavior
