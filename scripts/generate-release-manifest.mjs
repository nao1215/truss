#!/usr/bin/env node
/**
 * Build `release-manifest.json` and `checksums.txt` from the release archives.
 *
 * Every SHA-256 in the release is computed here, exactly once: `checksums.txt`
 * is rendered from the manifest rather than accumulated from per-job sidecar
 * files, so the two documents cannot disagree about an archive.
 *
 * usage:
 *   node scripts/generate-release-manifest.mjs \
 *     --dir release-assets \
 *     --tag v1.2.3 \
 *     --repository nao1215/truss \
 *     --features s3,gcs,azure \
 *     [--targets-file .github/release-targets.json] \
 *     [--cargo-toml Cargo.toml] \
 *     [--out release-assets/release-manifest.json] \
 *     [--checksums release-assets/checksums.txt]
 */

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { formatFromName, readArchive } from "./lib/archive.mjs";
import {
  archiveName,
  buildManifest,
  checkArchiveLayout,
  parseCargoFeatures,
  readCargoField,
  renderChecksums,
  resolveFeatures,
} from "./lib/release-manifest.mjs";
import {
  parseArgs,
  requireOption,
  sha256,
  splitList,
} from "./lib/release-cli.mjs";

const USAGE =
  "usage: node scripts/generate-release-manifest.mjs --dir <dir> --tag <tag> --repository <owner/name> --features <csv> [--targets-file <path>] [--cargo-toml <path>] [--out <path>] [--checksums <path>]";

const options = parseArgs(process.argv.slice(2));
const directory = requireOption(options, "dir", USAGE);
const tag = requireOption(options, "tag", USAGE);
const repository = requireOption(options, "repository", USAGE);
const extraFeatures = splitList(requireOption(options, "features", USAGE));
const targetsFile =
  typeof options["targets-file"] === "string"
    ? options["targets-file"]
    : ".github/release-targets.json";
const cargoTomlPath =
  typeof options["cargo-toml"] === "string" ? options["cargo-toml"] : "Cargo.toml";
const outPath =
  typeof options.out === "string" ? options.out : join(directory, "release-manifest.json");
const checksumsPath =
  typeof options.checksums === "string" ? options.checksums : join(directory, "checksums.txt");

const cargoToml = readFileSync(cargoTomlPath, "utf8");
const version = readCargoField(cargoToml, "package", "version");
const expectedVersion = tag.startsWith("v") ? tag.slice(1) : tag;

if (version !== expectedVersion) {
  throw new Error(`tag ${tag} does not match Cargo.toml version ${version}`);
}

const features = resolveFeatures(parseCargoFeatures(cargoToml), extraFeatures);
const targets = JSON.parse(readFileSync(targetsFile, "utf8"));

const artifacts = targets.map((entry) => {
  const format = entry.archive;
  const name = archiveName(tag, entry.target, format);
  const path = join(directory, name);
  const buffer = readFileSync(path);

  if (formatFromName(name) !== format) {
    throw new Error(`${name} does not match declared format ${format}`);
  }

  const entries = readArchive(buffer, format);
  const problems = checkArchiveLayout(entries, { target: entry.target });
  if (problems.length > 0) {
    throw new Error(`${name} has a malformed layout:\n  - ${problems.join("\n  - ")}`);
  }

  const binary = entries[0];

  return {
    target: entry.target,
    format,
    archiveSha256: sha256(buffer),
    archiveSize: buffer.length,
    binarySha256: sha256(binary.data),
    binarySize: binary.data.length,
  };
});

const manifest = buildManifest({
  version,
  tag,
  repository,
  features,
  artifacts,
});

writeFileSync(outPath, `${JSON.stringify(manifest, null, 2)}\n`);
writeFileSync(checksumsPath, renderChecksums(manifest));

console.log(`wrote ${outPath} (${manifest.artifacts.length} artifacts)`);
console.log(`wrote ${checksumsPath}`);
