# Vite truss-wasm Example

Minimal browser consumer for `@nao1215/truss-wasm`.

This example is intentionally small:

- imports `getCapabilitiesJson`, `inspectImageJson`, and `transformImage`
- configures Vite with `vite-plugin-wasm` and `vite-plugin-top-level-await`
- runs a transform immediately on page load with an inline 1px PNG
- lets you select your own local image and rerun the same pipeline

## Run

```sh
cd examples/vite-truss-wasm
npm install
npm run dev
```

Then open the local Vite URL in your browser.

This checked-in example intentionally targets the latest published `@nao1215/truss-wasm` release so a third-party user can clone the repository and run it without rebuilding the package first.

To smoke-test the same example source against the current repository checkout before the next npm release, run this from the repository root:

```sh
node ./scripts/run-wasm-vite-example-smoke.mjs
```

That command packs the local `packages/truss-wasm` artifact, swaps it into a temporary copy of this example, installs dependencies, and runs `vite build`.

## What It Demonstrates

- npm installation with `@nao1215/truss-wasm`
- direct ESM import in a Vite app
- the Vite plugin setup required for `wasm-bindgen --target bundler` output
- runtime capability inspection
- byte-in / byte-out transform flow without any server

If you are creating your own Vite app from scratch, the minimum setup is:

```sh
npm create vite@latest my-truss-app -- --template vanilla
cd my-truss-app
npm install @nao1215/truss-wasm
npm install -D vite-plugin-wasm vite-plugin-top-level-await
```

Then add the same plugin setup as [`vite.config.js`](./vite.config.js).

## Core Import

```ts
import {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
} from "@nao1215/truss-wasm";
```
