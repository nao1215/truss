//! SVG sanitization, reached the way the adapters reach it: bytes the sniffer calls SVG,
//! passed to `transform` with SVG output.
//!
//! Beyond not panicking:
//!
//! - the output is well-formed XML and is idempotent: sanitizing it again returns it unchanged;
//! - the output still sniffs as SVG;
//! - the output carries none of what the sanitizer promises to strip, judged by an independent
//!   reading of the output the way a renderer reads it: XML character and entity references
//!   resolved, CSS escapes decoded, and a `<style>` element's text taken as a whole. That is
//!   no forbidden element, no event handler, no `xml:base`, no `href` other than a fragment or
//!   a raster `data:image/`, no `url()` to anything else, no `@import`,
//!   no processing instruction, and no external or nested entity in the doctype.
#![no_main]

#[path = "../../tests/common/fuzz_css_oracle.rs"]
mod css_oracle;

use css_oracle::{check_at_rules, decode_css_escapes};
use libfuzzer_sys::fuzz_target;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use truss::{
    Artifact, ArtifactMetadata, MediaType, RawArtifact, TransformOptions, TransformRequest,
    sniff_artifact, transform,
};

const FORBIDDEN_ELEMENTS: [&str; 11] = [
    "script",
    "foreignobject",
    "iframe",
    "embed",
    "object",
    "animate",
    "set",
    "animatetransform",
    "animatemotion",
    "animatecolor",
    "handler",
];

fn sanitize(bytes: &[u8]) -> Option<Vec<u8>> {
    let input = Artifact::new(bytes.to_vec(), MediaType::Svg, ArtifactMetadata::default());
    let mut options = TransformOptions::default();
    options.format = Some(MediaType::Svg);
    transform(TransformRequest::new(input, options))
        .ok()
        .map(|result| result.artifact.bytes)
}

fn local_name(name: &str) -> String {
    name.rsplit_once(':')
        .map_or(name, |(_, local)| local)
        .to_ascii_lowercase()
}

fn is_safe_reference(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    value.is_empty()
        || value.starts_with('#')
        || (lower.starts_with("data:image/") && !lower.starts_with("data:image/svg"))
}

/// Every `url()` in `css` names a fragment or a raster data URL.
fn check_css_urls(css: &str, context: &str) {
    let decoded = decode_css_escapes(css).to_ascii_lowercase();
    let mut rest = decoded.as_str();
    while let Some(start) = rest.find("url(") {
        let after = &rest[start + 4..];
        let end = after.find(')').unwrap_or(after.len());
        let value = after[..end]
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .trim();
        assert!(
            is_safe_reference(value),
            "external url({value}) survived sanitization in {context}"
        );
        rest = &after[end..];
    }
}

fn resolve_reference(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let value = match number.strip_prefix('x') {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(value);
    }
    match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        "apos" => Some('\''),
        "quot" => Some('"'),
        _ => None,
    }
}

/// Checks the sanitized document the way a renderer would read it.
fn check_output(document: &str) {
    let mut reader = Reader::from_str(document);
    // The innermost open element and, for a `<style>`, the stylesheet text collected so far.
    let mut stack: Vec<(String, String)> = Vec::new();

    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(event) => event,
            Err(error) => panic!("the sanitized document is not well-formed: {error}"),
        };
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let name = local_name(element.name().as_ref());
                assert!(
                    !FORBIDDEN_ELEMENTS.contains(&name.as_str()),
                    "forbidden element <{name}> survived sanitization"
                );
                for attribute in element.attributes().flatten() {
                    let key = attribute.key.as_ref().to_ascii_lowercase();
                    let key_local = local_name(&key);
                    let value = attribute
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_or_else(|_| attribute.value.to_string(), |value| value.to_string());
                    let is_handler = key_local.len() > 2
                        && key_local.starts_with("on")
                        && key_local.as_bytes()[2].is_ascii_alphabetic();
                    assert!(!is_handler, "event handler {key} survived sanitization");
                    assert_ne!(key, "xml:base", "xml:base survived sanitization");
                    if key_local == "href" {
                        assert!(
                            is_safe_reference(&value),
                            "external {key}={value:?} survived sanitization"
                        );
                    }
                    check_css_urls(&value, &format!("attribute {key}"));
                }
                if matches!(event, Event::Start(_)) {
                    stack.push((name, String::new()));
                }
            }
            Event::End(_) => {
                if let Some((name, css)) = stack.pop()
                    && name == "style"
                {
                    check_css_urls(&css, "<style>");
                    check_at_rules(&css);
                }
            }
            Event::Text(ref text) => {
                if let Some((_, css)) = stack.last_mut() {
                    css.push_str(&text.xml10_content());
                }
            }
            Event::CData(ref data) => {
                if let Some((_, css)) = stack.last_mut() {
                    css.push_str(data);
                }
            }
            Event::GeneralRef(ref reference) => {
                if let Some((_, css)) = stack.last_mut() {
                    css.push(resolve_reference(reference).unwrap_or('\u{FFFD}'));
                }
            }
            Event::PI(_) => panic!("a processing instruction survived sanitization"),
            Event::DocType(ref doctype) => {
                let doctype = doctype.xml10_content();
                if let Some((_, subset)) = doctype.split_once('[') {
                    assert!(
                        !subset.contains("SYSTEM")
                            && !subset.contains("PUBLIC")
                            && !subset.contains('&'),
                        "an external or nested entity survived sanitization"
                    );
                }
            }
            _ => {}
        }
    }
}

fuzz_target!(|data: &[u8]| {
    // The adapters sanitize what the sniffer has already called SVG, so this does too.
    if !sniff_artifact(RawArtifact::new(data.to_vec(), None))
        .is_ok_and(|artifact| artifact.media_type == MediaType::Svg)
    {
        return;
    }
    let Some(once) = sanitize(data) else {
        return;
    };
    let document = std::str::from_utf8(&once).expect("the sanitized SVG is UTF-8");
    check_output(document);

    let twice = sanitize(&once).expect("the sanitized SVG is refused when sanitized again");
    assert_eq!(
        String::from_utf8_lossy(&twice),
        document,
        "sanitizing is not idempotent"
    );

    let reread =
        sniff_artifact(RawArtifact::new(once, None)).expect("the sanitized SVG does not sniff");
    assert_eq!(
        reread.media_type,
        MediaType::Svg,
        "the sanitized SVG sniffs as another format"
    );
});
