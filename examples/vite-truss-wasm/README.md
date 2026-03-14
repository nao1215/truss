# Vite truss-wasm Example

Minimal browser consumer for `@nao1215/truss-wasm`.

This example is intentionally small:

- imports `getCapabilitiesJson`, `inspectImageJson`, and `transformImage`
- runs a transform immediately on page load with an inline 1px PNG
- lets you select your own local image and rerun the same pipeline

## Run

```sh
cd examples/vite-truss-wasm
npm install
npm run dev
```

Then open the local Vite URL in your browser.

## What It Demonstrates

- npm installation with `@nao1215/truss-wasm`
- direct ESM import in a Vite app
- runtime capability inspection
- byte-in / byte-out transform flow without any server

## Core Import

```ts
import {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
} from "@nao1215/truss-wasm";
```
