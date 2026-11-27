#!/usr/bin/env bash
#
# coverage.sh measures line coverage of the unit tests, the Rust integration
# tests (tests/*.rs) and the atago CLI suite (e2e/atago) together, and writes
# it as LCOV plus a summary.
#
# All three run instrumented builds of the same code: `cargo test` for the
# unit and integration tests, and a debug truss on PATH for the atago specs,
# so the argument parsing, file handling and exit paths that only the real
# binary reaches are counted too. The debug profile keeps the E2E binary and
# the test binaries in one report.
#
# Needs cargo-llvm-cov 0.9.1 or newer (`cargo install cargo-llvm-cov`, plus
# the llvm-tools-preview component) and atago on PATH. Older cargo-llvm-cov
# does not find test binaries in the build-dir layout of current Cargo and
# reports only the truss binary.
#
# Usage: scripts/coverage.sh [lcov output path]   (default lcov.info)
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
OUT="${1:-lcov.info}"
cd "$REPO_ROOT"

if ! command -v atago >/dev/null 2>&1; then
	echo "coverage: atago is not installed; see CONTRIBUTING.md" >&2
	exit 127
fi

# Every cargo command below builds with -C instrument-coverage and writes its
# profiles under target/coverage. A directory of its own: the wrapper that
# instruments the build is invisible to cargo's change detection, so a plain
# `cargo test` in target/ would otherwise reuse instrumented builds.
export CARGO_TARGET_DIR="$REPO_ROOT/target/coverage"
eval "$(cargo llvm-cov show-env --sh)"
cargo llvm-cov clean --workspace

# Dependencies are built optimized: only truss's own code is instrumented, and
# unoptimized image codecs make the larger E2E conversions slow.
deps_optimized=(--config 'profile.dev.package."*".opt-level=3')

echo "coverage: unit and integration tests"
cargo test "${deps_optimized[@]}" --locked --all-features --all-targets

echo "coverage: atago CLI suite"
cargo build "${deps_optimized[@]}" --locked --all-features
PATH="$CARGO_TARGET_DIR/debug:$PATH" atago run e2e/atago

cargo llvm-cov report --lcov --output-path "$OUT"
cargo llvm-cov report --summary-only
echo "coverage: wrote $OUT"
