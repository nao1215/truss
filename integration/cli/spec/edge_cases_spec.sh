# shellcheck shell=sh
# ---------------------------------------------------------------------------
# edge_cases_spec.sh - Edge-case and adversarial image handling
#
# Real-world image processing must handle broken, unusual, and adversarial
# inputs gracefully. These tests verify that truss handles edge cases
# without crashing, hanging, or producing garbage output.
#
# Fixture overview:
#   1px.png           1x1 minimum-dimension PNG
#   large.png         10000x1 extremely wide PNG
#   tall.png          1x10000 extremely tall PNG
#   transparent.png   4x4 fully transparent PNG (all alpha=0)
#   semitransparent.png  8x8 with partial alpha
#   exif-rotated.jpg  JPEG with EXIF Orientation=6 (90° CW)
#   cmyk.jpg          CMYK color space JPEG
#   truncated.jpg     JPEG truncated mid-stream (~60% of data)
#   corrupt-header.jpg  JPEG with zeroed header bytes
#   invalid-chunk.png   PNG with invalid chunk type and bad CRC
#   zero-bytes.bin    Empty file (0 bytes)
#   random-noise.bin  256 bytes of random data
# ---------------------------------------------------------------------------

Describe "Edge-case image handling"
  setup() {
    WORK_DIR="${SHELLSPEC_TMPDIR}/truss-edge-$$"
    mkdir -p "$WORK_DIR"
  }

  cleanup() {
    rm -rf "$WORK_DIR"
  }

  Before "setup"
  After "cleanup"

  # -------------------------------------------------------------------------
  # Dimension extremes
  # -------------------------------------------------------------------------

  Describe "Minimum dimension (1x1)"
    It "inspects a 1x1 PNG"
      When run command truss inspect "${FIXTURES_DIR}/1px.png"
      The status should eq 0
      The output should include '"width": 1'
      The output should include '"height": 1'
    End

    It "converts a 1x1 PNG to JPEG"
      When run command truss convert "${FIXTURES_DIR}/1px.png" -o "${WORK_DIR}/1px.jpg"
      The status should eq 0
      The path "${WORK_DIR}/1px.jpg" should be file
    End
  End

  Describe "Extreme width (10000x1)"
    It "inspects a 10000x1 PNG"
      When run command truss inspect "${FIXTURES_DIR}/large.png"
      The status should eq 0
      The output should include '"width": 10000'
      The output should include '"height": 1'
    End

    It "converts a 10000x1 PNG to JPEG"
      When run command truss convert "${FIXTURES_DIR}/large.png" -o "${WORK_DIR}/large.jpg"
      The status should eq 0
      The path "${WORK_DIR}/large.jpg" should be file
    End
  End

  Describe "Extreme height (1x10000)"
    It "inspects a 1x10000 PNG"
      When run command truss inspect "${FIXTURES_DIR}/tall.png"
      The status should eq 0
      The output should include '"width": 1'
      The output should include '"height": 10000'
    End

    It "converts a 1x10000 PNG to JPEG"
      When run command truss convert "${FIXTURES_DIR}/tall.png" -o "${WORK_DIR}/tall.jpg"
      The status should eq 0
      The path "${WORK_DIR}/tall.jpg" should be file
    End
  End

  # -------------------------------------------------------------------------
  # Alpha / transparency
  # -------------------------------------------------------------------------

  Describe "Fully transparent PNG"
    It "inspects a fully transparent PNG"
      When run command truss inspect "${FIXTURES_DIR}/transparent.png"
      The status should eq 0
      The output should include '"hasAlpha": true'
    End

    It "converts a fully transparent PNG to JPEG without error"
      # JPEG does not support alpha — truss should composite over a background
      When run command truss convert "${FIXTURES_DIR}/transparent.png" -o "${WORK_DIR}/transparent.jpg"
      The status should eq 0
      The path "${WORK_DIR}/transparent.jpg" should be file
    End
  End

  Describe "Semi-transparent PNG"
    It "converts a semi-transparent PNG to JPEG"
      When run command truss convert "${FIXTURES_DIR}/semitransparent.png" -o "${WORK_DIR}/semi.jpg"
      The status should eq 0
      The path "${WORK_DIR}/semi.jpg" should be file
    End
  End

  # -------------------------------------------------------------------------
  # EXIF orientation
  # -------------------------------------------------------------------------

  Describe "EXIF-rotated JPEG"
    It "inspects an EXIF-rotated JPEG"
      When run command truss inspect "${FIXTURES_DIR}/exif-rotated.jpg"
      The status should eq 0
      The output should include '"format": "jpeg"'
    End

    It "converts an EXIF-rotated JPEG to PNG"
      When run command truss convert "${FIXTURES_DIR}/exif-rotated.jpg" -o "${WORK_DIR}/exif.png"
      The status should eq 0
      The path "${WORK_DIR}/exif.png" should be file
    End
  End

  # -------------------------------------------------------------------------
  # CMYK color space
  # -------------------------------------------------------------------------

  Describe "CMYK JPEG"
    It "inspects a CMYK JPEG"
      When run command truss inspect "${FIXTURES_DIR}/cmyk.jpg"
      The status should eq 0
      The output should include '"format": "jpeg"'
    End

    It "converts a CMYK JPEG to PNG"
      When run command truss convert "${FIXTURES_DIR}/cmyk.jpg" -o "${WORK_DIR}/cmyk.png"
      The status should eq 0
      The path "${WORK_DIR}/cmyk.png" should be file
    End
  End

  # -------------------------------------------------------------------------
  # Truncated / corrupted files
  # -------------------------------------------------------------------------

  Describe "Truncated JPEG"
    It "inspect may succeed on truncated JPEG (header intact)"
      # The JPEG header is valid so inspect can read dimensions,
      # but the image data is incomplete.
      When run command truss inspect "${FIXTURES_DIR}/truncated.jpg"
      The status should eq 0
      The stdout should include '"format"'
    End

    It "convert fails on truncated JPEG (exit 4 — transform/decode error)"
      When run command truss convert "${FIXTURES_DIR}/truncated.jpg" -o "${WORK_DIR}/trunc.png"
      The status should eq 4
      The stderr should include "error:"
    End
  End

  Describe "Corrupt JPEG header"
    It "inspects a corrupt-header JPEG (graceful handling)"
      # The corruption is mild enough that the decoder still works
      When run command truss inspect "${FIXTURES_DIR}/corrupt-header.jpg"
      The status should eq 0
      The stdout should include '"format"'
    End

    It "converts a corrupt-header JPEG"
      When run command truss convert "${FIXTURES_DIR}/corrupt-header.jpg" -o "${WORK_DIR}/corrupt.png"
      The status should eq 0
    End
  End

  Describe "Invalid PNG chunk"
    It "inspects a PNG with invalid chunk (tolerant decoder)"
      When run command truss inspect "${FIXTURES_DIR}/invalid-chunk.png"
      The status should eq 0
      The stdout should include '"format"'
    End

    It "converts a PNG with invalid chunk"
      When run command truss convert "${FIXTURES_DIR}/invalid-chunk.png" -o "${WORK_DIR}/badchunk.jpg"
      The status should eq 0
    End
  End

  # -------------------------------------------------------------------------
  # Non-image files
  # -------------------------------------------------------------------------

  Describe "Empty file (zero bytes)"
    It "rejects an empty file (exit 3 — unsupported format)"
      When run command truss inspect "${FIXTURES_DIR}/zero-bytes.bin"
      The status should eq 3
      The stderr should include "error:"
    End
  End

  Describe "Random noise (not an image)"
    It "rejects random binary data (exit 3 — unsupported format)"
      When run command truss inspect "${FIXTURES_DIR}/random-noise.bin"
      The status should eq 3
      The stderr should include "error:"
    End
  End
End
