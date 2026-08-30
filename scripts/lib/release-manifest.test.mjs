import assert from "node:assert/strict";
import { test } from "node:test";

import {
  SCHEMA_VERSION,
  archiveName,
  binaryName,
  buildManifest,
  checkArchiveLayout,
  checkManifest,
  compareVersions,
  downloadUrl,
  parseCargoFeatures,
  parseChecksums,
  parseTargetTriple,
  readCargoField,
  renderChecksums,
  resolveFeatures,
} from "./release-manifest.mjs";

const TAG = "v1.2.3";
const REPOSITORY = "nao1215/truss";
const TARGETS = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"];

const CARGO_TOML = `[package]
name = "truss-image"
version = "1.2.3"
edition = "2024"
rust-version = "1.92"

[features]
default = ["cli", "svg"]
cli = ["dep:clap", "server"]
server = ["dep:hmac", "svg"]
svg = ["dep:quick-xml"]
s3 = ["server", "dep:aws-sdk-s3"]
wasm = ["dep:wasm-bindgen"]

[dependencies]
clap = { version = "4", optional = true }
`;

function artifactInput(target, format, overrides = {}) {
  return {
    target,
    format,
    archiveSha256: "a".repeat(64),
    archiveSize: 2_000_000,
    binarySha256: "b".repeat(64),
    binarySize: 6_000_000,
    ...overrides,
  };
}

function sampleManifest(overrides = {}) {
  return buildManifest({
    version: "1.2.3",
    tag: TAG,
    repository: REPOSITORY,
    features: ["cli", "server"],
    artifacts: [
      artifactInput("x86_64-unknown-linux-gnu", "tar.gz"),
      artifactInput("x86_64-pc-windows-msvc", "zip", {
        archiveSha256: "c".repeat(64),
        binarySha256: "d".repeat(64),
      }),
    ],
    ...overrides,
  });
}

test("parseTargetTriple maps the triples truss ships", () => {
  assert.deepEqual(parseTargetTriple("x86_64-unknown-linux-gnu"), {
    os: "linux",
    arch: "x86_64",
    environment: "gnu",
  });
  assert.deepEqual(parseTargetTriple("aarch64-apple-darwin"), {
    os: "macos",
    arch: "aarch64",
    environment: null,
  });
  assert.deepEqual(parseTargetTriple("x86_64-pc-windows-msvc"), {
    os: "windows",
    arch: "x86_64",
    environment: "msvc",
  });
});

test("parseTargetTriple rejects input it cannot describe", () => {
  assert.throws(() => parseTargetTriple("wasm32-unknown-unknown"), /unsupported target triple/);
  assert.throws(() => parseTargetTriple("nonsense"), /unsupported target triple/);
  assert.throws(() => parseTargetTriple(""), /non-empty string/);
});

test("readCargoField reads the package section", () => {
  assert.equal(readCargoField(CARGO_TOML, "package", "version"), "1.2.3");
  assert.equal(readCargoField(CARGO_TOML, "package", "rust-version"), "1.92");
  assert.throws(() => readCargoField(CARGO_TOML, "package", "missing"), /not found/);
  assert.throws(() => readCargoField(CARGO_TOML, "absent", "version"), /section not found/);
});

test("parseCargoFeatures keeps feature names and drops dependency entries", () => {
  const features = parseCargoFeatures(CARGO_TOML);

  assert.deepEqual(features.get("default"), ["cli", "svg"]);
  assert.deepEqual(features.get("cli"), ["server"]);
  assert.deepEqual(features.get("server"), ["svg"]);
  assert.deepEqual(features.get("wasm"), []);
  assert.equal(features.has("clap"), false);
});

test("resolveFeatures expands defaults transitively and never reports 'default'", () => {
  const features = parseCargoFeatures(CARGO_TOML);

  assert.deepEqual(resolveFeatures(features, []), ["cli", "server", "svg"]);
  assert.deepEqual(resolveFeatures(features, ["s3"]), ["cli", "s3", "server", "svg"]);
  assert.deepEqual(resolveFeatures(features, ["wasm"], { withDefault: false }), ["wasm"]);
});

