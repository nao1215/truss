#!/usr/bin/env node
/**
 * Check one freshly built distribution archive, on the runner that built it.
 *
 * This runs inside the build matrix, before the archive is uploaded anywhere,
 * so a malformed archive never reaches the release. It verifies the archive
 * opens, holds exactly the executable at the expected path with the expected
 * mode and ownership, and -- when the runner can execute the target -- that
 * `truss --version` succeeds and reports the version being released.
 *
 * Cross-compiled targets pass `--exec false`: they get every check except the
 * one that would need a foreign CPU.
 *
 * usage:
 *   node scripts/check-release-archive.mjs \
 *     --archive truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz \
 *     --target x86_64-unknown-linux-gnu \
 *     --tag v1.2.3 \
 *     [--exec]
 */

import { readFileSync } from "node:fs";
import { basename } from "node:path";

import { formatFromName, readArchive } from "./lib/archive.mjs";
import {
  archiveName,
  binaryName,
  checkArchiveLayout,
} from "./lib/release-manifest.mjs";
import {
  checkBinaryVersion,
  parseArgs,
  reportAndExit,
  requireOption,
} from "./lib/release-cli.mjs";

const USAGE =
  "usage: node scripts/check-release-archive.mjs --archive <path> --target <triple> --tag <tag> [--exec]";

const options = parseArgs(process.argv.slice(2));
const archivePath = requireOption(options, "archive", USAGE);
const target = requireOption(options, "target", USAGE);
const tag = requireOption(options, "tag", USAGE);
const shouldExecute = options.exec === true || options.exec === "true";

const problems = [];
const fileName = basename(archivePath);
const format = formatFromName(fileName);

const expectedFileName = archiveName(tag, target, format);
if (fileName !== expectedFileName) {
  problems.push(`archive is named ${fileName}, expected ${expectedFileName}`);
}

const buffer = readFileSync(archivePath);
const entries = readArchive(buffer, format);
problems.push(...checkArchiveLayout(entries, { target }));

if (problems.length === 0 && shouldExecute) {
  problems.push(
    ...checkBinaryVersion(entries[0].data, {
      fileName: binaryName(target),
      expectedVersion: tag.startsWith("v") ? tag.slice(1) : tag,
    }),
  );
}

reportAndExit(`${fileName} (${target})`, problems);
