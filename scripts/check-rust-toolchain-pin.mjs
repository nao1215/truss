#!/usr/bin/env node
/**
 * Assert the release workflow's pinned Rust toolchain is explicit and is not
 * older than the crate's declared `rust-version`.
 *
 * The release used to build on `stable`, which meant the compiler that produced
 * a published binary depended on the day it was tagged, and nothing tied it to
 * the MSRV the crate claims. This check keeps the pin honest as either value
 * moves.
 *
 * usage: node scripts/check-rust-toolchain-pin.mjs --toolchain 1.92.0 [--cargo-toml Cargo.toml]
 */

import { readFileSync } from "node:fs";

import { compareVersions, readCargoField } from "./lib/release-manifest.mjs";
import { parseArgs, reportAndExit, requireOption } from "./lib/release-cli.mjs";

const USAGE =
  "usage: node scripts/check-rust-toolchain-pin.mjs --toolchain <version> [--cargo-toml <path>]";

const options = parseArgs(process.argv.slice(2));
const toolchain = requireOption(options, "toolchain", USAGE);
const cargoTomlPath =
  typeof options["cargo-toml"] === "string" ? options["cargo-toml"] : "Cargo.toml";

const problems = [];

if (!/^\d+\.\d+(\.\d+)?$/.test(toolchain)) {
  problems.push(
    `pinned toolchain "${toolchain}" is not an explicit version; release builds must not use stable/beta/nightly`,
  );
}

const rustVersion = readCargoField(readFileSync(cargoTomlPath, "utf8"), "package", "rust-version");

if (problems.length === 0 && compareVersions(toolchain, rustVersion) < 0) {
  problems.push(
    `pinned toolchain ${toolchain} is older than Cargo.toml rust-version ${rustVersion}`,
  );
}

reportAndExit(`rust toolchain pin ${toolchain} (rust-version ${rustVersion})`, problems);
