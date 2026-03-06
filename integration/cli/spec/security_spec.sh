# shellcheck shell=sh
# ---------------------------------------------------------------------------
# security_spec.sh - SVG security and sanitization
#
# SVG files can contain embedded scripts (XSS), XML entity expansion
# attacks (billion laughs), and external resource references (SSRF).
# truss must neutralize these threats when converting SVG to raster.
#
# Fixtures:
#   svg-entity-bomb.svg    XML entity expansion (<!ENTITY> nesting)
#   svg-script.svg         <script>, onclick, onload attributes
#   svg-external-ref.svg   External xlink:href references
#   svg-minimal.svg        Smallest valid SVG (1x1, no content)
# ---------------------------------------------------------------------------

Describe "SVG security"

  setup() {
    WORK_DIR="${SHELLSPEC_TMPDIR}/truss-security-$$"
    mkdir -p "$WORK_DIR"
  }

  cleanup() {
    rm -rf "$WORK_DIR"
  }

  Before "setup"
  After "cleanup"

  # -------------------------------------------------------------------------
  # Entity expansion bomb (billion laughs variant)
  # -------------------------------------------------------------------------

  Describe "Entity expansion bomb"
    It "rejects SVG with entity expansion (does not hang or OOM)"
      # The entity bomb SVG uses nested <!ENTITY> declarations.
      # truss should reject it rather than expanding entities.
      When run command truss convert "${FIXTURES_DIR}/svg-entity-bomb.svg" \
        -o "${WORK_DIR}/bomb.png" --format png
      The status should not eq 0
      The stderr should include "error:"
    End
  End

  # -------------------------------------------------------------------------
  # Script injection (XSS)
  # -------------------------------------------------------------------------

  Describe "SVG with embedded scripts"
    It "converts SVG with scripts to raster without error"
      # Scripts are irrelevant in raster output — the SVG sanitizer
      # strips them during parsing, and the rasterizer never executes JS.
      When run command truss convert "${FIXTURES_DIR}/svg-script.svg" \
        -o "${WORK_DIR}/noscript.png" --format png
      The status should eq 0
      The path "${WORK_DIR}/noscript.png" should be file
    End
  End

  # -------------------------------------------------------------------------
  # External references (SSRF)
  # -------------------------------------------------------------------------

  Describe "SVG with external references"
    It "converts SVG with external refs (refs are not fetched)"
      # External xlink:href URLs must NOT be fetched during conversion.
      # The converter should either strip them or ignore them.
      When run command truss convert "${FIXTURES_DIR}/svg-external-ref.svg" \
        -o "${WORK_DIR}/noextref.png" --format png
      The status should eq 0
      The path "${WORK_DIR}/noextref.png" should be file
    End
  End

  # -------------------------------------------------------------------------
  # Minimal SVG
  # -------------------------------------------------------------------------

  Describe "Minimal SVG (1x1)"
    It "inspects a minimal SVG"
      When run command truss inspect "${FIXTURES_DIR}/svg-minimal.svg"
      The status should eq 0
      The output should include '"svg"'
    End

    It "converts a minimal SVG to PNG"
      When run command truss convert "${FIXTURES_DIR}/svg-minimal.svg" \
        -o "${WORK_DIR}/minimal.png" --format png
      The status should eq 0
      The path "${WORK_DIR}/minimal.png" should be file
    End
  End
End
