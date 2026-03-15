import { readFileSync } from "node:fs";

const [
  tag,
  darwinAmd64ShaFile,
  darwinArm64ShaFile,
  linuxAmd64ShaFile,
  linuxArm64ShaFile,
] = process.argv.slice(2);

if (
  !tag ||
  !darwinAmd64ShaFile ||
  !darwinArm64ShaFile ||
  !linuxAmd64ShaFile ||
  !linuxArm64ShaFile
) {
  throw new Error(
    "usage: node ./scripts/render-homebrew-formula.mjs <tag> <darwin-amd64.sha256> <darwin-arm64.sha256> <linux-amd64.sha256> <linux-arm64.sha256>",
  );
}

const version = tag.startsWith("v") ? tag.slice(1) : tag;
const repository = process.env.GITHUB_REPOSITORY ?? "nao1215/truss";

function readSha256(filePath) {
  const content = readFileSync(filePath, "utf8").trim();
  const [sha256] = content.split(/\s+/);

  if (!sha256 || !/^[0-9a-f]{64}$/i.test(sha256)) {
    throw new Error(`failed to parse SHA256 from ${filePath}`);
  }

  return sha256;
}

function releaseUrl(target) {
  return `https://github.com/${repository}/releases/download/${tag}/truss-${tag}-${target}.tar.gz`;
}

const formula = `# typed: false
# frozen_string_literal: true

class Truss < Formula
  desc "Rust image toolkit for CLI, HTTP, and WASM workflows"
  homepage "https://github.com/${repository}"
  version "${version}"
  license "MIT"

  on_macos do
    if Hardware::CPU.intel?
      url "${releaseUrl("x86_64-apple-darwin")}"
      sha256 "${readSha256(darwinAmd64ShaFile)}"
    end

    if Hardware::CPU.arm?
      url "${releaseUrl("aarch64-apple-darwin")}"
      sha256 "${readSha256(darwinArm64ShaFile)}"
    end
  end

  on_linux do
    if Hardware::CPU.intel? && Hardware::CPU.is_64_bit?
      url "${releaseUrl("x86_64-unknown-linux-gnu")}"
      sha256 "${readSha256(linuxAmd64ShaFile)}"
    end

    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "${releaseUrl("aarch64-unknown-linux-gnu")}"
      sha256 "${readSha256(linuxArm64ShaFile)}"
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
