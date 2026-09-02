# @nao1215/truss-wasm

Official bundler-ready Wasm package for `truss`.

This package exposes the browser-facing Wasm adapter from `truss` as a prebuilt npm package for third-party web applications.

## What This Package Includes

- official Wasm build generated from `truss`
- browser-oriented bindings wrapped for npm consumers
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

This package initializes the Wasm module at import time, so consumer code does not call `init()` explicitly.

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

The package wrapper initializes with a top-level await, so the build target has to be one
where browsers support that natively. No plugin is needed:

```sh
npm install @nao1215/truss-wasm
```

```ts
import { defineConfig } from "vite";

export default defineConfig({
  build: {
    target: ["es2022", "edge89", "firefox89", "chrome89", "safari15"],
  },
});
```

Those are the first browser releases with native top-level await, and also the floor for
the `new URL(..., import.meta.url)` asset resolution the wrapper uses to locate the `.wasm`
binary. An older target needs a plugin such as `vite-plugin-top-level-await` to transform
the await.

Vite 8 is not supported yet. It builds with Rolldown, which emits the `.wasm` asset but
leaves no reference to it in the bundle, so the page compiles and then 404s at runtime.

For a runnable example, see `examples/vite-truss-wasm` in the repository.

## Exported API

This package exports the generated Wasm bindings directly:

- `WasmTransformOutput`
- `getCapabilitiesJson()`
- `inspectImageJson(inputBytes, declaredMediaType?)`
- `transformImage(inputBytes, declaredMediaType?, optionsJson)`
- `transformImageWithWatermark(inputBytes, declaredMediaType?, optionsJson, watermarkBytes, watermarkOptionsJson)`

For the JSON payload shapes, limits, and runtime caveats, see the repository's [WASM Integration guide](https://github.com/nao1215/truss/blob/main/docs/wasm.md).

## Compatibility

The package version tracks the truss release it is built from, so what a version number means is what the [crate documentation](https://docs.rs/truss-image) states. While the version is `0.x`, a minor release may change any of it.

From `1.0` on, the covered surface is the exported names listed above, the arguments each takes, and the field names of the JSON payloads they read and return, which [WASM Integration](https://github.com/nao1215/truss/blob/main/docs/wasm.md) specifies. Adding an export, an optional argument, or a payload field is a minor release; removing or renaming one is a major release, and so is changing what an existing field means. The `kind` values of a returned error are the class names of [Problem Types](https://github.com/nao1215/truss/blob/main/docs/problems.md), and are covered with them.

Not covered: the feature set listed above, which is a property of this build rather than of the API and may change in a minor release; the human-readable `message` of an error and the text of a warning; and the exact bytes an encode produces, which move with the codec libraries as they are upgraded.

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
