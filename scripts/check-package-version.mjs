import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(scriptDir, "..");
const packageDirArg = process.argv[2];

if (!packageDirArg) {
  throw new Error("usage: node ./scripts/check-package-version.mjs <package-dir>");
}

const packageDir = path.resolve(process.cwd(), packageDirArg);
const cargoManifestPath = path.join(rootDir, "Cargo.toml");
const packageJsonPath = path.join(packageDir, "package.json");
const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8"));
const cargoToml = readFileSync(cargoManifestPath, "utf8");
const cargoVersionMatch = cargoToml.match(
  /^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
);

if (!cargoVersionMatch) {
  throw new Error(`failed to read crate version from ${cargoManifestPath}`);
}

if (packageJson.version !== cargoVersionMatch[1]) {
  throw new Error(
    `package version ${packageJson.version} must match crate version ${cargoVersionMatch[1]}`,
  );
}
