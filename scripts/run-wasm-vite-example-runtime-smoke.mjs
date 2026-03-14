import { spawn, execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(scriptDir, "..");
const exampleDir = path.join(rootDir, "examples", "vite-truss-wasm");
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";
const npmCacheDir =
  process.env.NPM_CONFIG_CACHE ??
  path.join(process.env.TMPDIR ?? os.tmpdir(), "truss-wasm-vite-runtime-npm-cache");
const chromeBin = process.env.TRUSS_CHROME_BIN ?? "google-chrome";
const previewPort = 4173;
const previewUrl = `http://127.0.0.1:${previewPort}`;

let previewProcess;

try {
  run(npmCommand, ["ci"], { cwd: exampleDir });
  run(npmCommand, ["run", "build"], { cwd: exampleDir });

  previewProcess = spawn(
    npmCommand,
    ["run", "preview", "--", "--host", "127.0.0.1", "--port", String(previewPort)],
    {
      cwd: exampleDir,
      env: {
        ...process.env,
        NPM_CONFIG_CACHE: npmCacheDir,
      },
      shell: process.platform === "win32",
      stdio: "inherit",
    },
  );

  await waitForPreview(previewUrl);

  const requireFromExample = createRequire(path.join(exampleDir, "package.json"));
  const puppeteer = requireFromExample("puppeteer-core");
  const browserMessages = [];
  const browser = await puppeteer.launch({
    executablePath: resolveChromeExecutable(chromeBin),
    headless: "new",
    args: ["--disable-gpu", "--no-sandbox"],
  });

  try {
    const page = await browser.newPage();
    page.on("console", (message) => {
      browserMessages.push(`console:${message.type()}: ${message.text()}`);
    });
    page.on("pageerror", (error) => {
      browserMessages.push(`pageerror: ${error.message}`);
    });

    await page.goto(previewUrl, { waitUntil: "networkidle0" });

    try {
      await page.waitForFunction(
        () => {
          const status = document.querySelector("#status");
          return (
            status &&
            status.textContent &&
            (status.textContent.startsWith("Done: ") ||
              status.textContent.startsWith("Transform failed:"))
          );
        },
        { timeout: 15_000 },
      );
    } catch (error) {
      const html = await page.content();
      throw new Error(
        `runtime status did not settle within timeout\n${browserMessages.join("\n")}\n${html}`,
        { cause: error },
      );
    }

    const statusText = await page.$eval("#status", (element) => element.textContent ?? "");
    if (statusText !== "Done: image/jpeg 240x240") {
      throw new Error(
        `unexpected runtime status: ${statusText}\n${browserMessages.join("\n")}`,
      );
    }
  } finally {
    await browser.close();
  }

  console.log(
    JSON.stringify({
      example: "vite-truss-wasm",
      mode: "repo-runtime",
      previewUrl,
      status: "Done: image/jpeg 240x240",
    }),
  );
} finally {
  if (previewProcess && !previewProcess.killed) {
    previewProcess.kill("SIGTERM");
  }
}

function run(command, args, options = {}) {
  execFileSync(command, args, {
    cwd: options.cwd ?? rootDir,
    env: {
      ...process.env,
      NPM_CONFIG_CACHE: npmCacheDir,
    },
    shell: process.platform === "win32" && command.endsWith(".cmd"),
    stdio: "inherit",
  });
}

function resolveChromeExecutable(preferredCommand) {
  const candidates = [
    preferredCommand,
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
  ];

  for (const candidate of candidates) {
    const executable = resolveExecutableCandidate(candidate);
    if (!executable) {
      continue;
    }

    try {
      execFileSync(executable, ["--version"], {
        stdio: ["ignore", "ignore", "ignore"],
      });
      return executable;
    } catch {
      // Try the next candidate.
    }
  }

  throw new Error(
    `headless browser is unavailable. Set TRUSS_CHROME_BIN to a Chrome-compatible executable.`,
  );
}

function resolveExecutableCandidate(candidate) {
  if (path.isAbsolute(candidate)) {
    return existsSync(candidate) ? candidate : null;
  }

  try {
    if (process.platform === "win32") {
      return execFileSync("where.exe", [candidate], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      })
        .split(/\r?\n/)
        .find(Boolean)
        ?.trim() ?? null;
    }

    return execFileSync("sh", ["-lc", `command -v ${candidate}`], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

async function waitForPreview(url) {
  const deadline = Date.now() + 15_000;

  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) {
        return;
      }
    } catch {
      // Retry until the preview server starts responding.
    }

    await new Promise((resolve) => setTimeout(resolve, 250));
  }

  throw new Error(`timed out waiting for Vite preview at ${url}`);
}
