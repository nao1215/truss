#!/usr/bin/env bash
#
# run.sh builds truss from this checkout (cargo, release profile) and runs the
# atago end-to-end suite (e2e/atago/*.atago.yaml) against the real binary.
#
# The test DEFINITIONS are atago YAML — this script is only the environment
# bootstrap (a plain shell program, not a test framework).
#
# It runs on Linux, macOS, and Windows. On Windows it is the Git Bash that ships
# with the runner: this script is the only shell in the loop, because no scenario
# uses `shell: true` (which would be cmd.exe there, with no $VAR and no `cat`).
# The shared image fixtures reach the specs as ${fixtures}, declared once in
# e2e/atago/atago.project.yaml, so nothing here has to export them.
#
# Environment contract used by the specs:
#   PATH            truss resolves here (target/release from this checkout)
#
# Usage: e2e/run.sh [atago args...]        (e.g. e2e/run.sh --filter convert)
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

if ! command -v atago >/dev/null 2>&1; then
	echo "e2e: atago is not installed. Install it from https://github.com/nao1215/atago" >&2
	echo "e2e: e.g. 'go install github.com/nao1215/atago@latest' (CI uses nao1215/setup-atago)" >&2
	exit 127
fi

cd "$REPO_ROOT"

echo "e2e: building truss (cargo build --release --locked)..."
cargo build --release --locked

# Put the freshly built truss first on PATH so the specs exercise that binary.
# Windows resolves the same name to truss.exe through PATHEXT.
export PATH="$REPO_ROOT/target/release:$PATH"

echo "e2e: truss $(truss --version | head -1)"
# Relative spec path: it is the same string on every platform, and nothing has to
# translate it on the way to a native Windows atago.
# Extra args (e.g. --filter X) go before the path so the flag parser sees them.
atago run "$@" e2e/atago
