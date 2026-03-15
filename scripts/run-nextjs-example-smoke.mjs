import { execFileSync, spawn } from "node:child_process";
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

const trussEnv = {
  TRUSS_PUBLIC_BASE_URL: "http://localhost:8080",
  TRUSS_KEY_ID: "test-key",
  TRUSS_KEY_SECRET: "test-secret",
};

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
    env: trussEnv,
  });
  run(npmCommand, ["run", "build"], {
    cwd: tempExampleDir,
    env: trussEnv,
  });

  const standaloneDir = path.join(tempExampleDir, ".next", "standalone");
  if (!existsSync(standaloneDir)) {
    throw new Error(`next build did not produce ${standaloneDir}`);
  }

  // ── Runtime integration tests ──────────────────────────────────────
  // Start the standalone server and exercise the /api/truss route to
  // verify signing, validation, and error handling at runtime.
  await runIntegrationTests(standaloneDir);

  console.log(
    JSON.stringify({
      example: "nextjs",
      mode: "local-tarball",
      output: standaloneDir,
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

// ── Integration test runner ────────────────────────────────────────────

async function runIntegrationTests(standaloneDir) {
  const port = 3099;
  const baseUrl = `http://127.0.0.1:${port}`;
  const serverScript = path.join(standaloneDir, "server.js");

  console.log(`\nStarting Next.js standalone server on port ${port}...`);
  const server = spawn("node", [serverScript], {
    cwd: standaloneDir,
    env: {
      ...process.env,
      ...trussEnv,
      PORT: String(port),
      HOSTNAME: "127.0.0.1",
    },
    stdio: "pipe",
  });

  try {
    await waitForServer(baseUrl, server, 15_000);
    console.log("Server is ready. Running integration tests...\n");

    let passed = 0;
    let failed = 0;

    for (const tc of testCases(baseUrl)) {
      try {
        await tc.fn();
        console.log(`  ✓ ${tc.name}`);
        passed++;
      } catch (err) {
        console.error(`  ✗ ${tc.name}: ${err.message}`);
        failed++;
      }
    }

    console.log(`\n  ${passed} passed, ${failed} failed`);
    if (failed > 0) {
      throw new Error(`${failed} integration test(s) failed`);
    }
  } finally {
    server.kill("SIGTERM");
    // Give it a moment to clean up.
    await new Promise((r) => setTimeout(r, 500));
  }
}

async function waitForServer(baseUrl, proc, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    // Check if the process exited unexpectedly.
    if (proc.exitCode !== null) {
      throw new Error(`Server exited with code ${proc.exitCode}`);
    }
    try {
      const res = await fetch(`${baseUrl}/api/truss?path=test.jpg`);
      if (res.ok || res.status === 400) return;
    } catch {
      // Connection refused — server not ready yet.
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`Server did not become ready within ${timeoutMs}ms`);
}

function* testCases(baseUrl) {
  const api = (query) => `${baseUrl}/api/truss?${query}`;

  yield {
    name: "valid path returns signed URL",
    fn: async () => {
      const res = await fetch(api("path=photos/hero.jpg&width=800&format=webp"));
      assertStatus(res, 200);
      const body = await res.json();
      assert(typeof body.url === "string", "response should contain url string");
      assert(body.url.includes("signature="), "url should contain signature");
      assert(body.url.includes("expires="), "url should contain expires");
    },
  };

  yield {
    name: "same parameters produce same signed URL (cache-stable expires)",
    fn: async () => {
      const query = "path=photos/hero.jpg&width=400&format=jpeg";
      const res1 = await fetch(api(query));
      const res2 = await fetch(api(query));
      assertStatus(res1, 200);
      assertStatus(res2, 200);
      const body1 = await res1.json();
      const body2 = await res2.json();
      assert(body1.url === body2.url, "URLs should be identical within the same TTL window");
    },
  };

  yield {
    name: "missing path returns 400",
    fn: async () => {
      const res = await fetch(api("width=100"));
      assertStatus(res, 400);
      const body = await res.json();
      assert(body.error === "path is required", `expected "path is required", got "${body.error}"`);
    },
  };

  yield {
    name: "path traversal returns 400",
    fn: async () => {
      const res = await fetch(api("path=../etc/passwd"));
      assertStatus(res, 400);
      const body = await res.json();
      assert(body.error === "invalid path", `expected "invalid path", got "${body.error}"`);
    },
  };

  yield {
    name: "invalid format returns 400",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&format=gif"));
      assertStatus(res, 400);
    },
  };

  yield {
    name: "invalid width returns 400",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&width=0"));
      assertStatus(res, 400);
    },
  };

  yield {
    name: "width exceeding max returns 400",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&width=9999"));
      assertStatus(res, 400);
    },
  };

  yield {
    name: "fit without both dimensions returns 400",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&fit=cover&width=100"));
      assertStatus(res, 400);
      const body = await res.json();
      assert(
        body.error.includes("fit requires both"),
        `expected fit error, got "${body.error}"`,
      );
    },
  };

  yield {
    name: "fit with both dimensions succeeds",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&fit=cover&width=100&height=100"));
      assertStatus(res, 200);
    },
  };

  yield {
    name: "invalid quality returns 400",
    fn: async () => {
      const res = await fetch(api("path=test.jpg&quality=0"));
      assertStatus(res, 400);
    },
  };
}

function assertStatus(res, expected) {
  assert(
    res.status === expected,
    `expected status ${expected}, got ${res.status}`,
  );
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}
