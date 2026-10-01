/// Decodes CSS escapes the way a CSS tokenizer does, so `u\72 l(` reads as `url(`.
pub(super) fn decode_css_escapes(css: &str) -> String {
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

/// No `@import`, the at-rule that fetches a stylesheet by itself, reading the stylesheet with
/// escapes decoded and quoted strings skipped.
///
/// The sanitizer drops every at-rule outside its allowed list, but only `@import` loads
/// anything, and a stray at-keyword inside a block is ignored by a renderer, so asserting the
/// whole list here reported noise rather than exposure.
pub(super) fn check_at_rules(css: &str) {
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
            // Outside strings an escaped `@` is part of an identifier, not
            // the start of an at-rule. An escaped quote must not open a string
            // and hide a later real @import either.
            None if byte == b'\\' => {
                index += css_escape_len(bytes, index);
                continue;
            }
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
