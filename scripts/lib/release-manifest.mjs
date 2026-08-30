/**
 * Shared helpers for building and validating `release-manifest.json`.
 *
 * Everything in this module is pure: it takes strings and plain objects and
 * returns strings and plain objects. Filesystem access, archive extraction and
 * process execution live in the CLI wrappers so that this file stays unit
 * testable without a release build.
 */

/**
 * Manifest schema version.
 *
 * Bump this only for a breaking change to the document shape. Adding a new
 * optional field is not breaking: consumers are expected to ignore unknown
 * keys, which is why every group of related values is nested in its own object
 * rather than flattened into the artifact entry.
 */
export const SCHEMA_VERSION = 1;

/** Archive formats truss publishes. */
export const ARCHIVE_FORMATS = ["tar.gz", "zip"];

const SHA256_PATTERN = /^[0-9a-f]{64}$/;

/**
 * Split a Rust target triple into the fields consumers actually match on.
 *
 * Triples are `<arch>-<vendor>-<sys>[-<abi>]`. `os` and `arch` use the names a
 * caller is most likely to already have (Node's `process.platform` /
 * `process.arch` style names are close, but we keep the Rust spelling for
 * `arch` so it stays a lossless view of the triple).
 *
 * @param {string} target
 * @returns {{os: string, arch: string, environment: string | null}}
 */
export function parseTargetTriple(target) {
  if (typeof target !== "string" || target.trim() === "") {
    throw new Error("target triple must be a non-empty string");
  }

  const parts = target.split("-");
  if (parts.length < 3) {
    throw new Error(`unsupported target triple: ${target}`);
  }

  const [arch, , sys, ...rest] = parts;
  const abi = rest.length > 0 ? rest.join("-") : null;

  let os;
  switch (sys) {
    case "linux":
      os = "linux";
      break;
    case "darwin":
      os = "macos";
      break;
    case "windows":
      os = "windows";
      break;
    default:
      throw new Error(`unsupported target triple: ${target}`);
  }

  // Linux spells the libc in the fourth component (`gnu`, `musl`); Windows
  // spells the toolchain there (`msvc`, `gnu`); macOS has no fourth component.
  return { os, arch, environment: abi };
}

/**
 * Read a single-line `key = "value"` field out of a `Cargo.toml` section.
 *
 * @param {string} cargoToml
 * @param {string} section e.g. `package`
 * @param {string} key e.g. `version`
 * @returns {string}
 */
export function readCargoField(cargoToml, section, key) {
  const body = sectionBody(cargoToml, section);
  if (body === null) {
    throw new Error(`[${section}] section not found in Cargo.toml`);
  }

  for (const line of body) {
    const match = line.match(/^([A-Za-z0-9_-]+)\s*=\s*"([^"]*)"/);
    if (match && match[1] === key) {
      return match[2];
    }
  }

  throw new Error(`${key} not found in [${section}] of Cargo.toml`);
}

/**
 * Parse `[features]` into a map of feature name to the feature names it enables.
 *
 * `dep:foo` and `crate/feature` entries are dropped: they name Cargo
 * dependencies, not features a consumer can ask about.
 *
 * @param {string} cargoToml
 * @returns {Map<string, string[]>}
 */
export function parseCargoFeatures(cargoToml) {
  const body = sectionBody(cargoToml, "features");
  if (body === null) {
    return new Map();
  }

  const features = new Map();
  for (const line of body) {
    const match = line.match(/^([A-Za-z0-9_-]+)\s*=\s*\[(.*)\]\s*$/);
    if (!match) {
      continue;
    }

    const enabled = [...match[2].matchAll(/"([^"]+)"/g)]
      .map((entry) => entry[1])
      .filter((entry) => !entry.includes(":") && !entry.includes("/"));

    features.set(match[1], enabled);
  }

  return features;
}

/**
 * Expand a cargo feature selection into the full set that ends up compiled in.
 *
 * `["s3"]` on top of the default features resolves through `s3 -> server ->
 * svg` so that the manifest records what the binary can actually do rather than
 * the flags the workflow happened to pass.
 *
 * @param {Map<string, string[]>} featureMap
 * @param {string[]} requested
 * @param {{withDefault?: boolean}} [options]
 * @returns {string[]} sorted, de-duplicated feature names
 */
