# shellcheck shell=sh
# ---------------------------------------------------------------------------
# inspect_spec.sh - The truss inspect command
#
# The inspect command prints JSON metadata about an image: format, MIME type,
# dimensions, alpha channel presence, and animation status.
#
# Synopsis:
#   truss inspect <FILE>
#   truss inspect -        (read from stdin)
# ---------------------------------------------------------------------------

Describe "Inspect command"
  Describe "Local file inspection"
    It "prints JSON metadata for a PNG file"
      When run command truss inspect "$SAMPLE_PNG"
      The status should eq 0
      The output should include '"format"'
      The output should include '"width"'
      The output should include '"height"'
    End

    It "reports the correct format as png"
      When run command truss inspect "$SAMPLE_PNG"
      The output should include '"png"'
    End

    It "reports the correct dimensions (4x3)"
      When run command truss inspect "$SAMPLE_PNG"
      The output should include '"width": 4'
      The output should include '"height": 3'
    End

    It "reports alpha channel presence"
      When run command truss inspect "$SAMPLE_PNG"
      The output should include '"hasAlpha"'
    End
  End

  Describe "Stdin inspection"
    It "reads from stdin when given -"
      When run command sh -c "cat '$SAMPLE_PNG' | truss inspect -"
      The status should eq 0
      The output should include '"format"'
      The output should include '"width"'
    End
  End

  Describe "Error handling"
    It "exits 1 with usage and hint when no input is given"
      When run command truss inspect
      The status should eq 1
      The stderr should include "error:"
      The stderr should include "usage:"
      The stderr should include "hint:"
    End

    It "exits 2 when the file does not exist"
      When run command truss inspect /nonexistent/file.png
      The status should eq 2
      The stderr should include "error:"
    End
  End
End
