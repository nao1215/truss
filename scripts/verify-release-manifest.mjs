#!/usr/bin/env node
/**
 * Verify the release assets against `release-manifest.json` before publishing.
 *
 * Checks, for the whole release at once:
 *   * the manifest covers exactly the declared distribution targets, with no
 *     duplicate target, archive name or download URL;
 *   * every archive the manifest names exists, and its SHA-256 and byte size
 *     match what the manifest records;
 *   * every archive opens, holds exactly the executable at the recorded path,
 *     and the executable's SHA-256 and byte size match the manifest;
 *   * `checksums.txt` agrees with the manifest on every archive hash;
 *   * on the target this runner can execute, `truss --version` succeeds and
 *     reports the version being released.
 *
 * usage:
 *   node scripts/verify-release-manifest.mjs \
 *     --dir release-assets \
 *     --tag v1.2.3 \
 *     --repository nao1215/truss \
 *     [--targets-file .github/release-targets.json] \
 *     [--manifest release-assets/release-manifest.json] \
 *     [--checksums release-assets/checksums.txt] \
 *     [--exec-target x86_64-unknown-linux-gnu]
 */

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { readArchive } from "./lib/archive.mjs";
import { checkArchiveLayout, checkManifest } from "./lib/release-manifest.mjs";
import {
  checkBinaryVersion,
  parseArgs,
  reportAndExit,
  requireOption,
  sha256,
} from "./lib/release-cli.mjs";

const USAGE =
  "usage: node scripts/verify-release-manifest.mjs --dir <dir> --tag <tag> --repository <owner/name> [--targets-file <path>] [--manifest <path>] [--checksums <path>] [--exec-target <triple>]";

const options = parseArgs(process.argv.slice(2));
const directory = requireOption(options, "dir", USAGE);
const tag = requireOption(options, "tag", USAGE);
const repository = requireOption(options, "repository", USAGE);
const targetsFile =
  typeof options["targets-file"] === "string"
    ? options["targets-file"]
    : ".github/release-targets.json";
const manifestPath =
  typeof options.manifest === "string"
    ? options.manifest
    : join(directory, "release-manifest.json");
const checksumsPath =
  typeof options.checksums === "string" ? options.checksums : join(directory, "checksums.txt");
const execTarget = typeof options["exec-target"] === "string" ? options["exec-target"] : null;

const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const targets = JSON.parse(readFileSync(targetsFile, "utf8")).map((entry) => entry.target);

const problems = checkManifest(manifest, {
  tag,
  repository,
  targets,
  checksumsText: readFileSync(checksumsPath, "utf8"),
});

for (const artifact of manifest.artifacts ?? []) {
  const label = artifact.target;
  const path = join(directory, artifact.archive.name);

  if (!existsSync(path)) {
    problems.push(`${label}: ${artifact.archive.name} is missing from ${directory}`);
    continue;
  }

  const buffer = readFileSync(path);

  if (buffer.length !== artifact.archive.size) {
    problems.push(
      `${label}: archive is ${buffer.length} bytes, manifest says ${artifact.archive.size}`,
    );
  }

  const archiveDigest = sha256(buffer);
  if (archiveDigest !== artifact.archive.sha256) {
    problems.push(
      `${label}: archive SHA-256 is ${archiveDigest}, manifest says ${artifact.archive.sha256}`,
    );
    continue;
  }

  let entries;
  try {
    entries = readArchive(buffer, artifact.archive.format);
  } catch (error) {
    problems.push(`${label}: archive could not be extracted: ${error.message}`);
    continue;
  }

  const layoutProblems = checkArchiveLayout(entries, { target: artifact.target });
  problems.push(...layoutProblems.map((problem) => `${label}: ${problem}`));
  if (layoutProblems.length > 0) {
    continue;
  }

  const binary = entries[0];

  if (binary.data.length !== artifact.binary.size) {
    problems.push(
      `${label}: executable is ${binary.data.length} bytes, manifest says ${artifact.binary.size}`,
    );
  }

  const binaryDigest = sha256(binary.data);
  if (binaryDigest !== artifact.binary.sha256) {
    problems.push(
      `${label}: executable SHA-256 is ${binaryDigest}, manifest says ${artifact.binary.sha256}`,
    );
    continue;
  }

  if (execTarget !== null && artifact.target === execTarget) {
    problems.push(
      ...checkBinaryVersion(binary.data, {
        fileName: artifact.binary.path,
        expectedVersion: manifest.version,
      }).map((problem) => `${label}: ${problem}`),
    );
  }
}

if (execTarget !== null && !(manifest.artifacts ?? []).some((a) => a.target === execTarget)) {
  problems.push(`--exec-target ${execTarget} is not present in the manifest`);
}

reportAndExit(`release ${tag}`, problems);
