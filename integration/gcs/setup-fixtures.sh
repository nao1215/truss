#!/bin/bash
# Upload fixture images to fake-gcs-server, wait for truss, then run the given command.
set -eu

GCS_HOST="${GCS_HOST:-fake-gcs}"
GCS_PORT="${GCS_PORT:-4443}"
BUCKET="truss-test"
BASE_URL="http://${GCS_HOST}:${GCS_PORT}"

# ── Create the bucket ────────────────────────────────────────────
echo "Creating bucket in fake-gcs-server..."
for i in $(seq 1 30); do
  status=$(python3 -c "
import urllib.request, json, sys
data = json.dumps({'name': '${BUCKET}'}).encode()
req = urllib.request.Request('${BASE_URL}/storage/v1/b', data=data, method='POST')
req.add_header('Content-Type', 'application/json')
try:
    r = urllib.request.urlopen(req, timeout=2)
    print(r.status)
except urllib.error.HTTPError as e:
    # 409 = bucket already exists, which is fine
    print(e.code)
except Exception:
    print('0')
" 2>/dev/null) || true
  if [ "$status" = "200" ] || [ "$status" = "409" ]; then
    echo "Bucket ready (attempt ${i})"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: fake-gcs-server did not become ready (last status: ${status:-none})"
    exit 1
  fi
  sleep 1
done

# ── Upload fixtures ──────────────────────────────────────────────
upload() {
  local key="$1"
  local file="$2"
  local url="${BASE_URL}/upload/storage/v1/b/${BUCKET}/o?uploadType=media&name=${key}"
  local status
  status=$(python3 -c "
import urllib.request, sys
data = open('${file}', 'rb').read()
req = urllib.request.Request('${url}', data=data, method='POST')
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
    echo "  WARN: upload ${key} returned ${status}"
  fi
}

echo "Uploading fixtures to fake-gcs-server (bucket: ${BUCKET})..."
upload "sample.png"   /fixtures/sample.png
upload "sample.jpg"   /fixtures/sample.jpg
upload "1px.png"      /fixtures/1px.png
upload "transparent.png" /fixtures/transparent.png

# Generate a tiny 1x1 red PNG that exists ONLY in GCS — not on local disk.
# This proves that by-path truly reads from GCS, not the filesystem fallback.
GCSONLY_FILE="/tmp/gcsonly.png"
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
" > "$GCSONLY_FILE"
upload "gcsonly.png" "$GCSONLY_FILE"

# ── Wait for truss (via nginx), then exec the given command ──────
export SERVER_HOST="${SERVER_HOST:-nginx}"
export SERVER_PORT="${SERVER_PORT:-80}"
exec /wait-for-server.sh "$@"
