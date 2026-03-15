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
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";
const packageDir = path.join(rootDir, "packages", "truss-url-signer");
const exampleDir = path.join(rootDir, "examples", "nextjs");
const npmCacheDir =
  process.env.NPM_CONFIG_CACHE ??
  path.join(os.tmpdir(), "truss-nextjs-example-npm-cache");
const keepTmp = process.env.TRUSS_KEEP_TMP === "1";
const tempRoot = mkdtempSync(path.join(os.tmpdir(), "truss-nextjs-example-"));
const packDir = path.join(tempRoot, "pack");
const tempExampleDir = path.join(tempRoot, "example");
const packageJson = JSON.parse(
  readFileSync(path.join(packageDir, "package.json"), "utf8"),
);
const tarballName = `nao1215-truss-url-signer-${packageJson.version}.tgz`;

mkdirSync(packDir, { recursive: true });
mkdirSync(npmCacheDir, { recursive: true });

try {
  run(npmCommand, ["pack", "--pack-destination", packDir], { cwd: packageDir });
  cpSync(exampleDir, tempExampleDir, {
    recursive: true,
    filter: (src) => !src.includes("node_modules") && !src.includes(".next"),
  });

  const examplePackageJsonPath = path.join(tempExampleDir, "package.json");
  const examplePackageJson = JSON.parse(readFileSync(examplePackageJsonPath, "utf8"));
  examplePackageJson.dependencies["@nao1215/truss-url-signer"] = `file:../pack/${tarballName}`;
  writeFileSync(
    examplePackageJsonPath,
    `${JSON.stringify(examplePackageJson, null, 2)}\n`,
  );

  run(npmCommand, ["install"], { cwd: tempExampleDir });
  run(npmCommand, ["run", "typecheck"], {
    cwd: tempExampleDir,
    env: {
      TRUSS_PUBLIC_BASE_URL: "http://localhost:8080",
      TRUSS_KEY_ID: "test-key",
      TRUSS_KEY_SECRET: "test-secret",
    },
  });
  run(npmCommand, ["run", "build"], {
    cwd: tempExampleDir,
    env: {
      TRUSS_PUBLIC_BASE_URL: "http://localhost:8080",
      TRUSS_KEY_ID: "test-key",
      TRUSS_KEY_SECRET: "test-secret",
    },
  });

  const builtDir = path.join(tempExampleDir, ".next");
  if (!existsSync(builtDir)) {
    throw new Error(`next build did not produce ${builtDir}`);
  }

  console.log(
    JSON.stringify({
      example: "nextjs",
      mode: "local-tarball",
      output: builtDir,
      packageUnderTest: tarballName,
    }),
  );
} catch (error) {
  console.error(`nextjs example smoke failed in ${tempRoot}`);
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
      ...(options.env ?? {}),
    },
    shell: process.platform === "win32" && command.endsWith(".cmd"),
    stdio: "inherit",
  });
}
