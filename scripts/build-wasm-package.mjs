import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(scriptDir, "..");
const packageDir = path.join(rootDir, "packages", "truss-wasm");
const distDir = path.join(packageDir, "dist");
const cargoManifestPath = path.join(rootDir, "Cargo.toml");
const cargoWasmPath = path.join(
  rootDir,
  "target",
  "wasm32-unknown-unknown",
  "release",
  "truss.wasm",
);
const packageJsonPath = path.join(packageDir, "package.json");
const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8"));
const packageVersion = packageJson.version;
const wasmBindgenVersion = packageJson.trussWasmBuild?.wasmBindgenVersion;
const cargoVersionMatch = readFileSync(cargoManifestPath, "utf8").match(
  /^version = "([^"]+)"/m,
);

if (!wasmBindgenVersion) {
  throw new Error(
    `failed to read trussWasmBuild.wasmBindgenVersion from ${packageJsonPath}`,
  );
}

if (!cargoVersionMatch) {
  throw new Error(`failed to read crate version from ${cargoManifestPath}`);
}

const crateVersion = cargoVersionMatch[1];
if (crateVersion !== packageVersion) {
  throw new Error(
    `package version ${packageVersion} must match crate version ${crateVersion}`,
  );
}

function run(command, args, options = {}) {
  try {
    return execFileSync(command, args, {
      cwd: rootDir,
      encoding: options.capture ? "utf8" : undefined,
      stdio: options.capture ? "pipe" : "inherit",
    });
  } catch (error) {
    // Some sandboxes return stdout together with an EPERM wrapper for captured child output.
    if (options.capture && error?.status === 0 && typeof error.stdout === "string") {
      return error.stdout;
    }

    throw error;
  }
}

let detectedWasmBindgenVersion;

try {
  const versionOutput = run("wasm-bindgen", ["--version"], { capture: true }).trim();
  const versionMatch = versionOutput.match(/^wasm-bindgen\s+(.+)$/);

  if (!versionMatch) {
    throw new Error(
      `failed to parse wasm-bindgen CLI version from output: ${JSON.stringify(versionOutput)}`,
    );
  }

  detectedWasmBindgenVersion = versionMatch[1];
} catch (error) {
  if (error?.code !== "ENOENT") {
    throw error;
  }

  throw new Error(
    `wasm-bindgen CLI is required. Install it with \`cargo install wasm-bindgen-cli --version ${wasmBindgenVersion}\`.`,
  );
}

if (detectedWasmBindgenVersion !== wasmBindgenVersion) {
  throw new Error(
    `incompatible wasm-bindgen CLI version ${detectedWasmBindgenVersion}; expected ${wasmBindgenVersion}. Install the matching CLI with \`cargo install wasm-bindgen-cli --version ${wasmBindgenVersion}\`.`,
  );
}

rmSync(distDir, { force: true, recursive: true });
mkdirSync(distDir, { recursive: true });

run("cargo", [
  "build",
  "--release",
  "--locked",
  "--target",
  "wasm32-unknown-unknown",
  "--lib",
  "--no-default-features",
  "--features",
  "wasm,svg,avif",
  "--manifest-path",
  cargoManifestPath,
]);

run("wasm-bindgen", [
  "--target",
  "bundler",
  "--out-dir",
  distDir,
  cargoWasmPath,
]);
