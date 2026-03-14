# WASM Integration

This guide covers using `truss` as a browser-oriented WebAssembly module instead of as a CLI or HTTP server.

## What The WASM Adapter Is

The WASM adapter exposes the shared Rust image pipeline through a small JavaScript-facing surface. It is designed for local, client-side transforms:

- Input is raw image bytes from the browser.
- Output is transformed bytes plus JSON metadata.
- No server-side fetch, storage backend, signed URL, or secret-backed auth path is involved.

If you need remote URL fetches, storage backends, signed URLs, or server-side enforcement, use the HTTP server instead.

## Build Modes

### Official npm package build

The repository now includes the source for the official npm package at [`packages/truss-wasm`](../packages/truss-wasm). That package is intended for publication as `@nao1215/truss-wasm`.

Its official build uses:

- `wasm`
- `svg`
- `avif`
- `wasm-bindgen --target bundler`

Implication:

- bundler-based consumers can import the package directly
- the package does not require an explicit `init()` call
- AVIF decode/encode is enabled
- WebP output stays lossless in the official package

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

For bundler-based apps, the official package build is the recommended default. The raw `wasm-bindgen` output pair remains useful for static-page or custom hosting flows.

The examples in this guide assume a static page that lives next to the generated `pkg/` directory, for example `web/dist/index.html` importing `./pkg/truss.js`. If your app serves the generated files from another asset root, adjust the import path and `wasm-bindgen --out-dir` accordingly.

## npm Package Quick Start

For bundler-based browser apps, use the official package:

```ts
import {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
} from "@nao1215/truss-wasm";

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
```

The official package is generated with `wasm-bindgen --target bundler`, so there is no explicit `init()` step.

For Vite, add [`vite-plugin-wasm`](https://github.com/Menci/vite-plugin-wasm) and [`vite-plugin-top-level-await`](https://github.com/Menci/vite-plugin-top-level-await), as shown in [`examples/vite-truss-wasm/vite.config.js`](../examples/vite-truss-wasm/vite.config.js).

For a runnable browser consumer example, see [`examples/vite-truss-wasm`](../examples/vite-truss-wasm).

For a local install-and-transform smoke check that exercises the packed npm artifact from a throwaway consumer, run:

```sh
node ./scripts/run-wasm-consumer-smoke.mjs
```

To verify that the Vite example still bundles correctly against the current repository checkout, run:

```sh
node ./scripts/run-wasm-vite-example-smoke.mjs
```

## JavaScript Quick Start

For direct static hosting of the raw Wasm bindings, the generated package exports a default `init` function plus named helpers:

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

This example assumes a main-thread browser page with `File`, `Blob`, and object URL APIs available. The low-level WASM exports themselves only require byte arrays and strings.

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
  position?: "center" | "top" | "right" | "bottom" | "left" | "top-left" | "top-right" | "bottom-left" | "bottom-right";
  opacity?: number;
  margin?: number;
};
```

Defaults:

- `position`: `bottom-right`
- `opacity`: `50`
- `margin`: `10`

Validation:

- `opacity` must be between `1` and `100`.
- `margin` is a non-negative integer number of pixels.

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

- `width` and `height` must be greater than zero when provided.
- `fit` and `position` require both `width` and `height`.
- `format: "svg"` is only valid when the input is already SVG.
- `quality` must be between `1` and `100`, and only applies to lossy output formats.
- `quality` cannot be combined with `optimize: "lossless"`.
- `targetQuality` accepts values such as `ssim:0.98` or `psnr:42`.
- `targetQuality` requires `optimize: "auto"` or `optimize: "lossy"`.
- `ssim:*` targets must be greater than `0.0` and at most `1.0`.
- `psnr:*` targets must be greater than `0`.
- `background` is `RRGGBB` or `RRGGBBAA`.
- `crop` is `x,y,width,height`.
- Crop width and height must be greater than zero.
- `blur` and `sharpen` must each be between `0.1` and `100.0`.
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

Frontend note:

- The documented `kind` values come from `truss` itself.
- Your app may still see unrelated runtime exceptions from its own JS glue or browser APIs. The demo UI treats those as a separate fallback category.

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
- The examples in this repository assume a browser page with `File`, `Blob`, and `URL.createObjectURL`.

Not currently documented or tested as first-class integration targets:

- SSR environments
- Node.js-only runtimes
- Web Workers
- Managed or legacy WebViews

The raw exported functions accept `Uint8Array` and strings, so worker-style integrations are plausible, but this repository does not currently document or test them as a supported path.

## Packaging Notes

- The generated package targets `--target web`, so it expects an ES module environment.
- Ship the generated JS loader and `.wasm` file together.
- If you host the files yourself, keep the import path to `truss.js` stable relative to the emitted `.wasm` asset.

For the demo-specific build flow, see the [Development Guide](development.md).
