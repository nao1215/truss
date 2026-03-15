/**
 * Image source for a signed URL — either a storage path or a remote URL.
 *
 * - `kind: "path"` — references a file on truss storage by its key.
 * - `kind: "url"` — references an external image by HTTP(S) URL.
 *
 * An optional `version` string can be appended to bust caches when the
 * underlying asset changes without changing its path/URL.
 */
export type SignedUrlSource =
  | {
      kind: "path";
      path: string;
      version?: string | undefined;
    }
  | {
      kind: "url";
      url: string;
      version?: string | undefined;
    };

/** Resize fitting strategy when both width and height are specified. */
export type FitMode = "contain" | "cover" | "fill" | "inside";

/** Gravity / anchor position for crop and resize operations. */
export type Position =
  | "center"
  | "top"
  | "right"
  | "bottom"
  | "left"
  | "top-left"
  | "top-right"
  | "bottom-left"
  | "bottom-right";

/** Supported output image formats. */
export type OutputFormat =
  | "jpeg"
  | "png"
  | "webp"
  | "avif"
  | "bmp"
  | "tiff"
  | "svg";

/** Optimization strategy applied after encoding. */
export type OptimizeMode = "none" | "auto" | "lossless" | "lossy";

/** Rotation angle in 90-degree increments. */
export type QuarterTurn = 0 | 90 | 180 | 270;

/**
 * Target quality metric for adaptive encoding.
 * Format: `"ssim:<threshold>"` or `"psnr:<threshold>"`.
 */
export type TargetQuality = `ssim:${number}` | `psnr:${number}`;

/**
 * HMAC secret for URL signing.
 * Accepts a UTF-8 string, an `ArrayBuffer`, or a typed array view.
 */
export type HmacSecret = string | ArrayBuffer | ArrayBufferView;

/**
 * Image transform parameters included in the signed URL query string.
 *
 * All fields are optional — omitted fields use the server's defaults.
 * Some combinations are validated (e.g. `fit` requires both `width` and
 * `height`; `quality` is only valid for lossy formats).
 */
export interface TransformQuery {
  /** Output width in pixels (1–8192). */
  width?: number | undefined;
  /** Output height in pixels (1–8192). */
  height?: number | undefined;
  /** Resize fitting strategy. Requires both `width` and `height`. */
  fit?: FitMode | undefined;
  /** Crop / resize anchor position. Requires both `width` and `height`. */
  position?: Position | undefined;
  /** Output image format. */
  format?: OutputFormat | undefined;
  /** Encoding quality (1–100). Only applies to lossy formats (jpeg, webp, avif). */
  quality?: number | undefined;
  /** Post-encoding optimization strategy. */
  optimize?: OptimizeMode | undefined;
  /** Adaptive quality target metric. */
  targetQuality?: TargetQuality | undefined;
  /** Background colour (CSS hex, e.g. `"#ff0000"`) for transparent-to-opaque conversions. */
  background?: string | undefined;
  /** Rotation in 90-degree increments. */
  rotate?: QuarterTurn | undefined;
  /** Auto-orient based on EXIF data. */
  autoOrient?: boolean | undefined;
  /** Strip all metadata from the output image. */
  stripMetadata?: boolean | undefined;
  /** Preserve EXIF metadata in the output image. */
  preserveExif?: boolean | undefined;
  /** Crop region as `"x,y,w,h"`. */
  crop?: string | undefined;
  /** Gaussian blur sigma. */
  blur?: number | undefined;
  /** Sharpening sigma. */
  sharpen?: number | undefined;
}

/**
 * Watermark overlay parameters for signed URLs.
 */
export interface SignedWatermarkParams {
  /** HTTP(S) URL of the watermark image. */
  url: string;
  /** Watermark anchor position (default: `"bottom-right"`). */
  position?: Position | undefined;
  /** Watermark opacity (0–1, default: 1). */
  opacity?: number | undefined;
  /** Margin in pixels from the anchor edge. */
  margin?: number | undefined;
}

/**
 * Options for {@link signPublicUrl}.
 */
export interface SignPublicUrlOptions {
  /** Base URL of the truss server (e.g. `"https://images.example.com"`). */
  baseUrl: string | URL;
  /** Image source — a storage path or a remote URL. */
  source: SignedUrlSource;
  /** Optional image transforms to apply. */
  transforms?: TransformQuery | undefined;
  /** Signing key identifier (must match a key configured on the server). */
  keyId: string;
  /** HMAC shared secret corresponding to `keyId`. */
  secret: HmacSecret;
  /** Unix timestamp (seconds) at which the signed URL expires. */
  expires: number | bigint;
  /** Optional watermark overlay. */
  watermark?: SignedWatermarkParams | undefined;
  /** Named transform preset defined on the server. */
  preset?: string | undefined;
  /** HTTP method the signed URL is valid for (default: `"GET"`). */
  method?: string | undefined;
}

/**
 * Generate an HMAC-signed truss URL for public image delivery.
 *
 * The returned URL includes the image source, transforms, expiry timestamp,
 * and a cryptographic signature that the truss server will verify.
 *
 * @param options - Signing parameters.
 * @returns A fully-qualified signed URL string.
 * @throws {TypeError} If any parameter fails validation (e.g. missing
 *   required fields, incompatible transform combinations).
 */
export declare function signPublicUrl(options: SignPublicUrlOptions): string;
