# WASM Integration

This guide covers using `truss` as a browser-oriented WebAssembly module instead of as a CLI or HTTP server.

## What The WASM Adapter Is

The WASM adapter exposes the shared Rust image pipeline through a small JavaScript-facing surface. It is designed for local, client-side transforms:

- Input is raw image bytes from the browser.
- Output is transformed bytes plus JSON metadata.
- No server-side fetch, storage backend, signed URL, or secret-backed auth path is involved.

If you need remote URL fetches, storage backends, signed URLs, or server-side enforcement, use the HTTP server instead.

## Build Modes

### GitHub Pages demo build

The demo on GitHub Pages is built with the `wasm` and `svg` features only:

```sh
./scripts/build-wasm-demo.sh
```

That script currently runs:

```sh
cargo build \
  --release \
  --locked \
  --target wasm32-unknown-unknown \
  --lib \
  --no-default-features \
  --features "wasm,svg"
```

Implication:

- SVG processing is enabled.
- AVIF decode/encode is disabled.
- Lossy WebP encoding is disabled.

### Custom browser build

If your product needs AVIF or lossy WebP support, build your own artifact with the matching feature flags:

```sh
rustup target add wasm32-unknown-unknown
# Keep this version aligned with Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114

cargo build \
  --release \
  --locked \
  --target wasm32-unknown-unknown \
  --lib \
  --no-default-features \
  --features "wasm,svg,avif,webp-lossy"

wasm-bindgen \
  --target web \
  --out-dir web/dist/pkg \
  target/wasm32-unknown-unknown/release/truss.wasm
```

Feature flags relevant to browser builds:

| Feature | Effect |
|------|------|
| `wasm` | Enables the `wasm-bindgen` browser adapter |
| `svg` | Enables SVG sanitization, SVG input handling, and SVG output for SVG inputs |
| `avif` | Enables AVIF decode and encode |
| `webp-lossy` | Enables quality-controlled lossy WebP output |

`truss` does not currently ship an npm package. The intended distribution model is the `wasm-bindgen` output pair: generated JS loader plus `.wasm` binary.

## JavaScript Quick Start

The generated package exports a default `init` function plus named helpers:

```js
import init, {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
  transformImageWithWatermark,
} from "./pkg/truss.js";

await init();

const inputBytes = new Uint8Array(await file.arrayBuffer());
const capabilities = JSON.parse(getCapabilitiesJson());
const inspected = JSON.parse(inspectImageJson(inputBytes, undefined));

const result = transformImage(
  inputBytes,
  undefined,
  JSON.stringify({
    format: "jpeg",
    width: 1200,
    quality: 82,
    autoOrient: true,
  }),
);

const response = JSON.parse(result.responseJson);
const outputBlob = new Blob([result.bytes], {
  type: response.artifact.mimeType,
});
```

If you already know the input format, `declaredMediaType` may be one of `jpeg`, `png`, `webp`, `avif`, `bmp`, `tiff`, or `svg`. Pass `undefined` if you want `truss` to rely on byte sniffing alone.

## Runtime Capabilities

Browser builds can differ based on compile-time features. Always inspect capabilities at startup instead of assuming all formats are present.

```ts
type WasmCapabilities = {
  svg: boolean;
  webpLossy: boolean;
  avif: boolean;
};
```

| Field | Meaning |
|------|------|
| `svg` | SVG input/output processing is available |
| `webpLossy` | Quality-controlled lossy WebP output is available |
| `avif` | AVIF decode and encode are available |

The GitHub Pages demo uses `svg: true`, `webpLossy: false`, `avif: false`.

## Exported API

### `getCapabilitiesJson()`

Returns a JSON string with the `WasmCapabilities` shape shown above.

### `inspectImageJson(inputBytes, declaredMediaType?)`

Inspects image bytes and returns:

```ts
type WasmInspectResponse = {
  artifact: {
    mediaType: string;
    mimeType: string;
    width: number | null;
    height: number | null;
    frameCount: number;
    hasAlpha: boolean | null;
  };
};
```

This is useful for building your UI before running a transform.

### `transformImage(inputBytes, declaredMediaType?, optionsJson)`

Returns a `WasmTransformOutput` object:

```ts
type WasmTransformOutput = {
  bytes: Uint8Array;
  responseJson: string;
};
```

`responseJson` decodes to:

```ts
type WasmTransformResponse = {
  artifact: {
    mediaType: string;
    mimeType: string;
    width: number | null;
    height: number | null;
    frameCount: number;
    hasAlpha: boolean | null;
  };
  warnings: string[];
  suggestedExtension: string;
};
```

