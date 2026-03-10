#!/bin/bash
# Upload fixture images to s3mock, wait for truss, then run the given command.
set -eu

S3_HOST="${S3_HOST:-s3mock}"
S3_PORT="${S3_PORT:-9090}"
BUCKET="truss-test"
BASE_URL="http://${S3_HOST}:${S3_PORT}"

# ── Wait for s3mock (bucket must be accessible) ─────────────────
echo "Waiting for s3mock..."
for i in $(seq 1 30); do
  # GET the bucket to confirm s3mock can serve requests, not just accept TCP.
  status=$(python3 -c "
import urllib.request, sys
try:
    r = urllib.request.urlopen('${BASE_URL}/${BUCKET}', timeout=2)
    print(r.status)
except Exception:
    print('0')
" 2>/dev/null) || true
  if [ "$status" = "200" ]; then
    echo "s3mock is ready (attempt ${i})"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: s3mock did not become ready (last status: ${status:-none})"
    exit 1
  fi
  sleep 1
done

# ── Upload fixtures ──────────────────────────────────────────────
upload() {
  local key="$1"
  local file="$2"
  local url="${BASE_URL}/${BUCKET}/${key}"
  local status
  status=$(python3 -c "
import urllib.request, sys
data = open('${file}', 'rb').read()
req = urllib.request.Request('${url}', data=data, method='PUT')
req.add_header('Content-Type', 'application/octet-stream')
try:
    r = urllib.request.urlopen(req, timeout=5)
    print(r.status)
except urllib.error.HTTPError as e:
    print(e.code)
except Exception:
    print('0')
" 2>/dev/null)
  if [ "$status" = "200" ] || [ "$status" = "201" ]; then
    echo "  uploaded: ${key}"
  else
    echo "  ERROR: upload ${key} returned ${status}"
    exit 1
  fi
}

echo "Uploading fixtures to s3mock (bucket: ${BUCKET})..."
upload "sample.png"   /fixtures/sample.png
upload "sample.jpg"   /fixtures/sample.jpg
upload "1px.png"      /fixtures/1px.png
upload "transparent.png" /fixtures/transparent.png

# Generate a tiny 1x1 red PNG that exists ONLY in S3 — not on local disk.
# This proves that by-path truly reads from S3, not the filesystem fallback.
S3ONLY_FILE="/tmp/s3only.png"
python3 -c "
import struct, zlib
def png_1x1(r, g, b):
    raw = b'\x00' + bytes([r, g, b])
    compressed = zlib.compress(raw)
    def chunk(tag, data):
        c = tag + data
        return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)
    ihdr = struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', compressed) + chunk(b'IEND', b'')
import sys; sys.stdout.buffer.write(png_1x1(255, 0, 0))
" > "$S3ONLY_FILE"
upload "s3only.png" "$S3ONLY_FILE"

# ── Wait for truss (via nginx), then exec the given command ──────
export SERVER_HOST="${SERVER_HOST:-nginx}"
export SERVER_PORT="${SERVER_PORT:-80}"
exec /wait-for-server.sh "$@"
