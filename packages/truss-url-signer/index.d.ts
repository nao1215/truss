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

export type FitMode = "contain" | "cover" | "fill" | "inside";
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
export type OutputFormat =
  | "jpeg"
  | "png"
  | "webp"
  | "avif"
  | "bmp"
  | "tiff"
  | "svg";
export type OptimizeMode = "none" | "auto" | "lossless" | "lossy";
export type QuarterTurn = 0 | 90 | 180 | 270;
export type TargetQuality = `ssim:${number}` | `psnr:${number}`;
export type HmacSecret = string | ArrayBuffer | ArrayBufferView;

export interface TransformQuery {
  width?: number | undefined;
  height?: number | undefined;
  fit?: FitMode | undefined;
  position?: Position | undefined;
  format?: OutputFormat | undefined;
  quality?: number | undefined;
  optimize?: OptimizeMode | undefined;
  targetQuality?: TargetQuality | undefined;
  background?: string | undefined;
  rotate?: QuarterTurn | undefined;
  autoOrient?: boolean | undefined;
  stripMetadata?: boolean | undefined;
  preserveExif?: boolean | undefined;
  crop?: string | undefined;
  blur?: number | undefined;
  sharpen?: number | undefined;
}

export interface SignedWatermarkParams {
  url: string;
  position?: Position | undefined;
  opacity?: number | undefined;
  margin?: number | undefined;
}

export interface SignPublicUrlOptions {
  baseUrl: string | URL;
  source: SignedUrlSource;
  transforms?: TransformQuery | undefined;
  keyId: string;
  secret: HmacSecret;
  expires: number | bigint;
  watermark?: SignedWatermarkParams | undefined;
  preset?: string | undefined;
  method?: string | undefined;
}

export declare function signPublicUrl(options: SignPublicUrlOptions): string;