test("resolveFeatures is stable regardless of the order features are requested", () => {
  const features = parseCargoFeatures(CARGO_TOML);

  assert.deepEqual(resolveFeatures(features, ["s3", "wasm"]), resolveFeatures(features, ["wasm", "s3"]));
});

test("archiveName, downloadUrl and binaryName agree with the workflow", () => {
  assert.equal(
    archiveName(TAG, "x86_64-unknown-linux-gnu", "tar.gz"),
    "truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
  );
  assert.equal(archiveName(TAG, "x86_64-pc-windows-msvc", "zip"), "truss-v1.2.3-x86_64-pc-windows-msvc.zip");
  assert.throws(() => archiveName(TAG, "x86_64-unknown-linux-gnu", "7z"), /unsupported archive format/);

  assert.equal(
    downloadUrl(REPOSITORY, TAG, "truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"),
    "https://github.com/nao1215/truss/releases/download/v1.2.3/truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
  );

  assert.equal(binaryName("x86_64-unknown-linux-gnu"), "truss");
  assert.equal(binaryName("x86_64-pc-windows-msvc"), "truss.exe");
});

test("buildManifest records every field a consumer needs", () => {
  const manifest = sampleManifest();

  assert.equal(manifest.schemaVersion, SCHEMA_VERSION);
  assert.equal(manifest.version, "1.2.3");
  assert.equal(manifest.tag, TAG);
  assert.equal(manifest.repository, REPOSITORY);

  const linux = manifest.artifacts.find(
    (artifact) => artifact.target === "x86_64-unknown-linux-gnu",
  );
  assert.deepEqual(linux, {
    target: "x86_64-unknown-linux-gnu",
    os: "linux",
    arch: "x86_64",
    environment: "gnu",
    archive: {
      name: "truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
      format: "tar.gz",
      url: "https://github.com/nao1215/truss/releases/download/v1.2.3/truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
      sha256: "a".repeat(64),
      size: 2_000_000,
    },
    binary: {
      path: "truss",
      sha256: "b".repeat(64),
      size: 6_000_000,
    },
    features: ["cli", "server"],
  });
});

test("buildManifest orders artifacts by target triple regardless of input order", () => {
  const forward = buildManifest({
    version: "1.2.3",
    tag: TAG,
    repository: REPOSITORY,
    features: ["cli"],
    artifacts: [
      artifactInput("x86_64-unknown-linux-gnu", "tar.gz"),
      artifactInput("aarch64-apple-darwin", "tar.gz"),
    ],
  });
  const reversed = buildManifest({
    version: "1.2.3",
    tag: TAG,
    repository: REPOSITORY,
    features: ["cli"],
    artifacts: [
      artifactInput("aarch64-apple-darwin", "tar.gz"),
      artifactInput("x86_64-unknown-linux-gnu", "tar.gz"),
    ],
  });

  assert.deepEqual(
    forward.artifacts.map((artifact) => artifact.target),
    ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"],
  );
  assert.equal(JSON.stringify(forward), JSON.stringify(reversed));
});

test("renderChecksums and parseChecksums round-trip in sha256sum format", () => {
  const manifest = sampleManifest();
  const text = renderChecksums(manifest);

  assert.equal(
    text,
    `${"a".repeat(64)}  truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz\n${"c".repeat(64)}  truss-v1.2.3-x86_64-pc-windows-msvc.zip\n`,
  );
  assert.deepEqual(parseChecksums(text), [
    { sha256: "a".repeat(64), name: "truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz" },
    { sha256: "c".repeat(64), name: "truss-v1.2.3-x86_64-pc-windows-msvc.zip" },
  ]);
});

test("parseChecksums tolerates the binary-mode star and rejects junk", () => {
  assert.deepEqual(parseChecksums(`${"a".repeat(64)} *truss.zip\n`), [
    { sha256: "a".repeat(64), name: "truss.zip" },
  ]);
  assert.throws(() => parseChecksums("not-a-hash truss.zip\n"), /malformed checksum line/);
});

