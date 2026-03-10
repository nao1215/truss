#!/bin/bash
# Upload fixture images to Azurite, wait for truss, then run the given command.
set -eu

AZURE_HOST="${AZURE_HOST:-azurite}"
AZURE_PORT="${AZURE_PORT:-10000}"
CONTAINER="truss-test"
ACCOUNT="devstoreaccount1"
BASE_URL="http://${AZURE_HOST}:${AZURE_PORT}/${ACCOUNT}"

# Well-known Azurite development storage account key.
# https://learn.microsoft.com/en-us/azure/storage/common/storage-use-azurite#well-known-storage-account-and-key
AZURE_KEY="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="

# Write the Python helper to a temp file to avoid quoting issues with heredocs.
HELPER_PY=$(mktemp /tmp/azure_helper_XXXXXX.py)
cat > "$HELPER_PY" << 'PYEOF'
"""Azure Storage SharedKey helper for Azurite integration tests.

Usage:
  python3 helper.py create-container <base_url> <account> <key> <container>
  python3 helper.py upload <base_url> <account> <key> <container> <blob_key> <file_path>

Note on canonical resource for Azurite path-style URLs:
  For path-style URLs (http://host:port/account/container), the canonical
  resource includes the account twice: /{account}/{account}/{container}.
  This is because the Azure SharedKey spec defines canonical resource as
  /{account}/{url-path}, and the url-path already contains /{account}/...
"""
import hmac, hashlib, base64, sys, urllib.request
from datetime import datetime, timezone

VERSION = "2021-08-06"

def sign(account, key, method, content_length, content_type, extra_headers, canon_resource):
    date_str = datetime.now(timezone.utc).strftime("%a, %d %b %Y %H:%M:%S GMT")
    headers = {"x-ms-date": date_str, "x-ms-version": VERSION}
    headers.update({k.lower(): v for k, v in extra_headers.items()})
    canon_headers_str = "".join(f"{k}:{v}\n" for k, v in sorted(headers.items()))

    string_to_sign = (
        f"{method}\n"
        "\n"   # Content-Encoding
        "\n"   # Content-Language
        f"{content_length}\n"
        "\n"   # Content-MD5
        f"{content_type}\n"
        "\n"   # Date
        "\n"   # If-Modified-Since
        "\n"   # If-Match
        "\n"   # If-None-Match
        "\n"   # If-Unmodified-Since
        "\n"   # Range
        f"{canon_headers_str}"
        f"/{account}{canon_resource}"
    )
    sig = base64.b64encode(
        hmac.new(base64.b64decode(key), string_to_sign.encode(), hashlib.sha256).digest()
    ).decode()
    return f"SharedKey {account}:{sig}", date_str

def create_container(base_url, account, key, container):
    # Content-Length must be empty in string-to-sign for zero-length bodies.
    # Canonical resource for path-style Azurite: /{account}/{container}
    # (sign() prepends /{account}, giving /{account}/{account}/{container}).
    auth, date_str = sign(
        account, key, "PUT", "", "",
        {"x-ms-blob-public-access": "blob"},
        f"/{account}/{container}\nrestype:container",
    )
    url = f"{base_url}/{container}?restype=container"
    req = urllib.request.Request(url, data=None, method="PUT")
    req.add_header("Content-Length", "0")
    req.add_header("Authorization", auth)
    req.add_header("x-ms-date", date_str)
    req.add_header("x-ms-version", VERSION)
    req.add_header("x-ms-blob-public-access", "blob")
    try:
        r = urllib.request.urlopen(req, timeout=5)
        print(r.status)
    except urllib.error.HTTPError as e:
        print(e.code)
    except Exception as ex:
        print("0", file=sys.stderr)
        print(f"  create-container error: {ex}", file=sys.stderr)
        print("0")

def upload(base_url, account, key, container, blob_key, file_path):
    data = open(file_path, "rb").read()
    cl = str(len(data))
    auth, date_str = sign(
        account, key, "PUT", cl, "application/octet-stream",
        {"x-ms-blob-type": "BlockBlob"},
        f"/{account}/{container}/{blob_key}",
    )
    url = f"{base_url}/{container}/{blob_key}"
    req = urllib.request.Request(url, data=data, method="PUT")
    req.add_header("Content-Type", "application/octet-stream")
    req.add_header("Content-Length", cl)
    req.add_header("x-ms-blob-type", "BlockBlob")
    req.add_header("Authorization", auth)
    req.add_header("x-ms-date", date_str)
    req.add_header("x-ms-version", VERSION)
    try:
        r = urllib.request.urlopen(req, timeout=10)
        print(r.status)
    except urllib.error.HTTPError as e:
        print(e.code)
    except Exception as ex:
        print(f"  upload error: {ex}", file=sys.stderr)
        print("0")

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "create-container":
        create_container(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5])
    elif cmd == "upload":
        upload(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6], sys.argv[7])
PYEOF

# ── Create the container with public blob access ─────────────────
echo "Creating container in Azurite..."
for i in $(seq 1 30); do
  status=$(python3 "$HELPER_PY" create-container "$BASE_URL" "$ACCOUNT" "$AZURE_KEY" "$CONTAINER") || true
  if [ "$status" = "201" ] || [ "$status" = "409" ]; then
    echo "Container ready (attempt ${i})"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: Azurite did not become ready (last status: ${status:-none})"
    exit 1
  fi
  sleep 1
done

# ── Upload fixtures ──────────────────────────────────────────────
upload() {
  local blob_key="$1"
  local file="$2"
  local status
  status=$(python3 "$HELPER_PY" upload "$BASE_URL" "$ACCOUNT" "$AZURE_KEY" "$CONTAINER" "$blob_key" "$file")
  if [ "$status" = "200" ] || [ "$status" = "201" ]; then
    echo "  uploaded: ${blob_key}"
  else
    echo "  ERROR: upload ${blob_key} returned ${status}"
    exit 1
  fi
}

echo "Uploading fixtures to Azurite (container: ${CONTAINER})..."
upload "sample.png"   /fixtures/sample.png
upload "sample.jpg"   /fixtures/sample.jpg
upload "1px.png"      /fixtures/1px.png
upload "transparent.png" /fixtures/transparent.png

# Generate a tiny 1x1 red PNG that exists ONLY in Azure — not on local disk.
# This proves that by-path truly reads from Azure, not the filesystem fallback.
AZUREONLY_FILE="/tmp/azureonly.png"
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
" > "$AZUREONLY_FILE"
upload "azureonly.png" "$AZUREONLY_FILE"

rm -f "$HELPER_PY"

# ── Run the given command (truss/nginx readiness is handled by compose healthchecks) ──
exec "$@"
