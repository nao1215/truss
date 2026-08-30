/**
 * End-to-end test for the release distribution tooling.
 *
 * It packs stand-in executables with the same script the release workflow uses,
 * generates `release-manifest.json` and `checksums.txt` from them, and runs the
 * verifier over the result -- including the failure modes the verifier exists to
 * catch. Nothing here needs a release build or a GitHub Release, so it runs on
 * an ordinary pull request.
 */

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { readArchive } from "./lib/archive.mjs";
import { archiveName, binaryName, parseChecksums, readCargoField } from "./lib/release-manifest.mjs";

const SCRIPTS = dirname(fileURLToPath(import.meta.url));
const ROOT = dirname(SCRIPTS);
const REPOSITORY = "nao1215/truss";
const FEATURES = "s3,gcs,azure";
const EXEC_TARGET = "x86_64-unknown-linux-gnu";

// The stand-in executable is a `#!/bin/sh` script, which Windows cannot run, so
// the checks that spawn it are skipped there. The rest of the suite still runs
// on Windows -- that is where the ZIP branch of the packing script lives, and
// on macOS it is the bsdtar branch, neither of which a Linux-only run reaches.
const CAN_RUN_SHELL_SCRIPT = process.platform !== "win32";
const EXEC_ARGS = CAN_RUN_SHELL_SCRIPT ? ["--exec-target", EXEC_TARGET] : [];

const targets = JSON.parse(readFileSync(join(ROOT, ".github/release-targets.json"), "utf8"));
const version = readCargoField(readFileSync(join(ROOT, "Cargo.toml"), "utf8"), "package", "version");
const tag = `v${version}`;

let workspace;
let distribution;

function run(script, args, options = {}) {
  return spawnSync(process.execPath, [join(SCRIPTS, script), ...args], {
    cwd: ROOT,
    encoding: "utf8",
    ...options,
  });
}

function pack(target, format, into, reportedVersion = version) {
  // The staged file has to be named exactly as it will appear in the archive,
  // so give each target its own directory rather than a decorated file name.
  const stage = mkdtempSync(join(workspace, "stage-"));
  const source = join(stage, binaryName(target));

  // A shell script stands in for the real executable: the tooling only needs
  // something that opens, has bytes, and answers `--version`.
  writeFileSync(source, `#!/bin/sh\necho "truss ${reportedVersion}"\n`);

  const archivePath = join(into, archiveName(tag, target, format));
  const result = spawnSync("bash", [join(SCRIPTS, "pack-release-archive.sh"), source, archivePath], {
    encoding: "utf8",
  });

  assert.equal(result.status, 0, `packing ${target} failed: ${result.stderr}`);
  return archivePath;
}

function generate(into) {
  return run("generate-release-manifest.mjs", [
    "--dir",
    into,
    "--tag",
    tag,
    "--repository",
    REPOSITORY,
    "--features",
    FEATURES,
  ]);
}

function verify(into, extra = []) {
  return run("verify-release-manifest.mjs", [
    "--dir",
    into,
    "--tag",
    tag,
    "--repository",
    REPOSITORY,
    ...extra,
  ]);
}

/** Copy the good release into a scratch directory a test may corrupt. */
function cloneDistribution() {
  const clone = mkdtempSync(join(workspace, "clone-"));

  for (const entry of targets) {
    const name = archiveName(tag, entry.target, entry.archive);
    copyFileSync(join(distribution, name), join(clone, name));
  }
  copyFileSync(join(distribution, "release-manifest.json"), join(clone, "release-manifest.json"));
  copyFileSync(join(distribution, "checksums.txt"), join(clone, "checksums.txt"));

  return clone;
}

function readManifest(from = distribution) {
  return JSON.parse(readFileSync(join(from, "release-manifest.json"), "utf8"));
}

