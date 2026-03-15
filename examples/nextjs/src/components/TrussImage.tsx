import { signPublicUrl } from "@nao1215/truss-url-signer";
import type { OutputFormat, FitMode } from "@nao1215/truss-url-signer";
import { trussConfig } from "@/lib/truss";

interface TrussImageProps
  extends Omit<
    React.ImgHTMLAttributes<HTMLImageElement>,
    "src" | "width" | "height"
  > {
  /** Path to image on truss storage */
  src: string;
  /** Alt text */
  alt: string;
  /** Output width in pixels */
  width?: number;
  /** Output height in pixels */
  height?: number;
  /** Output format */
  format?: OutputFormat;
  /** Quality 1-100 (lossy formats only) */
  quality?: number;
  /** Fit mode when both width and height are set */
  fit?: FitMode;
}

/**
 * React Server Component that renders an image transformed by truss.
 * URL signing happens server-side at render time — the secret never reaches the browser.
 */
export function TrussImage({
  src,
  width,
  height,
  format,
  quality,
  fit,
  alt,
  ...imgProps
}: TrussImageProps) {
  const config = trussConfig();
  const signedUrl = signPublicUrl({
    baseUrl: config.publicBaseUrl,
    source: { kind: "path", path: src },
    transforms: { width, height, format, quality, fit },
    keyId: config.keyId,
    secret: config.secret,
    expires: Math.floor(Date.now() / 1000) + config.ttlSeconds,
  });

  return (
    // eslint-disable-next-line @next/next/no-img-element
    <img src={signedUrl} width={width} height={height} alt={alt} {...imgProps} />
  );
}
