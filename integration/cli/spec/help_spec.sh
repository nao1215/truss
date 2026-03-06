# shellcheck shell=sh
# ---------------------------------------------------------------------------
# help_spec.sh - The truss help system
#
# The help system provides discoverability for every command. Running truss
# without arguments, with --help/-h, or with the "help" subcommand prints
# usage information and exits successfully.
# ---------------------------------------------------------------------------

Describe "Help system"
  Describe "Top-level help"
    It "shows help when invoked with no arguments"
      When run command truss
      The status should eq 0
      The output should include "an image transformation tool"
      The output should include "COMMANDS"
    End

    It "shows help with --help flag"
      When run command truss --help
      The status should eq 0
      The output should include "COMMANDS"
      The output should include "convert"
      The output should include "inspect"
      The output should include "serve"
      The output should include "sign"
    End

    It "shows help with -h flag"
      When run command truss -h
      The status should eq 0
      The output should include "USAGE:"
    End

    It "shows help with 'help' subcommand"
      When run command truss help
      The status should eq 0
      The output should include "COMMANDS"
    End
  End

  Describe "Command-specific help via 'truss help <command>'"
    It "shows convert help with usage and options"
      When run command truss help convert
      The status should eq 0
      The output should include "truss convert"
      The output should include "--output"
      The output should include "--width"
      The output should include "--format"
      The output should include "EXAMPLES"
    End

    It "shows inspect help with usage"
      When run command truss help inspect
      The status should eq 0
      The output should include "truss inspect"
      The output should include "JSON"
    End

    It "shows serve help with bind and environment variables"
      When run command truss help serve
      The status should eq 0
      The output should include "truss serve"
      The output should include "--bind"
      The output should include "ENVIRONMENT VARIABLES"
      The output should include "TRUSS_BIND_ADDR"
    End

    It "shows sign help with required options"
      When run command truss help sign
      The status should eq 0
      The output should include "truss sign"
      The output should include "--base-url"
      The output should include "--key-id"
      The output should include "--secret"
      The output should include "--expires"
    End
  End

  Describe "Command-specific help via '<command> --help'"
    It "shows convert help with 'convert --help'"
      When run command truss convert --help
      The status should eq 0
      The output should include "truss convert"
      The output should include "OPTIONS"
    End

    It "shows inspect help with 'inspect --help'"
      When run command truss inspect --help
      The status should eq 0
      The output should include "truss inspect"
    End

    It "shows serve help with 'serve --help'"
      When run command truss serve --help
      The status should eq 0
      The output should include "truss serve"
    End

    It "shows sign help with 'sign --help'"
      When run command truss sign --help
      The status should eq 0
      The output should include "truss sign"
    End
  End

  Describe "Unknown commands"
    It "suggests the closest match for a typo (converrt -> convert)"
      When run command truss converrt
      The status should eq 1
      The stderr should include "convert"
    End

    It "exits 1 when the command is unknown"
      When run command truss notacommand
      The status should eq 1
      The stderr should include "error:"
    End
  End
End
