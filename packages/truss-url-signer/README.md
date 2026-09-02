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

- `baseUrl`: externally visible `http` or `https` base URL for truss. A path is kept, so `https://images.example.com/img` signs a request for a deployment served under that prefix; the path takes no part in the signature.
- `source`: `{ kind: "path", path, version? }` or `{ kind: "url", url, version? }`
- `transforms`: public query parameters such as `width`, `height`, `fit`, `format`, `quality`, `optimize`, `targetQuality`, `crop`, `blur`, `sharpen`, `grayscale`, and `withoutEnlargement`
- `watermark`: optional `watermarkUrl` parameters
- `keyId`, `secret`, `expires`
- `method`: optional canonical HTTP method, default `GET`

The package omits transform fields that would resolve to truss defaults, matching the Rust implementation. For the public contract and compatibility policy, see the repository's [Signed URL Specification](https://github.com/nao1215/truss/blob/main/docs/signed-url-spec.md).
It also rejects request-invariant invalid combinations before signing, including `fit` / `position` without bounded resize, `quality` with `optimize=lossless`, invalid `targetQuality` matrices, invalid crop strings, and watermark opacity outside `1..=100`. An empty `keyId`, `secret`, or source is rejected for the same reason: a server will not start with an empty key id or secret, so a URL carrying one can never verify. A key none of the objects above reads is rejected on the same grounds, since a misspelled optional key would otherwise be dropped in silence and sign a URL for a transform nobody asked for: `transform` for `transforms` signs one with no transform in it, and `watermarks` for `watermark` signs one that serves the image without its overlay. A key whose value is `undefined` counts as absent, so `{...options, preset: undefined}` still signs.

## Compatibility

The package version tracks the truss release it ships with, so what a version number means is what the [crate documentation](https://docs.rs/truss-image) states. While the version is `0.x`, a minor release may change any of it.

From `1.0` on, the covered surface is the exported function names, the keys of the options object and of the `source`, `transforms`, and `watermark` objects it contains, and the values each key accepts. Adding a key is a minor release; removing or renaming one is a major release. Because an unknown key is refused rather than ignored, a key removed here stops signing rather than signing something else, which is the behaviour a caller can act on.

Not covered: the text of the errors thrown for an invalid option, which name the key for a person to read. The URL this package produces is governed by the [Signed URL Specification](https://github.com/nao1215/truss/blob/main/docs/signed-url-spec.md#compatibility-policy), whose promise is stronger and applies even before `1.0`.

## Runtime Notes

- This package targets Node.js because URL signing requires a secret and uses `node:crypto`.
- Generated URLs are compatible with `truss sign` and the server-side verifier.

## Maintainer Note

This package now publishes via npm trusted publishing on tagged GitHub releases.

## License

MIT
