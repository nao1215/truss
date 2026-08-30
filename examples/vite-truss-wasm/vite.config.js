import { defineConfig } from "vite";

export default defineConfig({
  // `@nao1215/truss-wasm` initializes with a top-level await, so the build target has to be
  // one where browsers support that natively. These are the first releases that do, and
  // they are also the floor for the `new URL(..., import.meta.url)` asset resolution the
  // package uses to locate its `.wasm` binary, so nothing here is arbitrary.
  //
  // Targeting anything older means transforming the top-level await with a plugin, which
  // this example used to do. Dropping that plugin also removes the Rollup dependency it
  // pulled in, which is what blocked even attempting Vite 8.
  build: {
    target: ["es2022", "edge89", "firefox89", "chrome89", "safari15"],
  },
});
