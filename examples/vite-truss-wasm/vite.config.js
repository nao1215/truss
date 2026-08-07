import { defineConfig } from "vite";
import topLevelAwait from "vite-plugin-top-level-await";

export default defineConfig({
  // Vite's default baseline includes Safari 14.0, whose destructuring support
  // esbuild >= 0.28 flags as broken. esbuild cannot lower the destructuring
  // emitted by the wasm-bindgen glue, so the build fails. Safari 14.1 is the
  // first release that ships a usable implementation, and it is also the first
  // Safari with top-level await, which this example relies on.
  build: {
    target: ["es2020", "edge88", "firefox78", "chrome87", "safari14.1"],
  },
  plugins: [topLevelAwait()],
});
