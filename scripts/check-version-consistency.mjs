#!/usr/bin/env node
/**
 * Assert every published copy of the release version equals the crate's.
 *
 * The version is written in eight places across four files and a release edits all of them
 * by hand. A missed one publishes an artifact that reports a version other than its own: a
 * server whose `/health` says one thing and whose binary is another, or an npm package whose
 * manifest disagrees with the crate it wraps. Nothing else in CI reads more than one of the
 * copies, so this is what makes them a set rather than eight independent strings.
 *
 * The check only compares. Bumping the version stays a manual edit, because the release
 * procedure is where the decision about the number is made.
 *
 * usage: node scripts/check-version-consistency.mjs [--root <path>]
 */

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { parseArgs, reportAndExit } from "./lib/release-cli.mjs";
import {
  OPENAPI_VERSION_PATHS,
  collectChangelogProblems,
  collectVersionProblems,
  readCrateVersion,
  readYamlScalar,
} from "./lib/version-copies.mjs";

const options = parseArgs(process.argv.slice(2));
const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const root =
  typeof options.root === "string" ? path.resolve(options.root) : path.resolve(scriptDir, "..");

const read = (relative) => readFileSync(path.join(root, relative), "utf8");

const crateVersion = readCrateVersion(read("Cargo.toml"));
const openapi = read("docs/openapi.yaml");

const copies = [
  ...["packages/truss-url-signer/package.json", "packages/truss-wasm/package.json"].map(
    (relative) => ({
      label: relative,
      read: () => {
        const version = JSON.parse(read(relative)).version;
        return typeof version === "string"
          ? { value: version }
          : { error: "has no string version" };
      },
    }),
  ),
  ...OPENAPI_VERSION_PATHS.map((yamlPath) => ({
    label: `docs/openapi.yaml ${yamlPath.join(".")}`,
    read: () => readYamlScalar(openapi, yamlPath),
  })),
];

reportAndExit(`version ${crateVersion} across ${copies.length + 1} copies`, [
  ...collectVersionProblems(crateVersion, copies),
  ...collectChangelogProblems(read("CHANGELOG.md"), crateVersion),
]);
