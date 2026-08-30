#!/usr/bin/env bash
# generate-fixtures.sh — Create integration test fixture images.
#
# Requires: ImageMagick 7 (magick), Python 3 with Pillow (for the ICC fixture)
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

echo "[7/14] semitransparent.png — two half-transparent blocks, 16x8"
# Pillow, not ImageMagick. `magick xc:'rgba(255,0,0,128)'` composites the alpha away
# unless the canvas already has an alpha channel, and PNG32: only adds a channel that is
# opaque everywhere, which is how this fixture ended up fully opaque despite its name.
python3 -c "
from PIL import Image

image = Image.new('RGBA', (16, 8))
for x in range(16):
    for y in range(8):
        image.putpixel((x, y), (255, 0, 0, 128) if x < 8 else (0, 0, 255, 64))
image.save('$DIR/semitransparent.png')
print('  wrote 16x8 with alpha 128 and 64')
"

# ---------------------------------------------------------------------------
# 4. EXIF orientation
# ---------------------------------------------------------------------------

echo "[8/14] exif-rotated.jpg — 40x20 JPEG with EXIF Orientation=6 (90° CW)"
# Pillow, not ImageMagick: `magick -set 'EXIF:Orientation'` is a no-op on an image that
# has no EXIF profile to begin with, so this fixture used to carry no orientation tag at
# all and every test named for it was vacuous.
#
# The image is deliberately non-square and two-toned, so applying the orientation is
# observable twice over: the dimensions swap 40x20 -> 20x40, and the blue stripe moves
# from the left edge to the top.
python3 -c "
from PIL import Image

image = Image.new('RGB', (40, 20), (255, 0, 0))
for x in range(10):
    for y in range(20):
        image.putpixel((x, y), (0, 0, 255))
exif = image.getexif()
exif[274] = 6  # Orientation: rotate 90 degrees clockwise
image.save('$DIR/exif-rotated.jpg', exif=exif.tobytes(), quality=95)
print('  wrote 40x20 with EXIF Orientation=6')
"

# ---------------------------------------------------------------------------
# 4b. ICC profile (needs Pillow's ImageCms; ImageMagick cannot mint a profile)
# ---------------------------------------------------------------------------

echo "[8b/14] icc-profile.jpg — JPEG carrying an embedded sRGB ICC profile, no EXIF"
python3 -c "
from PIL import Image, ImageCms

# A gradient so lossy re-encoding has something to work with.
img = Image.new('RGB', (64, 64))
for x in range(64):
    for y in range(64):
        img.putpixel((x, y), (x * 4 % 256, y * 4 % 256, (x + y) * 2 % 256))
icc = ImageCms.ImageCmsProfile(ImageCms.createProfile('sRGB')).tobytes()
img.save('$DIR/icc-profile.jpg', 'JPEG', quality=90, icc_profile=icc)
print(f'  wrote a {len(icc)}-byte ICC profile')
"

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
# 6b. GIF inputs (decode-only format)
# ---------------------------------------------------------------------------

echo "[gif] sample.gif — 4x3 static GIF87a"
magick -size 4x3 xc:'rgb(255,0,0)' GIF87:"$DIR/sample.gif"

echo "[gif] transparent.gif — 4x4 static GIF89a with a transparent color index"
magick -size 4x4 xc:'rgba(0,0,0,0)' -type PaletteAlpha GIF:"$DIR/transparent.gif"

echo "[gif] animated.gif — 3-frame animated GIF89a"
magick -delay 10 -size 4x3 \
  xc:red xc:green xc:blue \
  -loop 0 "$DIR/animated.gif"

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
