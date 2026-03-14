import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(scriptDir, "..");
const packageDir = path.join(rootDir, "packages", "truss-wasm");
const packageJsonPath = path.join(packageDir, "package.json");
const fixtureImagePath = path.join(rootDir, "integration", "fixtures", "1px.png");
const npmCacheDir =
  process.env.NPM_CONFIG_CACHE ??
  path.join(os.tmpdir(), "truss-wasm-consumer-npm-cache");
const keepTmp = process.env.TRUSS_KEEP_TMP === "1";
const tempRoot = mkdtempSync(path.join(os.tmpdir(), "truss-wasm-consumer-"));
const packDir = path.join(tempRoot, "pack");
const consumerDir = path.join(tempRoot, "consumer");

const packageJson = JSON.parse(readFileSync(packageJsonPath, "utf8"));
const packageVersion = packageJson.version;
const tarballName = `nao1215-truss-wasm-${packageVersion}.tgz`;
const tarballPath = path.join(packDir, tarballName);

mkdirSync(packDir, { recursive: true });
mkdirSync(consumerDir, { recursive: true });
mkdirSync(npmCacheDir, { recursive: true });

try {
  run("npm", ["pack", "--pack-destination", packDir], { cwd: packageDir });

  writeFileSync(
    path.join(consumerDir, "package.json"),
    `${JSON.stringify(
      {
        name: "truss-wasm-consumer-smoke",
        private: true,
        type: "module",
      },
      null,
      2,
    )}\n`,
  );

  writeFileSync(
    path.join(consumerDir, "smoke.mjs"),
    `import fs from "node:fs";
import {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
} from "@nao1215/truss-wasm";

const inputPath = process.argv[2];
const inputBytes = new Uint8Array(fs.readFileSync(inputPath));
const capabilities = JSON.parse(getCapabilitiesJson());
const inspected = JSON.parse(inspectImageJson(inputBytes, undefined));

if (
  typeof capabilities.svg !== "boolean" ||
  typeof capabilities.webpLossy !== "boolean" ||
  typeof capabilities.avif !== "boolean"
) {
  throw new Error(\`unexpected capabilities payload: \${JSON.stringify(capabilities)}\`);
}

if (inspected.artifact.mediaType !== "png" || inspected.artifact.width !== 1 || inspected.artifact.height !== 1) {
  throw new Error(\`unexpected inspection payload: \${JSON.stringify(inspected)}\`);
}

const result = transformImage(
  inputBytes,
  undefined,
  JSON.stringify({
    format: "jpeg",
    width: 4,
    height: 4,
    fit: "fill",
    background: "FFFFFF",
    quality: 80
  }),
);

const response = JSON.parse(result.responseJson);

if (response.artifact.mediaType !== "jpeg" || response.artifact.width !== 4 || response.artifact.height !== 4) {
  throw new Error(\`unexpected transform payload: \${JSON.stringify(response)}\`);
}

if (!(result.bytes instanceof Uint8Array) || result.bytes.length === 0) {
  throw new Error("transform returned no bytes");
}

console.log(JSON.stringify({
  capabilities,
  inspected: inspected.artifact,
  output: response.artifact,
  outputBytes: result.bytes.length
}));
`,
  );

  run("npm", ["install", "--no-save", tarballPath], { cwd: consumerDir });
  run("node", ["--no-warnings", "smoke.mjs", fixtureImagePath], {
    cwd: consumerDir,
  });
} catch (error) {
  console.error(`consumer smoke failed in ${tempRoot}`);
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
