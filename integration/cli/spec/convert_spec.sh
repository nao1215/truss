# shellcheck shell=sh
# ---------------------------------------------------------------------------
# convert_spec.sh - The truss convert command
#
# The convert command transforms images between formats, resizes them, and
# applies other transformations. It is the primary command of truss.
#
# Synopsis:
#   truss convert <INPUT> -o <OUTPUT> [OPTIONS]
#   truss <INPUT> -o <OUTPUT> [OPTIONS]          (implicit convert)
#   truss convert - -o - --format <FMT>          (stdin/stdout)
# ---------------------------------------------------------------------------

Describe "Convert command"
  setup() {
    WORK_DIR="${SHELLSPEC_TMPDIR}/truss-convert-$$"
    mkdir -p "$WORK_DIR"
  }

  cleanup() {
    rm -rf "$WORK_DIR"
  }

  Before "setup"
  After "cleanup"

  Describe "Format conversion"
    It "converts PNG to JPEG"
      When run command truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/out.jpg"
      The status should eq 0
      The path "${WORK_DIR}/out.jpg" should be file
    End

    It "produces a valid JPEG file"
      # Pre-run: create the file so we can inspect it
      truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/check.jpg"
      When call is_jpeg "${WORK_DIR}/check.jpg"
      The status should eq 0
    End
  End

  Describe "Resizing"
    It "resizes to a target width"
      When run command truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/resized.png" --width 2
      The status should eq 0
      The path "${WORK_DIR}/resized.png" should be file
    End

    It "produces an image with the requested width"
      truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/w2.png" --width 2
      When call image_width "${WORK_DIR}/w2.png"
      The output should eq "2"
    End
  End

  Describe "Optimization"
    It "optimizes PNG losslessly via the optimize subcommand"
      When run command truss optimize "$SAMPLE_PNG" -o "${WORK_DIR}/optimized.png" --mode lossless
      The status should eq 0
      The path "${WORK_DIR}/optimized.png" should be file
    End
  End

  Describe "Implicit subcommand"
    It "converts without the 'convert' keyword (truss <INPUT> -o <OUTPUT>)"
      When run command truss "$SAMPLE_PNG" -o "${WORK_DIR}/implicit.jpg"
      The status should eq 0
      The path "${WORK_DIR}/implicit.jpg" should be file
    End
  End

  Describe "Stdin/stdout piping"
    It "reads from stdin (-) and writes to a file"
      Data
        #|$(cat "$SAMPLE_PNG")
      End
      When run command sh -c "cat '$SAMPLE_PNG' | truss convert - -o '${WORK_DIR}/piped.png' --format png"
      The status should eq 0
      The path "${WORK_DIR}/piped.png" should be file
    End

    It "reads from stdin and writes to stdout"
      When run command sh -c "cat '$SAMPLE_PNG' | truss convert - -o - --format png > '${WORK_DIR}/stdout.png'"
      The status should eq 0
      The path "${WORK_DIR}/stdout.png" should be file
    End
  End

  Describe "Error handling"
    It "exits 1 with error, usage, and hint when --output is missing"
      When run command truss convert "$SAMPLE_PNG"
      The status should eq 1
      The stderr should include "error:"
      The stderr should include "usage:"
      The stderr should include "hint:"
    End

    It "exits 1 when an unknown option is given"
      When run command truss convert --badopt foo
      The status should eq 1
      The stderr should include "error:"
    End

    It "treats -- as end-of-options (double dash)"
      # The file -input.png does not exist, but truss should NOT treat it
      # as an option — it should fail with I/O error, not unknown-option.
      When run command truss convert -o "${WORK_DIR}/dd.jpg" -- -input.png
      The status should eq 2
      The stderr should include "error:"
    End
  End
End
