/**
 * Locating the copies of the release version number that live outside `Cargo.toml`.
 *
 * The crate version is written in eight places across four files, and a release edits every
 * one of them by hand. Seven of the eight are addressed here by where they sit in their
 * document rather than by what they currently say, so that an edit which moves lines, or an
 * unrelated example that happens to contain the same digits, cannot make the check pass or
 * fail for the wrong reason.
 *
 * Everything in this module is pure: it takes document text and returns values and problems.
 * Reading the files is the CLI wrapper's job.
 */

/**
 * The YAML paths in `docs/openapi.yaml` that carry the crate version.
 *
 * Each is a list of keys rather than a dotted string because two of the segments contain a
 * dot-free but slash-bearing name (`/health`, `application/json`) and one is the quoted
 * status code `'200'`.
 */
export const OPENAPI_VERSION_PATHS = [
  ["info", "version"],
  [
    "paths",
    "/health",
    "get",
    "responses",
    "200",
    "content",
    "application/json",
    "example",
    "version",
  ],
  [
    "paths",
    "/health/live",
    "get",
    "responses",
    "200",
    "content",
    "application/json",
    "example",
    "version",
  ],
  ["components", "schemas", "LivenessResponse", "properties", "version", "example"],
  ["components", "schemas", "HealthDiagnosticResponse", "properties", "version", "example"],
];

/**
 * Read the scalar at one path of a block-style YAML mapping.
 *
 * This is not a YAML parser and does not try to be one. It walks the indentation of the
 * mapping keys, skips comments, skips the bodies of block scalars (`|` and `>`), and does not
 * descend into sequences, which is enough for the five values this repository addresses and
 * fails loudly rather than guessing on anything else. A path that is absent, that is not a
 * scalar, or that matches more than once is reported by the caller as a problem, so a
 * restructured document is a failure rather than a silent pass.
 *
 * @param {string} text YAML document
 * @param {string[]} path mapping keys from the document root
 * @returns {{value: string} | {error: string}}
 */
export function readYamlScalar(text, path) {
  const wanted = path.join(" > ");
  const matches = [];
  /** @type {{indent: number, key: string}[]} */
  const stack = [];
  let blockScalarIndent = null;

  const lines = text.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (line.trim() === "" || /^\s*#/.test(line)) {
      continue;
    }

    const indent = line.length - line.trimStart().length;

    if (blockScalarIndent !== null) {
      if (indent > blockScalarIndent) {
        continue;
      }
      blockScalarIndent = null;
    }

    const content = line.slice(indent);
    if (content.startsWith("-")) {
      // A sequence item. Nothing addressed here lives inside one, and descending would need
      // an index in the path, so the item and its children are left alone.
      continue;
    }

    const entry = splitMappingEntry(content);
    if (entry === null) {
      continue;
    }

    const { key, rest } = entry;

    while (stack.length > 0 && stack[stack.length - 1].indent >= indent) {
      stack.pop();
    }
    stack.push({ indent, key });

    if (rest === "|" || rest === ">" || /^[|>][-+\d]*$/.test(rest)) {
      blockScalarIndent = indent;
      continue;
    }
    if (rest === "" || rest.startsWith("#")) {
      continue;
    }

    const here = stack.map((entry) => entry.key);
    if (here.length === path.length && here.every((entry, at) => entry === path[at])) {
      matches.push({ line: index + 1, value: unquoteScalar(rest) });
    }
  }

  if (matches.length === 0) {
    return { error: `${wanted} is not in the document` };
  }
  if (matches.length > 1) {
    const where = matches.map((entry) => entry.line).join(", ");
    return { error: `${wanted} appears more than once, at lines ${where}` };
  }
  return { value: matches[0].value };
}

/**
 * Split one line of a block mapping into its key and the rest of the line.
 *
 * A key ends at the first `: ` or at a `:` that ends the line, which is what YAML itself
 * says and is why a route named `/images:transform` reads as one key rather than two.
 * Returns `null` for a line that is not a mapping entry.
 *
 * @param {string} content the line with its indentation removed
 * @returns {{key: string, rest: string} | null}
 */
function splitMappingEntry(content) {
  const quoted = /^(?:"([^"]*)"|'([^']*)')\s*:(?:\s+(.*))?$/.exec(content);
  if (quoted) {
    return { key: quoted[1] ?? quoted[2], rest: (quoted[3] ?? "").trim() };
  }

  const separator = content.indexOf(": ");
  if (separator >= 0) {
    return { key: content.slice(0, separator).trim(), rest: content.slice(separator + 2).trim() };
  }
  if (content.endsWith(":")) {
    return { key: content.slice(0, -1).trim(), rest: "" };
  }
  return null;
}

/**
 * Strip surrounding quotes and a trailing comment from a scalar.
 *
 * @param {string} scalar
 * @returns {string}
 */
function unquoteScalar(scalar) {
  const quoted = /^"([^"]*)"|^'([^']*)'/.exec(scalar);
  if (quoted) {
    return quoted[1] ?? quoted[2];
  }
  return scalar.replace(/\s+#.*$/, "").trim();
}

/**
 * Read the crate version out of `Cargo.toml`.
 *
 * The `[package]` table is matched explicitly so that a `version` in a dependency table
 * cannot be read as the crate's.
 *
 * @param {string} text
 * @returns {string}
 */
export function readCrateVersion(text) {
  const match = /^\[package\]$[\s\S]*?^version\s*=\s*"([^"]+)"/m.exec(text);
  if (!match) {
    throw new Error("Cargo.toml has no version in its [package] table");
  }
  return match[1];
}

/**
 * Compare every copy of the version against the crate's.
 *
 * @param {string} crateVersion
 * @param {{label: string, read: () => {value: string} | {error: string}}[]} copies
 * @returns {string[]} problems, empty when every copy agrees
 */
export function collectVersionProblems(crateVersion, copies) {
  const problems = [];

  for (const copy of copies) {
    const result = copy.read();
    if ("error" in result) {
      problems.push(`${copy.label}: ${result.error}`);
      continue;
    }
    if (result.value !== crateVersion) {
      problems.push(`${copy.label} is ${result.value}, expected ${crateVersion}`);
    }
  }

  return problems;
}
