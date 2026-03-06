# shellcheck shell=sh
# ---------------------------------------------------------------------------
# exit_code_spec.sh - Documented exit codes
#
# truss uses structured exit codes so that scripts and CI pipelines can
# distinguish between different failure modes:
#
#   0  Success
#   1  Usage error (bad arguments, missing required flags)
#   2  I/O error (file not found, permission denied, network failure)
#   3  Input error (unsupported format, corrupt file)
#   4  Transform error (encode failure, size limit exceeded, deadline)
# ---------------------------------------------------------------------------

Describe "Exit codes"
  setup() {
    WORK_DIR="${SHELLSPEC_TMPDIR}/truss-exit-$$"
    mkdir -p "$WORK_DIR"
  }

  cleanup() {
    rm -rf "$WORK_DIR"
  }

  Before "setup"
  After "cleanup"

  Context "exit 0 - Success"
    It "returns 0 for help"
      When run command truss --help
      The status should eq 0
      The stdout should include "USAGE"
    End

    It "returns 0 for a successful conversion"
      When run command truss convert "$SAMPLE_PNG" -o "${WORK_DIR}/ok.jpg"
      The status should eq 0
    End

    It "returns 0 for a successful inspection"
      When run command truss inspect "$SAMPLE_PNG"
      The status should eq 0
      The stdout should include '"format"'
    End
  End

  Context "exit 1 - Usage error (bad arguments)"
    It "returns 1 when convert is missing --output"
      When run command truss convert "$SAMPLE_PNG"
      The status should eq 1
      The stderr should include "error:"
    End

    It "returns 1 for an unknown command"
      When run command truss notacommand
      The status should eq 1
      The stderr should include "error:"
    End

    It "returns 1 when inspect has no input"
      When run command truss inspect
      The status should eq 1
      The stderr should include "error:"
    End

    It "returns 1 when sign has no arguments"
      When run command truss sign
      The status should eq 1
      The stderr should include "error:"
    End
  End

  Context "exit 2 - I/O error (file not found)"
    It "returns 2 when the input file does not exist"
      When run command truss inspect /no/such/file.png
      The status should eq 2
      The stderr should include "error:"
    End

    It "returns 2 when the convert input file does not exist"
      When run command truss convert /no/such/file.png -o "${WORK_DIR}/out.jpg"
      The status should eq 2
      The stderr should include "error:"
    End
  End

  Context "exit 3 - Input error (corrupt or unsupported data)"
    It "returns 3 when given corrupt image data"
      printf "this is not an image" > "${WORK_DIR}/corrupt.dat"
      When run command truss inspect "${WORK_DIR}/corrupt.dat"
      The status should eq 3
      The stderr should include "error:"
    End

    It "returns 3 for an empty file (zero bytes)"
      When run command truss inspect "${FIXTURES_DIR}/zero-bytes.bin"
      The status should eq 3
      The stderr should include "error:"
    End

    It "returns 3 for random binary data"
      When run command truss inspect "${FIXTURES_DIR}/random-noise.bin"
      The status should eq 3
      The stderr should include "error:"
    End
  End

  Context "exit 4 - Transform error (decode failure)"
    It "returns 4 when converting a truncated JPEG"
      When run command truss convert "${FIXTURES_DIR}/truncated.jpg" -o "${WORK_DIR}/trunc.png"
      The status should eq 4
      The stderr should include "error:"
    End
  End
End
