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
                let end = css_name_end(bytes, start);
                let raw_name = String::from_utf8_lossy(&bytes[start..end]);
                let name = decode_css_escapes(&raw_name).to_ascii_lowercase();
                assert_ne!(name, "import", "@import survived sanitization");
                index = end;
                continue;
            }
            None if is_css_name_byte(byte) || byte == b'\\' => {
                // Consume identifiers whole: escaped `@` and quotes have no
                // syntactic meaning here. An unquoted url() is one token too,
                // so @import in its fragment is not an at-rule.
                let end = css_name_end(bytes, index);
                let name = decode_css_escapes(&String::from_utf8_lossy(&bytes[index..end]));
                index = end;
                if name.eq_ignore_ascii_case("url") && bytes.get(index) == Some(&b'(') {
                    index += 1;
                    while bytes
                        .get(index)
                        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c))
                    {
                        index += 1;
                    }
                    if !matches!(bytes.get(index), Some(b'"' | b'\'')) {
                        // Bad URL remnants also end at the next unescaped ')'.
                        while index < bytes.len() {
                            match bytes[index] {
                                b')' => {
                                    index += 1;
                                    break;
                                }
                                b'\\' => index += css_escape_len(bytes, index),
                                _ => index += 1,
                            }
                        }
                    }
                }
                continue;
            }
            None => {}
        }
        index += 1;
    }
}

fn is_css_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || !byte.is_ascii()
}

fn css_name_end(bytes: &[u8], mut end: usize) -> usize {
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end += css_escape_len(bytes, end);
        } else if is_css_name_byte(bytes[end]) {
            end += 1;
        } else {
            break;
        }
    }
    end
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
