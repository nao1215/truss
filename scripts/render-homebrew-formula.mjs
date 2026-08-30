import { readFileSync } from "node:fs";

const [tag, manifestPath] = process.argv.slice(2);

if (!tag || !manifestPath) {
  throw new Error(
    "usage: node ./scripts/render-homebrew-formula.mjs <tag> <release-manifest.json>",
  );
}

const version = tag.startsWith("v") ? tag.slice(1) : tag;
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));

if (manifest.tag !== tag) {
  throw new Error(`manifest is for ${manifest.tag}, not ${tag}`);
}

// The formula sources its URLs and hashes from the release manifest so that
// Homebrew, checksums.txt and the manifest can never disagree about a build.
function artifact(target) {
  const entry = manifest.artifacts.find((candidate) => candidate.target === target);

  if (!entry) {
    throw new Error(`release manifest has no artifact for ${target}`);
  }
  if (!/^[0-9a-f]{64}$/.test(entry.archive.sha256)) {
    throw new Error(`release manifest has a malformed SHA-256 for ${target}`);
  }

  return entry;
}

function block(target) {
  const entry = artifact(target);

  return `      url "${entry.archive.url}"
      sha256 "${entry.archive.sha256}"`;
}

const formula = `# typed: false
# frozen_string_literal: true

class Truss < Formula
  desc "Rust image toolkit for CLI, HTTP, and WASM workflows"
  homepage "https://github.com/${manifest.repository}"
  version "${version}"
  license "MIT"

  on_macos do
    if Hardware::CPU.intel?
${block("x86_64-apple-darwin")}
    end

    if Hardware::CPU.arm?
${block("aarch64-apple-darwin")}
    end
  end

  on_linux do
    if Hardware::CPU.intel? && Hardware::CPU.is_64_bit?
${block("x86_64-unknown-linux-gnu")}
    end

    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
${block("aarch64-unknown-linux-gnu")}
    end
  end

  def install
    bin.install "truss"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/truss --version")
  end
end
`;

process.stdout.write(formula);
