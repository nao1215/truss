import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(scriptDir, "..");
const packageDir = path.join(rootDir, "packages", "truss-wasm");
const exampleDir = path.join(rootDir, "examples", "vite-truss-wasm");
const npmCacheDir =
  process.env.NPM_CONFIG_CACHE ??
  path.join(os.tmpdir(), "truss-wasm-vite-example-npm-cache");
const keepTmp = process.env.TRUSS_KEEP_TMP === "1";
const tempRoot = mkdtempSync(path.join(os.tmpdir(), "truss-wasm-vite-example-"));
const packDir = path.join(tempRoot, "pack");
const tempExampleDir = path.join(tempRoot, "example");
const packageJson = JSON.parse(
  readFileSync(path.join(packageDir, "package.json"), "utf8"),
);
const tarballName = `nao1215-truss-wasm-${packageJson.version}.tgz`;

mkdirSync(packDir, { recursive: true });
mkdirSync(npmCacheDir, { recursive: true });

try {
  run("npm", ["pack", "--pack-destination", packDir], { cwd: packageDir });
  cpSync(exampleDir, tempExampleDir, { recursive: true });

  const examplePackageJsonPath = path.join(tempExampleDir, "package.json");
  const examplePackageJson = JSON.parse(readFileSync(examplePackageJsonPath, "utf8"));
  examplePackageJson.dependencies["@nao1215/truss-wasm"] = `file:../pack/${tarballName}`;
  writeFileSync(
    examplePackageJsonPath,
    `${JSON.stringify(examplePackageJson, null, 2)}\n`,
  );

  run("npm", ["install"], { cwd: tempExampleDir });
  run("npm", ["run", "build"], { cwd: tempExampleDir });

  const builtIndexPath = path.join(tempExampleDir, "dist", "index.html");
  if (!existsSync(builtIndexPath)) {
    throw new Error(`vite build did not produce ${builtIndexPath}`);
  }

  console.log(
    JSON.stringify({
      example: "vite-truss-wasm",
      output: builtIndexPath,
      packageUnderTest: tarballName,
    }),
  );
} catch (error) {
  console.error(`vite example smoke failed in ${tempRoot}`);
  throw error;
} finally {
  if (!keepTmp) {
    rmSync(tempRoot, { force: true, recursive: true });
  }
}

function run(command, args, options = {}) {
  execFileSync(command, args, {
    cwd: options.cwd ?? rootDir,
    env: {
      ...process.env,
      NPM_CONFIG_CACHE: npmCacheDir,
    },
    stdio: "inherit",
  });
}