test("checkManifest accepts a manifest that matches its checksums file", () => {
  const manifest = sampleManifest();

  assert.deepEqual(
    checkManifest(manifest, {
      tag: TAG,
      repository: REPOSITORY,
      targets: TARGETS,
      checksumsText: renderChecksums(manifest),
    }),
    [],
  );
});

test("checkManifest reports a missing distribution target", () => {
  const manifest = sampleManifest();
  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: [...TARGETS, "aarch64-apple-darwin"],
  });

  assert.deepEqual(problems, ["manifest is missing target: aarch64-apple-darwin"]);
});

test("checkManifest reports an artifact that is not a declared target", () => {
  const manifest = sampleManifest();
  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: ["x86_64-unknown-linux-gnu"],
  });

  assert.deepEqual(problems, ["manifest has unexpected target: x86_64-pc-windows-msvc"]);
});

test("checkManifest reports duplicate targets, names and URLs", () => {
  const manifest = sampleManifest();
  manifest.artifacts.push(structuredClone(manifest.artifacts[0]));

  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: TARGETS,
  });

  assert.ok(problems.includes("duplicate target: x86_64-pc-windows-msvc"));
  assert.ok(problems.includes("duplicate archive name: truss-v1.2.3-x86_64-pc-windows-msvc.zip"));
  assert.ok(
    problems.includes(
      "duplicate download URL: https://github.com/nao1215/truss/releases/download/v1.2.3/truss-v1.2.3-x86_64-pc-windows-msvc.zip",
    ),
  );
});

test("checkManifest reports a checksums.txt that disagrees with the manifest", () => {
  const manifest = sampleManifest();
  const tampered = renderChecksums(manifest).replace("a".repeat(64), "e".repeat(64));

  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: TARGETS,
    checksumsText: tampered,
  });

  assert.deepEqual(problems, [
    `checksums.txt hash for truss-v1.2.3-x86_64-unknown-linux-gnu.tar.gz is ${"e".repeat(64)}, manifest says ${"a".repeat(64)}`,
  ]);
});

test("checkManifest reports checksums.txt entries with no manifest artifact", () => {
  const manifest = sampleManifest();
  const extra = `${renderChecksums(manifest)}${"f".repeat(64)}  truss-v1.2.3-extra.tar.gz\n`;

  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: TARGETS,
    checksumsText: extra,
  });

  assert.deepEqual(problems, [
    "checksums.txt has an entry not in the manifest: truss-v1.2.3-extra.tar.gz",
  ]);
});

test("checkManifest rejects wrong tag, version, repository and schema version", () => {
  const manifest = sampleManifest();
  manifest.schemaVersion = 99;
  manifest.tag = "v9.9.9";
  manifest.repository = "someone/else";

  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: TARGETS,
  });

  assert.ok(problems.some((problem) => problem.includes("schemaVersion is 99")));
  assert.ok(problems.some((problem) => problem.includes("manifest tag is v9.9.9")));
  assert.ok(problems.some((problem) => problem.includes("manifest repository is someone/else")));
});

test("checkManifest rejects malformed hashes, sizes and paths", () => {
  const manifest = sampleManifest();
  manifest.artifacts[0].archive.sha256 = "NOTAHASH";
  manifest.artifacts[0].archive.size = 0;
  manifest.artifacts[0].binary.path = "bin/truss";
  manifest.artifacts[1].archive.url = "https://example.com/evil.zip";
  manifest.artifacts[1].features = [];

  const problems = checkManifest(manifest, {
    tag: TAG,
    repository: REPOSITORY,
    targets: TARGETS,
  });

  assert.ok(
    problems.some((problem) =>
      problem.includes("archive.sha256 is not a lowercase SHA-256 digest"),
    ),
  );
  assert.ok(problems.some((problem) => problem.includes("archive.size is not a positive integer")));
  assert.ok(problems.some((problem) => problem.includes("binary.path is bin/truss")));
  assert.ok(problems.some((problem) => problem.includes("archive.url is https://example.com/evil.zip")));
  assert.ok(problems.some((problem) => problem.includes("features is empty")));
});

