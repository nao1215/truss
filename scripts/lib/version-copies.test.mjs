import assert from "node:assert/strict";
import { test } from "node:test";

import {
  OPENAPI_VERSION_PATHS,
  collectChangelogProblems,
  collectVersionProblems,
  readCrateVersion,
  readYamlScalar,
} from "./version-copies.mjs";

const DOCUMENT = [
  "openapi: 3.0.3",
  "info:",
  "  title: Truss Image Server API",
  "  version: 1.2.3",
  "  description: |",
  "    A block scalar whose body is not a mapping.",
  "    version: 9.9.9",
  "paths:",
  "  /images:transform:",
  "    post:",
  "      operationId: transformImage",
  "  /health:",
  "    get:",
  "      responses:",
  "        '200':",
  "          content:",
  "            application/json:",
  "              example:",
  "                status: ok",
  "                version: 1.2.3",
  "                checks:",
  "                  - name: storageRoot",
  "                    version: 8.8.8",
  "components:",
  "  schemas:",
  "    LivenessResponse:",
  "      properties:",
  "        version:",
  '          example: "1.2.3"',
  "",
].join("\n");

test("a scalar is read from the path it sits at", () => {
  assert.deepEqual(readYamlScalar(DOCUMENT, ["info", "version"]), { value: "1.2.3" });
  assert.deepEqual(
    readYamlScalar(DOCUMENT, [
      "paths",
      "/health",
      "get",
      "responses",
      "200",
      "content",
      "application/json",
      "example",
      "version",
    ]),
    { value: "1.2.3" },
  );
  assert.deepEqual(
    readYamlScalar(DOCUMENT, [
      "components",
      "schemas",
      "LivenessResponse",
      "properties",
      "version",
      "example",
    ]),
    { value: "1.2.3" },
  );
});

test("a version inside a block scalar or a sequence item is not the one at the path", () => {
  const problems = collectVersionProblems("1.2.3", [
    { label: "info", read: () => readYamlScalar(DOCUMENT, ["info", "version"]) },
  ]);
  assert.deepEqual(problems, []);
  assert.deepEqual(readYamlScalar(DOCUMENT, ["info", "description", "version"]), {
    error: "info > description > version is not in the document",
  });
});

test("a path that moved is a problem rather than a pass", () => {
  assert.deepEqual(readYamlScalar(DOCUMENT, ["info", "release"]), {
    error: "info > release is not in the document",
  });
});

test("a key containing a colon is one key", () => {
  assert.deepEqual(readYamlScalar(DOCUMENT, ["paths", "/images:transform", "post", "operationId"]), {
    value: "transformImage",
  });
});

test("a copy that disagrees is reported with both versions", () => {
  const problems = collectVersionProblems("1.2.3", [
    { label: "packages/truss-wasm/package.json", read: () => ({ value: "1.2.2" }) },
    { label: "docs/openapi.yaml info.version", read: () => ({ value: "1.2.3" }) },
  ]);
  assert.deepEqual(problems, ["packages/truss-wasm/package.json is 1.2.2, expected 1.2.3"]);
});

test("the crate version comes from the package table, not a dependency", () => {
  const manifest = [
    "[package]",
    'name = "truss-image"',
    'version = "0.22.0"',
    "",
    "[dependencies]",
    'image = { version = "0.25.10" }',
    "",
  ].join("\n");
  assert.equal(readCrateVersion(manifest), "0.22.0");
});

test("every addressed openapi path is distinct", () => {
  const addressed = OPENAPI_VERSION_PATHS.map((entry) => entry.join("."));
  assert.equal(new Set(addressed).size, addressed.length);
});

test("two Unreleased sections are a problem", () => {
  const changelog = [
    "# Changelog",
    "",
    "## [Unreleased]",
    "",
    "### Fixed",
    "",
    "- something",
    "",
    "## [Unreleased]",
    "",
    "### Fixed",
    "",
    "- something else",
    "",
    "## v1.2.3",
    "",
  ].join("\n");

  assert.deepEqual(collectChangelogProblems(changelog, "1.2.3"), [
    "the changelog has 2 [Unreleased] sections, which splits the notes",
  ]);
});

test("a release that did not rename its section is a problem", () => {
  const changelog = ["# Changelog", "", "## [Unreleased]", "", "## v1.2.2", ""].join("\n");

  assert.deepEqual(collectChangelogProblems(changelog, "1.2.3"), [
    'the newest released section is "## v1.2.2", expected "## v1.2.3"; a release renames [Unreleased] to the version it publishes',
  ]);
});

test("one Unreleased above the crate's own version is what a working tree looks like", () => {
  const changelog = [
    "# Changelog",
    "",
    "## [Unreleased]",
    "",
    "### Fixed",
    "",
    "- something",
    "",
    "## v1.2.3",
    "",
    "## v1.2.2",
    "",
  ].join("\n");

  assert.deepEqual(collectChangelogProblems(changelog, "1.2.3"), []);
});