describe("release distribution tooling", () => {
  before(() => {
    workspace = mkdtempSync(join(tmpdir(), "truss-release-e2e-"));
    distribution = join(workspace, "dist");
    mkdirSync(distribution);

    for (const entry of targets) {
      pack(entry.target, entry.archive, distribution);
    }

    const generated = generate(distribution);
    assert.equal(generated.status, 0, `manifest generation failed: ${generated.stderr}`);
  });

  after(() => {
    rmSync(workspace, { recursive: true, force: true });
  });

  test("packs one normalized executable per declared target", () => {
    for (const entry of targets) {
      const name = archiveName(tag, entry.target, entry.archive);
      const entries = readArchive(readFileSync(join(distribution, name)), entry.archive);

      assert.equal(entries.length, 1, `${name} should hold exactly one entry`);
      assert.equal(entries[0].name, binaryName(entry.target));
      assert.equal(entries[0].type, "file");

      if (entry.archive === "tar.gz") {
        assert.equal(entries[0].mode, 0o755);
        assert.equal(entries[0].uid, 0);
        assert.equal(entries[0].gid, 0);
        assert.equal(entries[0].uname, "");
        assert.equal(entries[0].gname, "");
      }
    }
  });

  test("packing the same binary twice produces byte-identical archives", () => {
    const first = mkdtempSync(join(workspace, "repeat-a-"));
    const second = mkdtempSync(join(workspace, "repeat-b-"));

    for (const entry of targets) {
      const a = readFileSync(pack(entry.target, entry.archive, first));
      const b = readFileSync(pack(entry.target, entry.archive, second));

      assert.ok(a.equals(b), `${entry.target} archive is not reproducible`);
    }
  });

  test("the manifest describes every declared target once, in target order", () => {
    const manifest = readManifest();

    assert.equal(manifest.schemaVersion, 1);
    assert.equal(manifest.name, "truss");
    assert.equal(manifest.version, version);
    assert.equal(manifest.tag, tag);
    assert.equal(manifest.repository, REPOSITORY);

    const listed = manifest.artifacts.map((artifact) => artifact.target);
    assert.deepEqual(listed, [...targets.map((entry) => entry.target)].sort());
    assert.equal(new Set(listed).size, listed.length);
  });

  test("the manifest records the features the release binaries are built with", () => {
    const manifest = readManifest();

    for (const artifact of manifest.artifacts) {
      // The workflow passes s3,gcs,azure on top of the default feature set;
      // the manifest reports the transitive closure a consumer actually gets.
      assert.deepEqual(artifact.features, [
        "avif",
        "azure",
        "cli",
        "gcs",
        "s3",
        "server",
        "svg",
        "webp-lossy",
      ]);
    }
  });

  test("the manifest points at the GitHub Release download URLs", () => {
    for (const artifact of readManifest().artifacts) {
      assert.equal(
        artifact.archive.url,
        `https://github.com/${REPOSITORY}/releases/download/${tag}/${artifact.archive.name}`,
      );
    }
  });

  test("generating the manifest twice from the same archives is byte-identical", () => {
    const clone = cloneDistribution();
    const before = readFileSync(join(clone, "release-manifest.json"), "utf8");

    assert.equal(generate(clone).status, 0);
    assert.equal(readFileSync(join(clone, "release-manifest.json"), "utf8"), before);
  });

  test("checksums.txt lists exactly the manifest archives with the same hashes", () => {
    const manifest = readManifest();
    const checksums = parseChecksums(readFileSync(join(distribution, "checksums.txt"), "utf8"));

    assert.equal(checksums.length, manifest.artifacts.length);
    for (const artifact of manifest.artifacts) {
      const entry = checksums.find((candidate) => candidate.name === artifact.archive.name);
      assert.ok(entry, `checksums.txt is missing ${artifact.archive.name}`);
      assert.equal(entry.sha256, artifact.archive.sha256);
    }
  });

  test("verification passes on the generated release", () => {
    const result = verify(distribution, EXEC_ARGS);

    assert.equal(result.status, 0, `${result.stdout}${result.stderr}`);
    assert.match(result.stdout, /ok$/m);
  });

  test("per-archive checks pass, and run the binary for executable targets", () => {
    for (const entry of targets) {
      const args = [
        "--archive",
        join(distribution, archiveName(tag, entry.target, entry.archive)),
        "--target",
        entry.target,
        "--tag",
        tag,
      ];
      if (entry.target === EXEC_TARGET && CAN_RUN_SHELL_SCRIPT) {
        args.push("--exec");
      }

      const result = run("check-release-archive.mjs", args);
      assert.equal(result.status, 0, `${entry.target}: ${result.stdout}${result.stderr}`);
    }
  });

  test("the executable target really is executed and its version compared", { skip: !CAN_RUN_SHELL_SCRIPT }, () => {
    const clone = cloneDistribution();
    pack(EXEC_TARGET, "tar.gz", clone, "0.0.1");
    assert.equal(generate(clone).status, 0);

    const result = verify(clone, ["--exec-target", EXEC_TARGET]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /reported "truss 0\.0\.1", expected version/);
  });

  test("verification fails when an archive no longer matches its manifest hash", () => {
    const clone = cloneDistribution();
    const path = join(clone, archiveName(tag, EXEC_TARGET, "tar.gz"));
    const bytes = readFileSync(path);

    bytes[bytes.length - 1] ^= 0xff;
    writeFileSync(path, bytes);

    const result = verify(clone);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /archive SHA-256 is/);
  });

  test("verification fails when the manifest executable hash is wrong", () => {
    const clone = cloneDistribution();
    const manifest = readManifest(clone);

    manifest.artifacts[0].binary.sha256 = "0".repeat(64);
    writeFileSync(join(clone, "release-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

    const result = verify(clone);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /executable SHA-256 is/);
  });

  test("verification fails when checksums.txt disagrees with the manifest", () => {
    const clone = cloneDistribution();
    const checksums = readFileSync(join(clone, "checksums.txt"), "utf8");

    writeFileSync(join(clone, "checksums.txt"), checksums.replace(/^[0-9a-f]{64}/m, "0".repeat(64)));

    const result = verify(clone);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /checksums\.txt hash for/);
  });

  test("verification fails when a distribution target is missing", () => {
    const clone = cloneDistribution();
    const manifest = readManifest(clone);
    const dropped = manifest.artifacts.pop();

    writeFileSync(join(clone, "release-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    rmSync(join(clone, dropped.archive.name));

    const result = verify(clone);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /manifest is missing target/);
  });

  test("verification fails when an archive named by the manifest is absent", () => {
    const clone = cloneDistribution();
    const manifest = readManifest(clone);

    rmSync(join(clone, manifest.artifacts[0].archive.name));

    const result = verify(clone);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /is missing from/);
  });

  test("manifest generation refuses a tag that does not match Cargo.toml", () => {
    const result = run("generate-release-manifest.mjs", [
      "--dir",
      distribution,
      "--tag",
      "v0.0.0",
      "--repository",
      REPOSITORY,
      "--features",
      FEATURES,
    ]);

    assert.equal(result.status, 1);
    assert.match(result.stderr, /does not match Cargo\.toml version/);
  });

  test("the Homebrew formula is rendered from the manifest", () => {
    const manifest = readManifest();
    const result = run("render-homebrew-formula.mjs", [
      tag,
      join(distribution, "release-manifest.json"),
    ]);

    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stdout.includes(`version "${version}"`));

    for (const target of [
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
      "x86_64-unknown-linux-gnu",
      "aarch64-unknown-linux-gnu",
    ]) {
      const artifact = manifest.artifacts.find((candidate) => candidate.target === target);
      assert.ok(result.stdout.includes(`sha256 "${artifact.archive.sha256}"`), target);
      assert.ok(result.stdout.includes(`url "${artifact.archive.url}"`), target);
    }
  });

  test("the Homebrew formula refuses a manifest for another tag", () => {
    const result = run("render-homebrew-formula.mjs", [
      "v0.0.0",
      join(distribution, "release-manifest.json"),
    ]);

    assert.equal(result.status, 1);
    assert.match(result.stderr, /manifest is for/);
  });
});
