#!/bin/bash
# Wait for the truss server to become healthy, then run the given command.
# Used as the entrypoint for the runn container because the truss
# distroless image does not have wget/curl for a Docker healthcheck.
#
# Uses Python (available in the runn image) for HTTP health checks to
# avoid raw TCP probes that would block truss worker threads.

set -eu

HOST="${SERVER_HOST:-truss}"
PORT="${SERVER_PORT:-8080}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-30}"
SLEEP_SECONDS="${SLEEP_SECONDS:-2}"

check_server() {
  # Prefer wget or curl for a real HTTP check; fall back to Python.
  if command -v wget >/dev/null 2>&1; then
    wget -q -O /dev/null "http://${HOST}:${PORT}/health/live" 2>/dev/null
  elif command -v curl >/dev/null 2>&1; then
    curl -sf "http://${HOST}:${PORT}/health/live" >/dev/null 2>&1
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c "
import urllib.request, sys
try:
    r = urllib.request.urlopen('http://${HOST}:${PORT}/health/live', timeout=2)
    sys.exit(0 if r.status == 200 else 1)
except Exception:
    sys.exit(1)
" 2>/dev/null
  else
    # Last resort: bash TCP probe. Note: this opens a connection without
    # sending an HTTP request, which may block a truss worker thread.
    (echo > "/dev/tcp/${HOST}/${PORT}") 2>/dev/null
  fi
}

attempt=1
while [ "$attempt" -le "$MAX_ATTEMPTS" ]; do
  if check_server; then
    echo "Server is ready (attempt ${attempt})"
    exec "$@"
  fi
  echo "Waiting for server... (attempt ${attempt}/${MAX_ATTEMPTS})"
  sleep "$SLEEP_SECONDS"
  attempt=$((attempt + 1))
done

echo "ERROR: Server did not become ready after ${MAX_ATTEMPTS} attempts"
exit 1
