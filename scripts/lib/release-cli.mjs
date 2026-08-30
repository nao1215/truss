/**
 * Filesystem and process helpers shared by the release manifest CLIs.
 *
 * Kept apart from `release-manifest.mjs` so the manifest logic itself stays
 * pure and unit testable without touching disk.
 */

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * Parse `--key value` and `--flag` arguments.
 *
 * @param {string[]} argv
 * @returns {Record<string, string | boolean>}
 */
export function parseArgs(argv) {
  const options = {};

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) {
      throw new Error(`unexpected argument: ${arg}`);
    }

    const key = arg.slice(2);
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) {
      options[key] = true;
    } else {
      options[key] = next;
      index += 1;
    }
  }

  return options;
}

/**
 * Read a required string option.
 *
 * @param {Record<string, string | boolean>} options
 * @param {string} key
 * @param {string} usage
 */
export function requireOption(options, key, usage) {
  const value = options[key];
  if (typeof value !== "string" || value === "") {
    throw new Error(`missing --${key}\n${usage}`);
  }
  return value;
}

/**
 * Split a comma-separated option into a trimmed, non-empty list.
 *
 * @param {string} value
 * @returns {string[]}
 */
export function splitList(value) {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter((item) => item !== "");
}

/**
 * Lowercase hex SHA-256 of a buffer.
 *
 * @param {Buffer} buffer
 * @returns {string}
 */
export function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

/**
 * Write the extracted executable to a temporary file and run `--version`.
 *
 * Used for the targets a runner can actually execute; cross-compiled artifacts
 * are checked structurally instead.
 *
 * @param {Buffer} binary extracted executable bytes
 * @param {{fileName: string, expectedVersion: string}} expectations
 * @returns {string[]} problems, empty when the binary runs and reports the expected version
 */
export function checkBinaryVersion(binary, expectations) {
  const directory = mkdtempSync(join(tmpdir(), "truss-release-"));
  const path = join(directory, expectations.fileName);

  try {
    writeFileSync(path, binary);
    chmodSync(path, 0o755);

    const result = spawnSync(path, ["--version"], { encoding: "utf8" });

    if (result.error) {
      return [`running ${expectations.fileName} --version failed: ${result.error.message}`];
    }
    if (result.status !== 0) {
      return [
        `${expectations.fileName} --version exited with ${result.status}: ${(result.stderr ?? "").trim()}`,
      ];
    }

    const stdout = (result.stdout ?? "").trim();
    if (!stdout.split(/\s+/).includes(expectations.expectedVersion)) {
      return [
        `${expectations.fileName} --version reported "${stdout}", expected version ${expectations.expectedVersion}`,
      ];
    }

    return [];
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

/**
 * Print problems and exit non-zero when there are any.
 *
 * @param {string} title
 * @param {string[]} problems
 */
export function reportAndExit(title, problems) {
  if (problems.length === 0) {
    console.log(`${title}: ok`);
    return;
  }

  console.error(`${title}: ${problems.length} problem(s)`);
  for (const problem of problems) {
    console.error(`  - ${problem}`);
  }
  process.exit(1);
}
