#!/usr/bin/env bash
# generate-fixtures.sh — Create integration test fixture images.
#
# Requires: ImageMagick 7 (magick), Python 3
# Output:   integration/fixtures/
#
# These fixtures exercise edge cases that real-world image processing
# tools must handle correctly. "Normal images only" is not enough.

set -euo pipefail

DIR="$(cd "$(dirname "$0")/../integration/fixtures" && pwd)"
mkdir -p "$DIR"

echo "=== Generating test fixtures in $DIR ==="

# ---------------------------------------------------------------------------
# 1. Normal baseline images
# ---------------------------------------------------------------------------

echo "[1/14] sample.png — 4x3 RGBA baseline"
magick -size 4x3 xc:'rgba(255,0,0,255)' "$DIR/sample.png"

echo "[2/14] sample.jpg — 4x3 RGB JPEG baseline"
magick -size 4x3 xc:'rgb(0,128,255)' "$DIR/sample.jpg"

# ---------------------------------------------------------------------------
# 2. Dimension edge cases
# ---------------------------------------------------------------------------

echo "[3/18] sample.bmp — 4x3 RGBA BMP baseline"
magick -size 4x3 xc:'rgba(0,255,0,255)' "$DIR/sample.bmp"

echo "[4/18] transparent.bmp — 32-bit RGBA BMP with alpha"
magick -size 4x4 xc:'rgba(255,0,0,128)' -type TrueColorAlpha BMP3:"$DIR/transparent.bmp"

echo "[5/18] 1px.png — minimum dimension (1x1)"
magick -size 1x1 xc:red "$DIR/1px.png"

echo "[4/14] large.png — 10000x1 wide image"
magick -size 10000x1 xc:blue "$DIR/large.png"

echo "[5/14] tall.png — 1x10000 tall image"
magick -size 1x10000 xc:green "$DIR/tall.png"

# ---------------------------------------------------------------------------
# 3. Alpha / transparency
# ---------------------------------------------------------------------------

echo "[6/14] transparent.png — fully transparent 4x4 RGBA"
magick -size 4x4 xc:'rgba(0,0,0,0)' -type TrueColorAlpha PNG32:"$DIR/transparent.png"

echo "[7/14] semitransparent.png — checkerboard with alpha"
magick -size 8x8 \
  xc:'rgba(255,0,0,128)' xc:'rgba(0,0,255,64)' \
  +append \
  "$DIR/semitransparent.png"

# ---------------------------------------------------------------------------
# 4. EXIF orientation
# ---------------------------------------------------------------------------

echo "[8/14] exif-rotated.jpg — JPEG with EXIF Orientation=6 (90° CW)"
magick -size 4x3 xc:'rgb(255,0,0)' \
  -set 'EXIF:Orientation' 6 \
  "$DIR/exif-rotated.jpg"

# ---------------------------------------------------------------------------
# 5. CMYK JPEG
# ---------------------------------------------------------------------------

echo "[9/14] cmyk.jpg — CMYK color space JPEG"
magick -size 4x3 xc:'cmyk(0%,100%,100%,0%)' \
  -colorspace CMYK \
  "$DIR/cmyk.jpg"

# ---------------------------------------------------------------------------
# 6. Truncated / corrupted files (Python for byte manipulation)
# ---------------------------------------------------------------------------

echo "[10/14] truncated.jpg — JPEG truncated mid-stream"
python3 -c "
import subprocess, os
# Generate a valid JPEG first
subprocess.run(['magick', '-size', '64x64', 'xc:red', '-quality', '85', '/tmp/truss-fixture-full.jpg'], check=True)
data = open('/tmp/truss-fixture-full.jpg', 'rb').read()
# Truncate at ~60% of file
cut = len(data) * 6 // 10
open('$DIR/truncated.jpg', 'wb').write(data[:cut])
os.remove('/tmp/truss-fixture-full.jpg')
print(f'  wrote {cut} of {len(data)} bytes')
"

echo "[11/14] corrupt-header.jpg — JPEG with corrupted header bytes"
python3 -c "
import subprocess, os
subprocess.run(['magick', '-size', '8x8', 'xc:blue', '-quality', '90', '/tmp/truss-fixture-corrupt.jpg'], check=True)
data = bytearray(open('/tmp/truss-fixture-corrupt.jpg', 'rb').read())
# Corrupt bytes 6-10 (inside the JFIF/Exif header area)
for i in range(6, min(11, len(data))):
    data[i] = 0x00
open('$DIR/corrupt-header.jpg', 'wb').write(data)
os.remove('/tmp/truss-fixture-corrupt.jpg')
print(f'  wrote {len(data)} bytes with corrupted header')
"

echo "[12/14] invalid-chunk.png — PNG with invalid chunk type"
python3 -c "
import struct, zlib

# Build a minimal valid PNG, then insert a bad chunk
width, height = 4, 4
raw = b''
for y in range(height):
    raw += b'\x00'
    for x in range(width):
        raw += bytes([255, 0, 0, 255])
compressed = zlib.compress(raw)
sig = b'\x89PNG\r\n\x1a\n'

def chunk(ctype, data):
    c = ctype + data
    return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)

ihdr = struct.pack('>IIBBBBB', width, height, 8, 6, 0, 0, 0)
# Insert an invalid chunk with bad CRC between IHDR and IDAT
bad_chunk = struct.pack('>I', 4) + b'bADc' + b'\xDE\xAD\xBE\xEF' + struct.pack('>I', 0x00000000)
out = sig + chunk(b'IHDR', ihdr) + bad_chunk + chunk(b'IDAT', compressed) + chunk(b'IEND', b'')
open('$DIR/invalid-chunk.png', 'wb').write(out)
print(f'  wrote {len(out)} bytes with invalid PNG chunk')
"

echo "[13/14] zero-bytes.bin — completely empty file"
: > "$DIR/zero-bytes.bin"

echo "[14/14] random-noise.bin — 256 bytes of random data"
python3 -c "
import os
open('$DIR/random-noise.bin', 'wb').write(os.urandom(256))
print('  wrote 256 random bytes')
"

# ---------------------------------------------------------------------------
# 7. SVG edge cases (hand-crafted)
# ---------------------------------------------------------------------------

echo "[bonus] svg-entity-bomb.svg — XML entity expansion attack"
cat > "$DIR/svg-entity-bomb.svg" << 'SVGEOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg [
  <!ENTITY a "aaaaaaaaaa">
  <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
  <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
  <!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">
  <!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <text x="10" y="50">&e;</text>
</svg>
SVGEOF

echo "[bonus] svg-script.svg — SVG with embedded script (XSS)"
cat > "$DIR/svg-script.svg" << 'SVGEOF'
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <script>alert('xss')</script>
  <rect width="100" height="100" fill="red" onclick="alert('click')"/>
  <text x="10" y="50" onload="alert('load')">hello</text>
</svg>
SVGEOF

echo "[bonus] svg-external-ref.svg — SVG with external entity reference"
cat > "$DIR/svg-external-ref.svg" << 'SVGEOF'
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
     width="100" height="100">
  <image xlink:href="https://evil.example.com/tracking.png" width="100" height="100"/>
  <use xlink:href="https://evil.example.com/shapes.svg#arrow"/>
</svg>
SVGEOF

echo "[bonus] svg-minimal.svg — smallest valid SVG"
cat > "$DIR/svg-minimal.svg" << 'SVGEOF'
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>
SVGEOF

echo ""
echo "=== Done: $(ls "$DIR" | wc -l) fixtures in $DIR ==="
ls -lhS "$DIR"
