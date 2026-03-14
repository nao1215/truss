# @nao1215/truss-wasm

Official bundler-ready Wasm package for `truss`.

This package exposes the browser-facing Wasm adapter from `truss` as a prebuilt npm package for third-party web applications.

## What This Package Includes

- official Wasm build generated from `truss`
- bundler-oriented output from `wasm-bindgen --target bundler`
- TypeScript definitions generated alongside the Wasm bindings
- a fixed feature set for reproducible third-party integration

Current official feature set:

- `wasm`
- `svg`
- `avif`

This package intentionally does **not** include `webp-lossy`. In browser builds, WebP output stays lossless in this package.

## Installation

```sh
npm install @nao1215/truss-wasm
```

## Quick Start

Unlike the raw `--target web` bindings, the bundler build does not require an explicit `init()` call.

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

const response = JSON.parse(result.responseJson);
const outputBlob = new Blob([result.bytes], {
  type: response.artifact.mimeType,
});
```

### Vite

Vite needs community Wasm handling for the `wasm-bindgen --target bundler` output. Use `vite-plugin-wasm` together with `vite-plugin-top-level-await`:

```sh
npm install @nao1215/truss-wasm
npm install -D vite-plugin-wasm vite-plugin-top-level-await
```

```ts
import { defineConfig } from "vite";
import topLevelAwait from "vite-plugin-top-level-await";
import wasm from "vite-plugin-wasm";

export default defineConfig({
  plugins: [wasm(), topLevelAwait()],
});
```

For a runnable example, see `examples/vite-truss-wasm` in the repository.

## Exported API

This package exports the generated Wasm bindings directly:

- `WasmTransformOutput`
- `getCapabilitiesJson()`
- `inspectImageJson(inputBytes, declaredMediaType?)`
- `transformImage(inputBytes, declaredMediaType?, optionsJson)`
- `transformImageWithWatermark(inputBytes, declaredMediaType?, optionsJson, watermarkBytes, watermarkOptionsJson)`

For the JSON payload shapes, limits, and runtime caveats, see the repository's [WASM Integration guide](https://github.com/nao1215/truss/blob/main/docs/wasm.md).

## Build From Source

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.114

cd packages/truss-wasm
npm run build
npm pack --dry-run
```

`npm run build` writes the generated Wasm bindings into `packages/truss-wasm/dist/`. `npm pack` triggers the `prepack` script, which runs the same build automatically, and `--dry-run` performs a packaging smoke check without creating the tarball.

## License

MIT
