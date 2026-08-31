#!/usr/bin/env python3
"""Insert a clean aperture (clap) into an AVIF's primary item.

No encoder available to generate-fixtures.sh writes the box, so the fixtures that carry
one are made by patching a file that ImageMagick wrote: a `clap` box goes into `ipco`, the
primary item's `ipma` entry gains an essential association to it in the position MIAF
requires (after the descriptive properties, before `irot` and `imir`), and every box size
and `iloc` offset the insertion moved is corrected.

Usage: avif-add-clap.py SRC DST WIDTH HEIGHT [HORIZONTAL_OFFSET VERTICAL_OFFSET]

The aperture is WIDTH by HEIGHT pixels, centred unless offsets are given, which are the
distances from the picture centre to the aperture centre in pixels.
"""

import struct
import sys


def boxes(buf, start, end):
    """Yields (type, offset, size) for each box in buf[start:end]."""
    offset = start
    while offset + 8 <= end:
        size, kind = struct.unpack(">I4s", buf[offset : offset + 8])
        if size == 0:
            size = end - offset
        yield kind, offset, size
        offset += size


def find(buf, path, start, end, header=0):
    """Returns (offset, size) of the box at path, a list of box types from the top level."""
    for kind, offset, size in boxes(buf, start + header, end):
        if kind == path[0]:
            if len(path) == 1:
                return offset, size
            child_header = 8 + (4 if kind == b"meta" else 0)
            return find(buf, path[1:], offset, offset + size, child_header)
    raise KeyError(path)


def grow(buf, offset, delta):
    size = struct.unpack(">I", buf[offset : offset + 4])[0]
    buf[offset : offset + 4] = struct.pack(">I", size + delta)


def main(argv):
    if len(argv) not in (5, 7):
        sys.exit(__doc__)
    src, dst = argv[1], argv[2]
    aperture_width, aperture_height = int(argv[3]), int(argv[4])
    horizontal, vertical = (int(argv[5]), int(argv[6])) if len(argv) == 7 else (0, 0)
    buf = bytearray(open(src, "rb").read())

    meta, _ = find(buf, [b"meta"], 0, len(buf))
    iprp, _ = find(buf, [b"meta", b"iprp"], 0, len(buf))
    ipco, ipco_size = find(buf, [b"meta", b"iprp", b"ipco"], 0, len(buf))
    iloc, _ = find(buf, [b"meta", b"iloc"], 0, len(buf))
    property_types = [kind for kind, _, _ in boxes(buf, ipco + 8, ipco + ipco_size)]

    # clap: width, height, horizontal offset, vertical offset, each numerator over denominator.
    payload = struct.pack(
        ">IIIIiIiI", aperture_width, 1, aperture_height, 1, horizontal, 1, vertical, 1
    )
    clap = struct.pack(">I4s", 8 + len(payload), b"clap") + payload
    buf[ipco + ipco_size : ipco + ipco_size] = clap
    for offset in (ipco, iprp, meta):
        grow(buf, offset, len(clap))
    clap_index = len(property_types) + 1

    ipma, _ = find(buf, [b"meta", b"iprp", b"ipma"], 0, len(buf))
    version, flags = buf[ipma + 8], buf[ipma + 11]
    entry_count = struct.unpack(">I", buf[ipma + 12 : ipma + 16])[0]
    index_width = 2 if flags & 1 else 1
    cursor = ipma + 16
    added = 0
    for _ in range(entry_count):
        if version == 0:
            item = struct.unpack(">H", buf[cursor : cursor + 2])[0]
            cursor += 2
        else:
            item = struct.unpack(">I", buf[cursor : cursor + 4])[0]
            cursor += 4
        count = buf[cursor]
        count_at = cursor
        cursor += 1
        if item != 1:
            cursor += count * index_width
            continue
        indices = []
        for k in range(count):
            at = cursor + k * index_width
            raw = struct.unpack(">H", buf[at : at + 2])[0] if index_width == 2 else buf[at]
            indices.append(raw & (0x7FFF if index_width == 2 else 0x7F))
        position = next(
            (k for k, index in enumerate(indices) if property_types[index - 1] in (b"irot", b"imir")),
            count,
        )
        at = cursor + position * index_width
        entry = (
            struct.pack(">H", 0x8000 | clap_index)
            if index_width == 2
            else bytes([0x80 | clap_index])
        )
        buf[at:at] = entry
        buf[count_at] = count + 1
        added = len(entry)
        break
    if not added:
        sys.exit("no ipma entry for item 1")
    grow(buf, ipma, added)
    for offset in (iprp, meta):
        grow(buf, offset, added)

    # The media data moved down by everything inserted, so the iloc offsets follow it.
    shift = len(clap) + added
    version = buf[iloc + 8]
    offset_size, length_size = buf[iloc + 12] >> 4, buf[iloc + 12] & 0xF
    base_offset_size = buf[iloc + 13] >> 4
    index_size = buf[iloc + 13] & 0xF if version in (1, 2) else 0
    cursor = iloc + 14
    if version < 2:
        item_count = struct.unpack(">H", buf[cursor : cursor + 2])[0]
        cursor += 2
    else:
        item_count = struct.unpack(">I", buf[cursor : cursor + 4])[0]
        cursor += 4
    for _ in range(item_count):
        cursor += 2 if version < 2 else 4
        if version in (1, 2):
            cursor += 2
        cursor += 2
        base = int.from_bytes(buf[cursor : cursor + base_offset_size], "big")
        if base_offset_size and base:
            buf[cursor : cursor + base_offset_size] = (base + shift).to_bytes(base_offset_size, "big")
        cursor += base_offset_size
        extent_count = struct.unpack(">H", buf[cursor : cursor + 2])[0]
        cursor += 2
        for _ in range(extent_count):
            cursor += index_size
            if offset_size and not (base_offset_size and base):
                extent = int.from_bytes(buf[cursor : cursor + offset_size], "big")
                buf[cursor : cursor + offset_size] = (extent + shift).to_bytes(offset_size, "big")
            cursor += offset_size + length_size

    open(dst, "wb").write(buf)


if __name__ == "__main__":
    main(sys.argv)
