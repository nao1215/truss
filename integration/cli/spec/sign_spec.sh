# shellcheck shell=sh
# ---------------------------------------------------------------------------
# sign_spec.sh - The truss sign command
#
# The sign command generates HMAC-signed public URLs for the truss HTTP
# server. These URLs allow clients to request specific transformations
# without needing direct API access.
#
# Synopsis:
#   truss sign --base-url <URL> --path <PATH> \
#     --key-id <ID> --secret <SECRET> --expires <UNIX_SECS> [OPTIONS]
# ---------------------------------------------------------------------------

Describe "Sign command"
  Describe "Help"
    It "shows help with --help"
      When run command truss sign --help
      The status should eq 0
      The output should include "truss sign"
      The output should include "--base-url"
    End
  End

  Describe "Error handling"
    It "exits 1 with usage and hint when no arguments are given"
      When run command truss sign
      The status should eq 1
      The stderr should include "error:"
      The stderr should include "usage:"
      The stderr should include "hint:"
    End
  End

  Describe "URL generation"
    It "produces a signed HTTPS URL with all required arguments"
      When run command truss sign \
        --base-url "https://cdn.example.com" \
        --path "/photos/hero.jpg" \
        --key-id "testkey" \
        --secret "testsecret" \
        --expires 1700000000
      The status should eq 0
      The output should include "https://"
    End

    It "includes transform options in the signed URL"
      When run command truss sign \
        --base-url "https://cdn.example.com" \
        --path "/photos/hero.jpg" \
        --key-id "testkey" \
        --secret "testsecret" \
        --expires 1700000000 \
        --width 640 \
        --format webp
      The status should eq 0
      The output should include "https://"
      The output should include "640"
    End
  End
End
