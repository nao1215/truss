# @nao1215/truss-url-signer

Official Node.js / TypeScript signer for `truss` public image URLs.

This package generates HMAC-signed URLs for `GET /images/by-path` and `GET /images/by-url` using the same canonicalization rules as `truss sign` and the server-side verifier.

## Installation

```sh
npm install @nao1215/truss-url-signer
```

## Quick Start

```ts
import { signPublicUrl } from "@nao1215/truss-url-signer";

const signedUrl = signPublicUrl({
  baseUrl: "https://images.example.com",
  source: {
    kind: "path",
    path: "hero.jpg",
  },
  transforms: {
    width: 1200,
    format: "webp",
    optimize: "lossy",
    targetQuality: "ssim:0.98",
  },
  keyId: "public-demo",
  secret: process.env.TRUSS_SIGNING_SECRET ?? "",
  expires: Math.floor(Date.now() / 1000) + 300,
});
```

## Remote URL Example

```ts
import { signPublicUrl } from "@nao1215/truss-url-signer";

const signedUrl = signPublicUrl({
  baseUrl: "https://images.example.com",
  source: {
    kind: "url",
    url: "https://origin.example.com/photo.png",
    version: "v3",
  },
  transforms: {
    width: 800,
    height: 800,
    fit: "cover",
    format: "avif",
  },
  watermark: {
    url: "https://cdn.example.com/logo.png",
    position: "bottom-right",
    opacity: 50,
    margin: 16,
  },
  keyId: "public-demo",
  secret: process.env.TRUSS_SIGNING_SECRET ?? "",
  expires: 1900000000,
});
```

## API

The package exports one function:

- `signPublicUrl(options)` returns a fully qualified signed URL string

`options` supports:

- `baseUrl`: externally visible `http` or `https` origin for truss
- `source`: `{ kind: "path", path, version? }` or `{ kind: "url", url, version? }`
- `transforms`: public query parameters such as `width`, `height`, `fit`, `format`, `optimize`, `targetQuality`, `crop`, `blur`, and `sharpen`
- `watermark`: optional `watermarkUrl` parameters
- `keyId`, `secret`, `expires`
- `method`: optional canonical HTTP method, default `GET`

The package omits transform fields that would resolve to truss defaults, matching the Rust implementation. For the public contract and compatibility policy, see the repository's [Signed URL Specification](https://github.com/nao1215/truss/blob/main/docs/signed-url-spec.md).
It also rejects request-invariant invalid combinations before signing, including `fit` / `position` without bounded resize, `quality` with `optimize=lossless`, invalid `targetQuality` matrices, invalid crop strings, and watermark opacity outside `1..=100`.

## Runtime Notes

- This package targets Node.js because URL signing requires a secret and uses `node:crypto`.
- Generated URLs are compatible with `truss sign` and the server-side verifier.

## Maintainer Note

The first npm release for a new package must be done manually so the package name is registered on npm. After that, trusted publishing can be enabled for automated releases.

## License

MIT
