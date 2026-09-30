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

/// Decodes CSS escapes the way a CSS tokenizer does, so `u\72 l(` reads as `url(`.
fn decode_css_escapes(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
            hex.push(chars.next().unwrap_or_default());
        }
        if hex.is_empty() {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if chars.peek().is_some_and(|next| next.is_ascii_whitespace()) {
            chars.next();
        }
        let decoded = u32::from_str_radix(&hex, 16)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or('\u{FFFD}');
        out.push(decoded);
    }
    out
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

/// No `@import`, the at-rule that fetches a stylesheet by itself, reading the stylesheet with
/// escapes decoded and quoted strings skipped.
///
/// The sanitizer drops every at-rule outside its allowed list, but only `@import` loads
/// anything, and a stray at-keyword inside a block is ignored by a renderer, so asserting the
/// whole list here reported noise rather than exposure.
fn check_at_rules(css: &str) {
    let bytes = css.as_bytes();
    let mut quote = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(_) if byte == b'\\' => {
                index += css_escape_len(bytes, index);
                continue;
            }
            Some(open) if byte == open => quote = None,
            // CSS ends a malformed string at an unescaped newline. The next
            // quote starts a new string. Decode escapes only after tokenizing:
            // `\A` inside a string represents a newline but does not end it.
            Some(_) if matches!(byte, b'\n' | b'\r' | 0x0c) => quote = None,
            Some(_) => {}
            None if bytes[index..].starts_with(b"/*") => {
                index += bytes[index + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")
                    .map_or(bytes.len() - index, |end| end + 4);
                continue;
            }
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'@' => {
                let start = index + 1;
                let mut end = start;
                while end < bytes.len() {
                    if bytes[end] == b'\\' {
                        end += css_escape_len(bytes, end);
                    } else if bytes[end].is_ascii_alphanumeric()
                        || matches!(bytes[end], b'-' | b'_')
                    {
                        end += 1;
                    } else {
                        break;
                    }
                }
                let raw_name = String::from_utf8_lossy(&bytes[start..end]);
                let name = decode_css_escapes(&raw_name).to_ascii_lowercase();
                assert_ne!(name, "import", "@import survived sanitization");
                index = end;
                continue;
            }
            None => {}
        }
        index += 1;
    }
}

fn css_escape_len(bytes: &[u8], start: usize) -> usize {
    let mut end = start + 1;
    while end < bytes.len() && end - start <= 6 && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end == start + 1 && end < bytes.len() {
        end += 1;
    } else if end < bytes.len() && bytes[end].is_ascii_whitespace() {
        if bytes[end..].starts_with(b"\r\n") {
            end += 1;
        }
        end += 1;
    }
    end - start
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