export function resolveFeatures(featureMap, requested, options = {}) {
  const withDefault = options.withDefault ?? true;
  const seen = new Set();
  const resolved = new Set();
  const queue = [...requested];

  if (withDefault) {
    queue.push("default");
  }

  while (queue.length > 0) {
    const feature = queue.pop();
    if (seen.has(feature)) {
      continue;
    }
    seen.add(feature);

    // `default` is a cargo implementation detail, not a capability: it is
    // walked for the features it turns on but never reported as one.
    if (feature !== "default") {
      resolved.add(feature);
    }

    for (const child of featureMap.get(feature) ?? []) {
      if (!seen.has(child)) {
        queue.push(child);
      }
    }
  }

  return [...resolved].sort();
}

/**
 * Archive file name for a target. Keep in sync with the release workflow.
 *
 * @param {string} tag e.g. `v0.15.0`
 * @param {string} target
 * @param {string} format one of {@link ARCHIVE_FORMATS}
 */
export function archiveName(tag, target, format) {
  if (!ARCHIVE_FORMATS.includes(format)) {
    throw new Error(`unsupported archive format: ${format}`);
  }
  return `truss-${tag}-${target}.${format}`;
}

/**
 * Public download URL of a GitHub release asset.
 *
 * @param {string} repository `owner/name`
 * @param {string} tag
 * @param {string} assetName
 */
export function downloadUrl(repository, tag, assetName) {
  return `https://github.com/${repository}/releases/download/${tag}/${assetName}`;
}

/**
 * Name of the truss executable inside an archive for a target.
 *
 * @param {string} target
 */
export function binaryName(target) {
  return parseTargetTriple(target).os === "windows" ? "truss.exe" : "truss";
}

/**
 * Build the manifest document.
 *
 * Artifacts are sorted by target triple so the same inputs always produce a
 * byte-identical document.
 *
 * @param {object} input
 * @param {string} input.version crate version without the leading `v`
 * @param {string} input.tag release tag
 * @param {string} input.repository `owner/name`
 * @param {string[]} input.features resolved cargo features every binary is built with
 * @param {Array<{target: string, format: string, archiveSha256: string, archiveSize: number, binarySha256: string, binarySize: number}>} input.artifacts
 * @returns {object}
 */
export function buildManifest(input) {
  const { version, tag, repository, features, artifacts } = input;

  const entries = [...artifacts]
    .sort((a, b) => (a.target < b.target ? -1 : a.target > b.target ? 1 : 0))
    .map((artifact) => {
      const { os, arch, environment } = parseTargetTriple(artifact.target);
      const name = archiveName(tag, artifact.target, artifact.format);

      return {
        target: artifact.target,
        os,
        arch,
        environment,
        archive: {
          name,
          format: artifact.format,
          url: downloadUrl(repository, tag, name),
          sha256: artifact.archiveSha256,
          size: artifact.archiveSize,
        },
        binary: {
          path: binaryName(artifact.target),
          sha256: artifact.binarySha256,
          size: artifact.binarySize,
        },
        features: [...features].sort(),
      };
    });

  return {
    schemaVersion: SCHEMA_VERSION,
    name: "truss",
    version,
    tag,
    repository,
    artifacts: entries,
  };
}

/**
 * Render `checksums.txt` from the manifest.
 *
 * The manifest is the single place archive hashes are computed; this keeps the
 * two files from disagreeing by construction. The format matches `sha256sum`
 * output (hash, two spaces, file name) so `sha256sum -c checksums.txt` works.
 *
 * @param {object} manifest
 * @returns {string}
 */
export function renderChecksums(manifest) {
  return manifest.artifacts
    .map((artifact) => `${artifact.archive.sha256}  ${artifact.archive.name}`)
    .sort()
    .join("\n")
    .concat("\n");
}

/**
 * Parse `sha256sum`-style lines.
 *
 * @param {string} text
 * @returns {Array<{sha256: string, name: string}>}
 */
export function parseChecksums(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "")
    .map((line) => {
      const match = line.match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
      if (!match) {
        throw new Error(`malformed checksum line: ${line}`);
      }
      return { sha256: match[1].toLowerCase(), name: match[2] };
    });
}

