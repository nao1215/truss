import { type NextRequest, NextResponse } from "next/server";
import { signPublicUrl } from "@nao1215/truss-url-signer";
import type { OutputFormat, FitMode } from "@nao1215/truss-url-signer";
import { trussConfig } from "@/lib/truss";

const ALLOWED_FORMATS = new Set([
  "jpeg",
  "png",
  "webp",
  "avif",
  "bmp",
  "tiff",
  "svg",
]);
const ALLOWED_FITS = new Set(["contain", "cover", "fill", "inside"]);
const MAX_DIMENSION = 8192;
// Only allow word characters, forward slashes, hyphens, and dots (as extensions).
// Reject path traversal sequences.
const PATH_PATTERN = /^[\w][\w/\-]*(?:\.[\w]+)*$/;

// WARNING: This route is unauthenticated. In production, protect it with
// session-based auth or restrict it to trusted origins. Without auth, any
// caller can request a signed URL for any path on truss storage.
export function GET(request: NextRequest) {
  const params = request.nextUrl.searchParams;
  const path = params.get("path");

  if (!path) {
    return NextResponse.json({ error: "path is required" }, { status: 400 });
  }
  if (path.includes("..") || !PATH_PATTERN.test(path)) {
    return NextResponse.json({ error: "invalid path" }, { status: 400 });
  }

  const format = params.get("format") ?? undefined;
  if (format && !ALLOWED_FORMATS.has(format)) {
    return NextResponse.json({ error: "invalid format" }, { status: 400 });
  }

  const fit = params.get("fit") ?? undefined;
  if (fit && !ALLOWED_FITS.has(fit)) {
    return NextResponse.json({ error: "invalid fit" }, { status: 400 });
  }

  const width = params.has("width") ? Number(params.get("width")) : undefined;
  const height = params.has("height")
    ? Number(params.get("height"))
    : undefined;
  const quality = params.has("quality")
    ? Number(params.get("quality"))
    : undefined;

  if (
    width !== undefined &&
    (!Number.isInteger(width) || width < 1 || width > MAX_DIMENSION)
  ) {
    return NextResponse.json({ error: "invalid width" }, { status: 400 });
  }
  if (
    height !== undefined &&
    (!Number.isInteger(height) || height < 1 || height > MAX_DIMENSION)
  ) {
    return NextResponse.json({ error: "invalid height" }, { status: 400 });
  }
  if (
    quality !== undefined &&
    (!Number.isInteger(quality) || quality < 1 || quality > 100)
  ) {
    return NextResponse.json({ error: "invalid quality" }, { status: 400 });
  }

  const config = trussConfig();
  const url = signPublicUrl({
    baseUrl: config.publicBaseUrl,
    source: { kind: "path", path },
    transforms: {
      width,
      height,
      format: format as OutputFormat | undefined,
      quality,
      fit: fit as FitMode | undefined,
    },
    keyId: config.keyId,
    secret: config.secret,
    expires: Math.floor(Date.now() / 1000) + config.ttlSeconds,
  });

  return NextResponse.json({ url });
}
