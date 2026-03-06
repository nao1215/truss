# shellcheck shell=sh
# ---------------------------------------------------------------------------
# bmp_spec.sh - BMP format support
#
# Tests BMP input detection, inspection, conversion to/from other formats,
# and error handling for BMP-specific edge cases.
# ---------------------------------------------------------------------------

Describe "BMP format support"
  setup() {
    WORK_DIR="${SHELLSPEC_TMPDIR}/truss-bmp-$$"
    mkdir -p "$WORK_DIR"
  }

  cleanup() {
    rm -rf "$WORK_DIR"
  }

  Before "setup"
  After "cleanup"

  Describe "Inspect"
    It "detects BMP format and dimensions"
      When run command truss inspect "$SAMPLE_BMP"
      The status should eq 0
      The output should include '"format": "bmp"'
      The output should include '"width": 4'
      The output should include '"height": 3'
    End
  End

  Describe "Convert from BMP"
    It "converts BMP to PNG"
      When run command truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/out.png"
      The status should eq 0
      The path "${WORK_DIR}/out.png" should be file
    End

    It "produces a valid PNG from BMP"
      truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/check.png"
      When call is_png "${WORK_DIR}/check.png"
      The status should eq 0
    End

    It "converts BMP to JPEG"
      When run command truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/out.jpg"
      The status should eq 0
      The path "${WORK_DIR}/out.jpg" should be file
    End

    It "produces a valid JPEG from BMP"
      truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/check.jpg"
      When call is_jpeg "${WORK_DIR}/check.jpg"
      The status should eq 0
    End
  End

  Describe "Convert to BMP"
    It "converts PNG to BMP"
      When run command truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/out.bmp"
      The status should eq 0
      The path "${WORK_DIR}/out.bmp" should be file
    End

    It "produces a valid BMP file"
      truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/check.bmp"
      When call is_bmp "${WORK_DIR}/check.bmp"
      The status should eq 0
    End

    It "converts JPEG to BMP"
      When run command truss convert "$SAMPLE_JPG" -o "${WORK_DIR}/out.bmp" --format bmp
      The status should eq 0
      The path "${WORK_DIR}/out.bmp" should be file
    End
  End

  Describe "Resizing"
    It "resizes BMP to a target width"
      When run command truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/resized.png" --width 2
      The status should eq 0
      The path "${WORK_DIR}/resized.png" should be file
    End

    It "produces an image with the requested width"
      truss convert "$SAMPLE_BMP" -o "${WORK_DIR}/w2.png" --width 2
      When call image_width "${WORK_DIR}/w2.png"
      The output should eq "2"
    End
  End

  Describe "Round-trip"
    It "round-trips PNG to BMP and back to PNG"
      truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/step1.bmp"
      When run command truss convert "${WORK_DIR}/step1.bmp" -o "${WORK_DIR}/step2.png"
      The status should eq 0
      The path "${WORK_DIR}/step2.png" should be file
    End
  End

  Describe "Implicit subcommand"
    It "converts BMP without the 'convert' keyword"
      When run command truss "$SAMPLE_BMP" -o "${WORK_DIR}/implicit.png"
      The status should eq 0
      The path "${WORK_DIR}/implicit.png" should be file
    End
  End
End