/**
 * Validate everything about a manifest that can be checked without the files.
 *
 * Returns a list of human-readable problems; an empty list means the manifest
 * is internally consistent, covers exactly the expected targets, and agrees
 * with `checksums.txt`.
 *
 * @param {object} manifest
 * @param {object} expectations
 * @param {string} expectations.tag
 * @param {string} expectations.repository
 * @param {string[]} expectations.targets every target the release must ship
 * @param {string} [expectations.checksumsText]
 * @returns {string[]}
 */
export function checkManifest(manifest, expectations) {
  const problems = [];
  const { tag, repository, targets, checksumsText } = expectations;

  if (manifest.schemaVersion !== SCHEMA_VERSION) {
    problems.push(
      `schemaVersion is ${manifest.schemaVersion}, expected ${SCHEMA_VERSION}`,
    );
  }

  if (manifest.tag !== tag) {
    problems.push(`manifest tag is ${manifest.tag}, expected ${tag}`);
  }

  const expectedVersion = tag.startsWith("v") ? tag.slice(1) : tag;
  if (manifest.version !== expectedVersion) {
    problems.push(
      `manifest version is ${manifest.version}, expected ${expectedVersion}`,
    );
  }

  if (manifest.repository !== repository) {
    problems.push(
      `manifest repository is ${manifest.repository}, expected ${repository}`,
    );
  }

  if (!Array.isArray(manifest.artifacts) || manifest.artifacts.length === 0) {
    problems.push("manifest has no artifacts");
    return problems;
  }

  const seenTargets = new Set();
  const seenNames = new Set();
  const seenUrls = new Set();

  for (const artifact of manifest.artifacts) {
    const label = artifact.target ?? "<missing target>";

    if (seenTargets.has(artifact.target)) {
      problems.push(`duplicate target: ${label}`);
    }
    seenTargets.add(artifact.target);

    if (seenNames.has(artifact.archive?.name)) {
      problems.push(`duplicate archive name: ${artifact.archive?.name}`);
    }
    seenNames.add(artifact.archive?.name);

    if (seenUrls.has(artifact.archive?.url)) {
      problems.push(`duplicate download URL: ${artifact.archive?.url}`);
    }
    seenUrls.add(artifact.archive?.url);

    for (const [field, value] of [
      ["archive.sha256", artifact.archive?.sha256],
      ["binary.sha256", artifact.binary?.sha256],
    ]) {
      if (typeof value !== "string" || !SHA256_PATTERN.test(value)) {
        problems.push(`${label}: ${field} is not a lowercase SHA-256 digest`);
      }
    }

    for (const [field, value] of [
      ["archive.size", artifact.archive?.size],
      ["binary.size", artifact.binary?.size],
    ]) {
      if (!Number.isInteger(value) || value <= 0) {
        problems.push(`${label}: ${field} is not a positive integer`);
      }
    }

    const expectedName = artifact.archive?.format
      ? archiveName(tag, artifact.target, artifact.archive.format)
      : null;
    if (expectedName !== null && artifact.archive?.name !== expectedName) {
      problems.push(
        `${label}: archive.name is ${artifact.archive?.name}, expected ${expectedName}`,
      );
    }

    const expectedUrl = downloadUrl(repository, tag, artifact.archive?.name);
    if (artifact.archive?.url !== expectedUrl) {
      problems.push(
        `${label}: archive.url is ${artifact.archive?.url}, expected ${expectedUrl}`,
      );
    }

    if (artifact.binary?.path !== binaryName(artifact.target)) {
      problems.push(
        `${label}: binary.path is ${artifact.binary?.path}, expected ${binaryName(artifact.target)}`,
      );
    }

    if (!Array.isArray(artifact.features) || artifact.features.length === 0) {
      problems.push(`${label}: features is empty`);
    }
  }

  for (const target of targets) {
    if (!seenTargets.has(target)) {
      problems.push(`manifest is missing target: ${target}`);
    }
  }
  for (const target of seenTargets) {
    if (!targets.includes(target)) {
      problems.push(`manifest has unexpected target: ${target}`);
    }
  }

  if (checksumsText !== undefined) {
    const expected = renderChecksums(manifest);
    const actual = parseChecksums(checksumsText);
    const expectedEntries = parseChecksums(expected);

    for (const entry of expectedEntries) {
      const match = actual.find((candidate) => candidate.name === entry.name);
      if (match === undefined) {
        problems.push(`checksums.txt is missing ${entry.name}`);
      } else if (match.sha256 !== entry.sha256) {
        problems.push(
          `checksums.txt hash for ${entry.name} is ${match.sha256}, manifest says ${entry.sha256}`,
        );
      }
    }

    for (const entry of actual) {
      if (!expectedEntries.some((candidate) => candidate.name === entry.name)) {
        problems.push(`checksums.txt has an entry not in the manifest: ${entry.name}`);
      }
    }
  }

  return problems;
}