test("checkManifest rejects an empty artifact list", () => {
  const manifest = sampleManifest();
  manifest.artifacts = [];

  assert.deepEqual(
    checkManifest(manifest, { tag: TAG, repository: REPOSITORY, targets: TARGETS }),
    ["manifest has no artifacts"],
  );
});

test("checkArchiveLayout accepts a normalized unix archive", () => {
  assert.deepEqual(
    checkArchiveLayout(
      [
        {
          name: "truss",
          type: "file",
          mode: 0o755,
          uid: 0,
          gid: 0,
          uname: "",
          gname: "",
          size: 10,
          data: Buffer.alloc(10),
        },
      ],
      { target: "x86_64-unknown-linux-gnu" },
    ),
    [],
  );
});

test("checkArchiveLayout accepts a windows zip with no unix mode", () => {
  assert.deepEqual(
    checkArchiveLayout(
      [
        {
          name: "truss.exe",
          type: "file",
          mode: null,
          uid: null,
          gid: null,
          uname: null,
          gname: null,
          size: 10,
          data: Buffer.alloc(10),
        },
      ],
      { target: "x86_64-pc-windows-msvc" },
    ),
    [],
  );
});

test("checkArchiveLayout rejects extra entries, directories and build-tree paths", () => {
  const entry = (name, overrides = {}) => ({
    name,
    type: "file",
    mode: 0o755,
    uid: 0,
    gid: 0,
    uname: "",
    gname: "",
    size: 10,
    data: Buffer.alloc(10),
    ...overrides,
  });

  const extra = checkArchiveLayout([entry("truss"), entry("README.md")], {
    target: "x86_64-unknown-linux-gnu",
  });
  assert.match(extra[0], /archive holds 2 entries/);

  const directory = checkArchiveLayout([entry("truss/", { type: "directory" })], {
    target: "x86_64-unknown-linux-gnu",
  });
  assert.ok(directory.some((problem) => problem.includes("expected truss")));
  assert.ok(directory.some((problem) => problem.includes("is a directory")));

  const nested = checkArchiveLayout([entry("target/release/truss")], {
    target: "x86_64-unknown-linux-gnu",
  });
  assert.deepEqual(nested, ["archive entry is target/release/truss, expected truss"]);
});

test("checkArchiveLayout rejects runner ownership and a non-executable mode", () => {
  const problems = checkArchiveLayout(
    [
      {
        name: "truss",
        type: "file",
        mode: 0o644,
        uid: 1001,
        gid: 127,
        uname: "runner",
        gname: "docker",
        size: 10,
        data: Buffer.alloc(10),
      },
    ],
    { target: "x86_64-unknown-linux-gnu" },
  );

  assert.ok(problems.some((problem) => problem.includes("which is not executable")));
  assert.ok(problems.some((problem) => problem.includes("owned by 1001:127, expected 0:0")));
  assert.ok(problems.some((problem) => problem.includes("owner names runner:docker")));
});

test("checkArchiveLayout rejects an empty executable", () => {
  const problems = checkArchiveLayout(
    [
      {
        name: "truss",
        type: "file",
        mode: 0o755,
        uid: 0,
        gid: 0,
        uname: "",
        gname: "",
        size: 0,
        data: Buffer.alloc(0),
      },
    ],
    { target: "x86_64-unknown-linux-gnu" },
  );

  assert.deepEqual(problems, ["archive entry truss is empty"]);
});

test("compareVersions orders release versions", () => {
  assert.ok(compareVersions("1.92.0", "1.92") === 0);
  assert.ok(compareVersions("1.94.1", "1.92") > 0);
  assert.ok(compareVersions("1.91.0", "1.92") < 0);
  assert.ok(compareVersions("2.0.0", "1.99.99") > 0);
});
