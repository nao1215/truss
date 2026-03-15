# Next.js + truss Example

Server-side image transformation with [truss](https://github.com/nao1215/truss) and Next.js App Router.

## What This Demonstrates

- **Server-side URL signing** with `@nao1215/truss-url-signer` — secrets never reach the browser
- **`<TrussImage>` Server Component** — generates signed URLs at render time with zero client-side JavaScript
- **API Route Handler** (`/api/truss`) — signs URLs on demand for client-side use cases
- **Docker Compose** for local development

## Architecture

```text
Browser ──GET──▶ Next.js (Server Component)
                   │  signPublicUrl() at render time
                   ▼
                 <img src="http://localhost:8080/images/by-path?...&signature=abc">
                   │
Browser ──GET──▶ truss server (verifies signature, transforms image)
```

## Quick Start

### With Docker Compose

```bash
# Start truss server (builds from source)
docker compose up -d

# Install dependencies and start Next.js dev server
cp .env.local.example .env.local
npm install
npm run dev
```

Open http://localhost:3000 to see transformed images served by truss at http://localhost:8080.

### Without Docker

1. Start a truss server with signed URL support:

```bash
TRUSS_STORAGE_ROOT=../../images \
TRUSS_SIGNING_KEYS='{"dev-key":"dev-secret-change-in-production"}' \
TRUSS_PUBLIC_BASE_URL=http://localhost:8080 \
truss serve
```

2. Configure and start Next.js:

```bash
cp .env.local.example .env.local
npm install
npm run dev
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `TRUSS_PUBLIC_BASE_URL` | truss server URL (as seen by the browser) | — |
| `TRUSS_KEY_ID` | Signing key ID (must match `TRUSS_SIGNING_KEYS` on the server) | — |
| `TRUSS_KEY_SECRET` | Signing secret (must match `TRUSS_SIGNING_KEYS` on the server) | — |
| `TRUSS_URL_TTL_SECONDS` | Signed URL lifetime in seconds | `3600` |

> **Security note:** These are server-side-only variables. Never prefix them with `NEXT_PUBLIC_`.

## `<TrussImage>` Component

A React Server Component that signs URLs at render time:

```tsx
import { TrussImage } from "@/components/TrussImage";

<TrussImage
  src="photos/hero.jpg"  // path on truss storage
  alt="Hero image"
  width={800}
  format="webp"
  quality={80}
  fit="cover"
  loading="lazy"
/>
```

### Props

| Prop | Type | Description |
|------|------|-------------|
| `src` | `string` | Path to image on truss storage (required) |
| `alt` | `string` | Alt text (required) |
| `width` | `number` | Output width in pixels |
| `height` | `number` | Output height in pixels |
| `format` | `"webp" \| "avif" \| "jpeg" \| "png" \| ...` | Output format |
| `quality` | `number` | Quality 1–100 (lossy formats) |
| `fit` | `"contain" \| "cover" \| "fill" \| "inside"` | Resize fit mode |

All standard `<img>` attributes (`loading`, `className`, etc.) are also accepted.

## API Route (Client-Side Signing)

For cases where you need signed URLs on the client (e.g., dynamic galleries):

```http
GET /api/truss?path=sample.jpg&width=400&format=webp
```

Response:

```json
{ "url": "http://localhost:8080/images/by-path?path=sample.jpg&width=400&format=webp&keyId=dev-key&expires=1700000000&signature=abc123" }
```

The route validates all parameters and restricts `path` to safe characters to prevent path traversal.

> **Production warning:** This route is unauthenticated. In production, protect it with session-based auth or restrict access to trusted origins. Without authentication, any caller can request a signed URL for any storage path.
