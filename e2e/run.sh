#!/usr/bin/env bash
#
# run.sh builds truss from this checkout (cargo, release profile) and runs the
# atago end-to-end suite (e2e/atago/*.atago.yaml) against the real binary.
#
# The test DEFINITIONS are atago YAML — this script is only the environment
# bootstrap (a plain shell program, not a test framework).
#
# Environment contract used by the specs:
#   PATH            truss resolves here (target/release from this checkout)
#   FIXTURES_DIR    absolute path to the shared integration/fixtures images
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

echo "e2e: building truss (cargo build --release --locked)..."
(cd "$REPO_ROOT" && cargo build --release --locked)

# Put the freshly built truss first on PATH so the specs exercise that binary.
export PATH="$REPO_ROOT/target/release:$PATH"
export FIXTURES_DIR="$REPO_ROOT/integration/fixtures"

echo "e2e: truss $(truss --version | head -1)"
# Extra args (e.g. --filter X) go before the path so the flag parser sees them.
atago run "$@" "$SCRIPT_DIR/atago"
