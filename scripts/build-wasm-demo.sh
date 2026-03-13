#!/bin/sh

set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DIST_DIR="$ROOT_DIR/web/dist"
PKG_DIR="$DIST_DIR/pkg"
SWAGGER_DIST_DIR="$DIST_DIR/swagger"
WASM_PATH="$ROOT_DIR/target/wasm32-unknown-unknown/release/truss.wasm"

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen CLI is required. Install it with:" >&2
  echo "  cargo install wasm-bindgen-cli --version 0.2.114" >&2
  exit 1
fi

mkdir -p "$DIST_DIR" "$PKG_DIR" "$SWAGGER_DIST_DIR"

cargo build \
  --release \
  --locked \
  --target wasm32-unknown-unknown \
  --lib \
  --no-default-features \
  --features "wasm,svg" \
  --manifest-path "$ROOT_DIR/Cargo.toml"

wasm-bindgen \
  --target web \
  --out-dir "$PKG_DIR" \
  "$WASM_PATH"

cp "$ROOT_DIR/web/index.html" "$DIST_DIR/index.html"
cp "$ROOT_DIR/web/app.js" "$DIST_DIR/app.js"
cp "$ROOT_DIR/web/styles.css" "$DIST_DIR/styles.css"
cp "$ROOT_DIR/doc/openapi.yaml" "$DIST_DIR/openapi.yaml"
cp "$ROOT_DIR/web/swagger/index.html" "$SWAGGER_DIST_DIR/index.html"
cp "$ROOT_DIR/web/swagger/swagger.css" "$SWAGGER_DIST_DIR/swagger.css"
: > "$DIST_DIR/.nojekyll"