/**
 * Compare two dotted version strings.
 *
 * @param {string} a
 * @param {string} b
 * @returns {number} negative when `a < b`, zero when equal, positive otherwise
 */
export function compareVersions(a, b) {
  const left = a.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const right = b.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const length = Math.max(left.length, right.length);

  for (let index = 0; index < length; index += 1) {
    const diff = (left[index] ?? 0) - (right[index] ?? 0);
    if (diff !== 0) {
      return diff;
    }
  }

  return 0;
}

function sectionBody(cargoToml, section) {
  const lines = cargoToml.split("\n");
  const header = `[${section}]`;
  const start = lines.findIndex((line) => line.trim() === header);
  if (start === -1) {
    return null;
  }

  const body = [];
  for (let index = start + 1; index < lines.length; index += 1) {
    const line = lines[index].trim();
    if (line.startsWith("[")) {
      break;
    }
    if (line === "" || line.startsWith("#")) {
      continue;
    }
    body.push(line);
  }

  return body;
}

/**
 * Validate the shape of an extracted archive listing.
 *
 * A distribution archive must hold exactly one entry: the executable, at the
 * archive root. Directory entries, build-tree prefixes, stray debug files and
 * runner-owned uid/gid are all rejected here rather than being discovered by a
 * user running `tar x` as root.
 *
 * ZIP written by 7-Zip on Windows records no Unix mode at all (`mode` is
 * null); that is expected for the Windows target and only the entry name and
 * layout are asserted there.
 *
 * @param {import("./archive.mjs").ArchiveEntry[]} entries
 * @param {{target: string}} expectations
 * @returns {string[]} human-readable problems, empty when the layout is correct
 */
export function checkArchiveLayout(entries, expectations) {
  const problems = [];
  const expectedName = binaryName(expectations.target);
  const isWindows = parseTargetTriple(expectations.target).os === "windows";

  if (entries.length !== 1) {
    problems.push(
      `archive holds ${entries.length} entries (${entries
        .map((entry) => entry.name)
        .join(", ")}), expected exactly ${expectedName}`,
    );
    return problems;
  }

  const [entry] = entries;

  if (entry.name !== expectedName) {
    problems.push(`archive entry is ${entry.name}, expected ${expectedName}`);
  }

  if (entry.type !== "file") {
    problems.push(`archive entry ${entry.name} is a ${entry.type}, expected a regular file`);
  }

  if (entry.size <= 0) {
    problems.push(`archive entry ${entry.name} is empty`);
  }

  if (entry.mode !== null && (entry.mode & 0o111) === 0) {
    problems.push(
      `archive entry ${entry.name} has mode ${entry.mode.toString(8)}, which is not executable`,
    );
  }

  if (!isWindows) {
    if (entry.mode !== 0o755) {
      problems.push(
        `archive entry ${entry.name} has mode ${entry.mode === null ? "none" : entry.mode.toString(8)}, expected 755`,
      );
    }
    if (entry.uid !== 0 || entry.gid !== 0) {
      problems.push(
        `archive entry ${entry.name} is owned by ${entry.uid}:${entry.gid}, expected 0:0`,
      );
    }
    if ((entry.uname ?? "") !== "" || (entry.gname ?? "") !== "") {
      problems.push(
        `archive entry ${entry.name} carries owner names ${entry.uname}:${entry.gname}, expected none`,
      );
    }
  }

  return problems;
}
