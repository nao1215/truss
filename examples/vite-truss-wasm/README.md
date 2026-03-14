# Vite truss-wasm Example

Minimal browser consumer for `@nao1215/truss-wasm`.

This example is intentionally small:

- imports `getCapabilitiesJson`, `inspectImageJson`, and `transformImage`
- configures Vite with `vite-plugin-top-level-await`
- runs a transform immediately on page load with an inline 1px PNG
- lets you select your own local image and rerun the same pipeline

## Run

```sh
cat ../../.nvmrc
cd examples/vite-truss-wasm
npm ci
npm run dev
```

Then open the local Vite URL in your browser.

This checked-in example intentionally installs the sibling `packages/truss-wasm` source from the same repository checkout, so the example always matches the current branch. Use the Node.js version from the repository root `.nvmrc` so your local setup matches CI.

To smoke-test the same example source against the current repository checkout before the next npm release, run this from the repository root:

```sh
node ./scripts/run-wasm-vite-example-smoke.mjs
```

That command packs the local `packages/truss-wasm` artifact, swaps it into a temporary copy of this example, installs dependencies, and runs `vite build`.

To verify the checked-in example exactly as written, including browser runtime behavior, run this from the repository root:

```sh
node ./scripts/run-wasm-vite-example-runtime-smoke.mjs
```

## What It Demonstrates

- npm installation with `@nao1215/truss-wasm`
- direct ESM import in a Vite app
- the minimal Vite setup required for the package wrapper's top-level await
- runtime capability inspection
- byte-in / byte-out transform flow without any server

If you are creating your own Vite app from scratch, the minimum setup is:

```sh
npm create vite@latest my-truss-app -- --template vanilla
cd my-truss-app
npm install @nao1215/truss-wasm
npm install -D vite-plugin-top-level-await
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
