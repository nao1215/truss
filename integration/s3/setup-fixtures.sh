#!/bin/bash
# Upload fixture images to s3mock, wait for truss, then run the given command.
set -eu

S3_HOST="${S3_HOST:-s3mock}"
S3_PORT="${S3_PORT:-9090}"
BUCKET="truss-test"

# ── Wait for s3mock ──────────────────────────────────────────────
echo "Waiting for s3mock..."
for i in $(seq 1 30); do
  if wget -q -O /dev/null "http://${S3_HOST}:${S3_PORT}" 2>/dev/null; then
    echo "s3mock is ready (attempt ${i})"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: s3mock did not become ready"
    exit 1
  fi
  sleep 1
done

# ── Upload fixtures ──────────────────────────────────────────────
upload() {
  local key="$1"
  local file="$2"
  local url="http://${S3_HOST}:${S3_PORT}/${BUCKET}/${key}"
  local status
  status=$(wget -q -O /dev/null --method=PUT --body-file="$file" \
    --header="Content-Type: application/octet-stream" \
    -S "$url" 2>&1 | grep "HTTP/" | tail -1 | awk '{print $2}')
  if [ "$status" = "200" ] || [ "$status" = "201" ]; then
    echo "  uploaded: ${key}"
  else
    echo "  WARN: upload ${key} returned ${status}"
  fi
}

echo "Uploading fixtures to s3mock (bucket: ${BUCKET})..."
upload "sample.png"   /fixtures/sample.png
upload "sample.jpg"   /fixtures/sample.jpg
upload "1px.png"      /fixtures/1px.png
upload "transparent.png" /fixtures/transparent.png

# ── Wait for truss (via nginx), then exec the given command ──────
export SERVER_HOST="${SERVER_HOST:-nginx}"
export SERVER_PORT="${SERVER_PORT:-80}"
exec /wait-for-server.sh "$@"