Use `artifact.mimeType` when creating a `Blob`, and `suggestedExtension` when generating a download filename.

### `transformImageWithWatermark(inputBytes, declaredMediaType?, optionsJson, watermarkBytes, watermarkOptionsJson)`

This matches `transformImage`, but overlays a raster watermark before encoding the output.

Watermark options:

```ts
type WasmWatermarkOptions = {
  position?: string;
  opacity?: number;
  margin?: number;
};
```

Defaults:

- `position`: `bottom-right`
- `opacity`: `50`
- `margin`: `10`

## Transform Options Contract

`optionsJson` must match this JSON shape:

```ts
type WasmTransformOptions = {
  width?: number;
  height?: number;
  fit?: "contain" | "cover" | "fill" | "inside";
  position?: "center" | "top" | "right" | "bottom" | "left" | "top-left" | "top-right" | "bottom-left" | "bottom-right";
  format?: "jpeg" | "png" | "webp" | "avif" | "bmp" | "tiff" | "svg";
  quality?: number;
  optimize?: "none" | "auto" | "lossless" | "lossy";
  targetQuality?: string;
  background?: string;
  rotate?: 0 | 90 | 180 | 270;
  autoOrient?: boolean;
  keepMetadata?: boolean;
  preserveExif?: boolean;
  crop?: string;
  blur?: number;
  sharpen?: number;
};
```

Notes:

- `targetQuality` accepts values such as `ssim:0.98` or `psnr:42`.
- `background` is `RRGGBB` or `RRGGBBAA`.
- `crop` is `x,y,width,height`.
- `keepMetadata` and `preserveExif` are mutually exclusive.
- `autoOrient` defaults to `true`.

Transform semantics are the same as the CLI pipeline described in the main [README](../README.md).

## Error Contract

On failure, the exported WASM functions throw a `JsValue` whose string form is a JSON payload:

```ts
type WasmErrorPayload = {
  kind:
    | "invalidInput"
    | "invalidOptions"
    | "unsupportedInputMediaType"
    | "unsupportedOutputMediaType"
    | "decodeFailed"
    | "encodeFailed"
    | "capabilityMissing"
    | "limitExceeded";
  message: string;
};
```

Typical cases:

| `kind` | Meaning |
|------|------|
| `invalidInput` | Declared type conflicts with detected bytes |
| `invalidOptions` | Options JSON is malformed or contains invalid values |
| `unsupportedInputMediaType` | Input bytes are not a supported image format |
| `unsupportedOutputMediaType` | Requested output format is impossible, such as raster to SVG |
| `decodeFailed` | The image is structurally invalid |
| `encodeFailed` | Output encoding failed |
| `capabilityMissing` | The build excluded a requested feature |
| `limitExceeded` | Input, output, or watermark size exceeded a safety limit |

## Browser-Specific Constraints And Caveats

- The WASM adapter accepts bytes only. It does not fetch remote URLs and does not use storage backends.
- Raster input cannot be converted into SVG output.
- Watermarks must be raster images. SVG watermark input is rejected.
- AVIF encode/decode requires the `avif` feature.
- Lossy WebP output requires the `webp-lossy` feature.
- Metadata retention is not implemented for AVIF output.
- Lossy WebP optimization cannot preserve metadata.
- The WASM adapter does not inject a transform deadline. Browser apps should own their own UX for cancellation, progress, and timeouts.
- A browser may fail to preview a valid transformed artifact even when the conversion succeeded. In that case the output bytes are still usable for download.

## Safety Limits

These limits come from the shared core and apply to browser builds too:

| Limit | Value |
|------|------:|
| Max decoded input pixels | `100000000` |
| Max output pixels | `67108864` |
| Max watermark pixels | `4000000` |

The demo UI adds one more browser-side check:

- Watermark uploads larger than 10 MB are rejected before calling into WASM.

If you build your own UI, that 10 MB byte-size check is optional. The shared Rust core still enforces pixel-based safety limits.

## Browser Compatibility

`truss` does not currently publish a formal browser version matrix for the WASM build.

Documented expectations today:

- JavaScript must be enabled.
- The runtime must support ES modules.
- The runtime must support WebAssembly.

If you need to support older or managed browser environments, validate them against your own generated artifact.

## Packaging Notes

- The generated package targets `--target web`, so it expects an ES module environment.
- Ship the generated JS loader and `.wasm` file together.
- If you host the files yourself, keep the import path to `truss.js` stable relative to the emitted `.wasm` asset.

For the demo-specific build flow, see the [Development Guide](development.md).
