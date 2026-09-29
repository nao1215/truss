#!/bin/sh
# Writes a deterministic RGB PNG for the himorime suite in this directory.
#
#   sh gen.sh png <width>x<height> <out>
#
# The picture is a diagonal gradient with fine noise from a fixed seed, so
# it compresses and resizes like a photograph rather than a flat colour, and
# every run (and both revisions of a comparison) reads the same bytes. It is
# written with the Python standard library only, and fast enough for a
# 12-megapixel image: each row is a slice of one of a few noise rows,
# brightened by a byte translation table, so the per-pixel work runs in C.
set -eu

if [ "$#" -ne 3 ] || [ "$1" != png ]; then
	echo "usage: sh gen.sh png <width>x<height> <out>" >&2
	exit 1
fi

exec python3 - "$2" "$3" <<'EOF'
import random
import struct
import sys
import zlib

size, out = sys.argv[1], sys.argv[2]
width, height = (int(v) for v in size.split("x"))

rng = random.Random(1215)
shift = 97
# 64 rows of a horizontal gradient with noise, each long enough to be read
# at any of `shift` offsets. Picking one of them at random for every row
# keeps zlib from finding the previous rows again inside its 32 KiB window,
# so the file is as large as a noisy photograph's, not a pattern's.
lines = []
for _ in range(64):
    line = bytearray()
    for x in range(width + shift):
        base = x * 160 // (width + shift)
        for channel in (0, 40, 90):
            line.append((base + channel + rng.randrange(24)) & 0xFF)
    lines.append(bytes(line))

tables = {}
raw = bytearray()
for y in range(height):
    offset = y * 96 // height
    table = tables.get(offset)
    if table is None:
        table = bytes((v + offset) & 0xFF for v in range(256))
        tables[offset] = table
    start = 3 * rng.randrange(shift)
    raw.append(0)  # filter type: none
    raw += lines[rng.randrange(64)][start:start + 3 * width].translate(table)


def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


with open(out, "wb") as f:
    f.write(b"\x89PNG\r\n\x1a\n")
    f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
    f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 6)))
    f.write(chunk(b"IEND", b""))
EOF
