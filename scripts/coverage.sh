#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

if [ "$#" -eq 0 ]; then
  cargo llvm-cov --workspace --all-targets --summary-only
else
  cargo llvm-cov "$@"
fi
