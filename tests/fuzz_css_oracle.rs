//! Regression tests for the independent CSS checks used by the SVG fuzz target.

#[path = "common/fuzz_css_oracle.rs"]
mod css_oracle;

use css_oracle::check_at_rules;
use rstest::rstest;

#[rstest]
#[case::escaped_at(r"\@import 'local';")]
#[case::hex_escaped_at(r"\40 import 'local';")]
#[case::escaped_quote(r#"\" \@import 'local';"#)]
#[case::string(r#"text { content: "@import"; }"#)]
#[case::fragment_url("url(#@import)")]
#[case::escaped_url_name(r"u\72 l(#@import)")]
#[case::bad_url("url(bad( @import)")]
#[case::comment("/* @import 'local'; */")]
fn accepts_text_that_is_not_an_import_rule(#[case] css: &str) {
    check_at_rules(css);
}

#[rstest]
#[case::plain("@import 'remote';")]
#[case::escaped_name(r"@\69mport 'remote';")]
#[case::escaped_backslash(r"\\@import 'remote';")]
#[case::after_escaped_at(r"\@prefix @import 'remote';")]
#[case::after_escaped_quote(r#"\" @import 'remote';"#)]
#[case::after_url("url(#local) @import 'remote';")]
#[case::after_quoted_url("url('#local') @import 'remote';")]
#[case::after_bad_url("url(bad( @ignored) @import 'remote';")]
#[case::after_bad_string("\"broken\n@import 'remote';")]
#[should_panic(expected = "@import survived sanitization")]
fn rejects_real_import_rules(#[case] css: &str) {
    check_at_rules(css);
}
