# shellcheck shell=sh

# ---------------------------------------------------------------------------
# spec_helper.sh - ShellSpec test helpers for the truss CLI
# ---------------------------------------------------------------------------

# Path to the truss binary. In Docker it lives at /usr/local/bin/truss;
# override via TRUSS_BIN for local development builds.
TRUSS_BIN="${TRUSS_BIN:-/usr/local/bin/truss}"

# Path to the fixtures directory. In Docker the Dockerfile copies fixtures
# to /fixtures; override via FIXTURES_DIR for local runs.
FIXTURES_DIR="${FIXTURES_DIR:-/fixtures}"

# Convenience aliases for fixtures.
SAMPLE_PNG="${FIXTURES_DIR}/sample.png"
SAMPLE_JPG="${FIXTURES_DIR}/sample.jpg"
SAMPLE_BMP="${FIXTURES_DIR}/sample.bmp"

# ---------------------------------------------------------------------------
# Temporary directory — cleaned up automatically by ShellSpec
# ---------------------------------------------------------------------------

setup() {
  WORK_DIR="${SHELLSPEC_TMPDIR}/truss-test-$$"
  mkdir -p "$WORK_DIR"
}

cleanup() {
  rm -rf "$WORK_DIR"
}

# ---------------------------------------------------------------------------
# Helper: run truss with arguments
# ---------------------------------------------------------------------------

truss() {
  "$TRUSS_BIN" "$@"
}

# ---------------------------------------------------------------------------
# Helper: check whether a file is a valid JPEG
# ---------------------------------------------------------------------------

is_jpeg() {
  # JPEG files start with FF D8 FF
  head -c 3 "$1" 2>/dev/null | od -A n -t x1 | tr -d ' ' | grep -qi 'ffd8ff'
}

# ---------------------------------------------------------------------------
# Helper: check whether a file is a valid PNG
# ---------------------------------------------------------------------------

is_png() {
  # PNG files start with 89 50 4E 47
  head -c 4 "$1" 2>/dev/null | od -A n -t x1 | tr -d ' ' | grep -qi '89504e47'
}

# ---------------------------------------------------------------------------
# Helper: check whether a file is a valid BMP
# ---------------------------------------------------------------------------

is_bmp() {
  # BMP files start with 42 4D ("BM")
  head -c 2 "$1" 2>/dev/null | od -A n -t x1 | tr -d ' ' | grep -qi '424d'
}

# ---------------------------------------------------------------------------
# Helper: get image width via truss inspect
# ---------------------------------------------------------------------------

image_width() {
  "$TRUSS_BIN" inspect "$1" 2>/dev/null | grep '"width"' | grep -o '[0-9]*'
}

# ---------------------------------------------------------------------------
# Helper: network availability check
# ---------------------------------------------------------------------------

has_network() {
  # Attempt a lightweight HTTP HEAD to detect connectivity
  if command -v wget >/dev/null 2>&1; then
    wget -q --spider --timeout=3 "https://httpbin.org/status/200" 2>/dev/null
  elif command -v curl >/dev/null 2>&1; then
    curl -sf --head --max-time 3 "https://httpbin.org/status/200" >/dev/null 2>&1
  else
    return 1
  fi
}
