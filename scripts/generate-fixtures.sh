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
# A BITMAPV4HEADER, not BMP3: the Windows 3.x header has no alpha mask, so ImageMagick
# wrote this fixture as 24-bit with the alpha discarded and every test named for it was
# about an opaque file. The V4 header carries the mask, and the pixels are half-transparent
# red so that alpha is observable in every output: kept in PNG and BMP, composited in JPEG.
magick -size 4x4 xc:'rgba(255,0,0,0.5)' -define bmp:format=bmp4 "$DIR/transparent.bmp"

echo "[5/18] 1px.png — minimum dimension (1x1)"
magick -size 1x1 xc:red "$DIR/1px.png"

echo "[4/14] large.png — 10000x1 wide image"
magick -size 10000x1 xc:blue "$DIR/large.png"

echo "[5/14] tall.png — 1x10000 tall image"
magick -size 1x10000 xc:green "$DIR/tall.png"

echo "[5b/18] indexed.png — 128x128 PNG-8 (colour type 3, 16-colour palette)"
# Every other PNG fixture is truecolour, which is why an indexed input going through the
# pipeline as RGB went unnoticed: there is no encoder for colour type 3 here, so a
# same-format pass used to come back 45% larger than it started.
python3 -c "
from PIL import Image

image = Image.new('RGB', (128, 128))
for x in range(128):
    for y in range(128):
        image.putpixel((x, y), ((x // 16) * 32, (y // 16) * 32, 64))
image.convert('P', palette=Image.ADAPTIVE, colors=16).save('$DIR/indexed.png', optimize=True)
print('  wrote a 16-colour indexed PNG')
"

echo "[5c/18] flat.jpg — 256x256 single-colour JPEG"
# An encoder does not always beat whatever produced the input. A flat colour is the
# everyday case where it does not: a colour swatch, a placeholder, a document scan.
python3 -c "
from PIL import Image

Image.new('RGB', (256, 256), (30, 80, 200)).save('$DIR/flat.jpg', quality=85)
print('  wrote a 256x256 flat JPEG')
"

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

echo "[8*/14] exif-rotated.png / exif-rotated.webp — the same tag in the other containers"
# PNG carries the tag in an eXIf chunk and WebP in an EXIF chunk, and browsers honour it in
# both, so truss reads it in both. Same picture and same tag as exif-rotated.jpg, so a test
# can ask the three containers the same question and expect one answer.
python3 -c "
from PIL import Image

image = Image.new('RGB', (40, 20), (255, 0, 0))
for x in range(10):
    for y in range(20):
        image.putpixel((x, y), (0, 0, 255))
exif = image.getexif()
exif[274] = 6  # Orientation: rotate 90 degrees clockwise
image.save('$DIR/exif-rotated.png', exif=exif.tobytes())
image.save('$DIR/exif-rotated.webp', exif=exif.tobytes(), lossless=True)
print('  wrote 40x20 PNG and WebP with EXIF Orientation=6')
"

echo "[8a/14] exif-transposed-5.jpg / exif-transposed-7.jpg — orientations 5 and 7"
# Orientations 5 to 8 all turn a 40x20 image into a 20x40 one, so dimensions alone cannot
# tell them apart. These two are the pair that a mirrored transform gets backwards: 5 is
# "mirror horizontal and rotate 270 CW", 7 is "mirror horizontal and rotate 90 CW". The
# marker bars along the top and the left edge make the difference visible in the output.
python3 -c "
from PIL import Image

for orientation in (5, 7):
    image = Image.new('RGB', (40, 20), (255, 255, 255))
    for x in range(12):
        for y in range(4):
            image.putpixel((x, y), (255, 0, 0))
    for x in range(4):
        for y in range(12):
            image.putpixel((x, y), (0, 0, 255))
    exif = image.getexif()
    exif[274] = orientation
    image.save(f'$DIR/exif-transposed-{orientation}.jpg', exif=exif.tobytes(), quality=95)
    print(f'  wrote 40x20 with EXIF Orientation={orientation}')
"

echo "[8c/14] irot-rotated.avif / imir-transposed-5.avif — the same orientations as AVIF item properties"
# AVIF has no EXIF orientation of its own: an encoder writes the transform as the irot and
# imir item properties of the primary item, and browsers apply those and ignore any Exif
# item. `-orient` sets ImageMagick's orientation without minting an EXIF profile, so
# libheif writes the properties and nothing else, which is what proves truss reads the
# properties rather than a tag. Same pictures as exif-rotated.jpg and exif-transposed-5.jpg,
# so the same assertions and the same baseline apply. `-alpha off` keeps the drawing from
# growing an alpha plane, which would be a second item with its own properties, and
# `-depth 8` keeps these about orientation: a 16-bit canvas would otherwise come out as a
# 12-bit AVIF, which is what the deep fixtures below are for.
magick -size 40x20 xc:red -fill blue -draw 'rectangle 0,0 9,19' \
  -alpha off -depth 8 -orient right-top "$DIR/irot-rotated.avif"
magick -size 40x20 xc:white -fill red -draw 'rectangle 0,0 11,3' -fill blue -draw 'rectangle 0,0 3,11' \
  -alpha off -depth 8 -orient left-top "$DIR/imir-transposed-5.avif"

echo "[8c*/14] clap-cropped.avif / clap-rotated.avif — the same pictures behind a clean aperture"
# clap is the container-level crop MIAF defines alongside irot and imir, applied before
# either. No encoder here writes the box, so avif-add-clap.py patches it into a file libheif
# wrote: the property, the essential association in the position MIAF requires, and the
# box sizes and iloc offsets the insertion moved. Both keep the middle 30 columns of the
# 40x20 picture, so 5 of the 10 blue columns survive; the rotated one then turns that cut.
magick -size 40x20 xc:red -fill blue -draw 'rectangle 0,0 9,19' \
  -alpha off -depth 8 "$DIR/clap-base.avif"
python3 "$(dirname "$0")/avif-add-clap.py" "$DIR/clap-base.avif" "$DIR/clap-cropped.avif" 30 20
python3 "$(dirname "$0")/avif-add-clap.py" "$DIR/irot-rotated.avif" "$DIR/clap-rotated.avif" 30 20
rm -f "$DIR/clap-base.avif"

echo "[8d/14] deep-10bit.avif / deep-12bit.avif — high bit depth AVIF with saturated samples"
# The image crate writes 8-bit AVIF only, so nothing in the test suite reached the 10/12-bit
# decode path until these. A blue left half, a red right half, and a white bar along the
# top put a sample at the top of its range in U, in V, and in Y, which is what rounded past
# 8 bits and wrapped to zero. The 8-bit baseline of the same picture lives in
# e2e/atago/testdata/deep-avif.png.
for depth in 10 12; do
  magick -size 40x20 xc:red -fill blue -draw 'rectangle 0,0 19,19' -fill white -draw 'rectangle 0,0 39,3' \
    -alpha off -depth "$depth" "$DIR/deep-${depth}bit.avif"
done

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

echo "[bonus] svg-animate-xss.svg — SMIL animation that sets href at render time"
# The attribute filter only inspects `href`. SMIL writes it after the fact through `to`,
# `values`, `from`, or `by`, which is how the canonical payload used to pass through a
# sanitizer that removes `<script>` and `on*` handlers. The <set> element does the same for
# an external reference, which the sanitizer also promises to remove.
cat > "$DIR/svg-animate-xss.svg" << 'SVGEOF'
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <a>
    <animate attributeName="href" values="javascript:alert(document.domain)" begin="0s" dur="1s" fill="freeze"/>
    <text x="10" y="20">click me</text>
  </a>
  <image x="0" y="0" width="100" height="100">
    <set attributeName="href" to="https://evil.example.com/track.png" begin="0s"/>
  </image>
  <handler type="text/javascript">alert('handler')</handler>
</svg>
SVGEOF

echo "[bonus] svg-illustrator-prolog.svg — the XML prolog Adobe Illustrator writes"
# A generator comment before the doctype and a doctype carrying an internal subset are
# both legal prolog, and both are what a real editor emits. Detection walked a fixed
# sequence instead of the grammar and refused the file as an unknown signature.
cat > "$DIR/svg-illustrator-prolog.svg" << 'SVGEOF'
<?xml version="1.0" encoding="utf-8"?>
<!-- Generator: Adobe Illustrator 27.0.0, SVG Export Plug-In . SVG Version: 6.00 Build 0)  -->
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
	<!ENTITY ns_extend "http://ns.adobe.com/Extensibility/1.0/">
	<!ENTITY ns_ai "http://ns.adobe.com/AdobeIllustrator/10.0/">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <rect width="100" height="100" fill="green"/>
</svg>
SVGEOF

echo "[bonus] svg-external-css.svg — external references the element and href rules do not see"
# Three spellings of the same idea, none of which is an element or an href: a stylesheet
# processing instruction the browser honours, a presentation attribute carrying the url()
# that would have been stripped from `style`, and an @import whose at-keyword is written
# with a CSS escape so a literal search for "@import" misses it.
cat > "$DIR/svg-external-css.svg" << 'SVGEOF'
<?xml version="1.0" encoding="UTF-8"?>
<?xml-stylesheet type="text/xsl" href="https://evil.example.com/x.xsl"?>
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <style>@\69 mport "https://evil.example.com/x.css";</style>
  <rect width="100" height="100" fill="url(https://evil.example.com/paint.svg#g)"
        filter="url(https://evil.example.com/filter.svg#f)"/>
</svg>
SVGEOF

echo "[bonus] svg-minimal.svg — smallest valid SVG"
cat > "$DIR/svg-minimal.svg" << 'SVGEOF'
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>
SVGEOF

echo ""
echo "=== Done: $(ls "$DIR" | wc -l) fixtures in $DIR ==="
ls -lhS "$DIR"
