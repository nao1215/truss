//! Multipart uploads to `POST /images`, the one route that parses multipart.
//!
//! The input is a length-prefixed `Content-Type` value followed by the request body, sent
//! with a valid bearer token so the body is always parsed. A body with a JSON number of four
//! or more digits is skipped, which keeps the size the options part asks for below 1000 a
//! side and one input cheap; the server's own decode limit is lowered for the same reason.
//!
//! Beyond not panicking: the server answers with a well-formed response that is never a 500,
//! and a 200 carries an image the sniffer reads as the type its `Content-Type` names.
#![no_main]

use libfuzzer_sys::fuzz_target;
use truss::{RawArtifact, sniff_artifact};
use truss_fuzz::{Input, exchange, parse_response, quiet_config};

const TOKEN: &str = "fuzz-token";

/// Whether a JSON number of four or more digits follows a `:` anywhere in `body`.
///
/// That is the shape of `"width": 5000` in the options part, and the only way an upload asks
/// for a large output: the other numeric options are range-checked or bounded by the image.
fn has_large_json_number(body: &[u8]) -> bool {
    body.iter().enumerate().any(|(index, byte)| {
        *byte == b':' && {
            let rest = &body[index + 1..];
            let start = rest
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'-'))
                .unwrap_or(rest.len());
            rest[start..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
                >= 4
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let content_type = input.string(96);
    let body = input.rest();
    if content_type.contains(['\r', '\n']) || has_large_json_number(body) {
        return;
    }

    let mut request = format!(
        "POST /images HTTP/1.1\r\nHost: fuzz\r\nAuthorization: Bearer {TOKEN}\r\n\
         Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);

    let Some(response) = parse_response(&exchange(quiet_config(Some(TOKEN)), &request)) else {
        return;
    };
    assert_ne!(
        response.status,
        500,
        "the server failed: {}",
        String::from_utf8_lossy(&response.body)
    );
    if response.status == 200 {
        let declared = response
            .header("content-type")
            .expect("a 200 names its content type");
        let declared = declared.split(';').next().unwrap_or_default().trim();
        let sniffed =
            sniff_artifact(RawArtifact::new(response.body.clone(), None)).unwrap_or_else(|error| {
                let body = String::from_utf8_lossy(&response.body[..response.body.len().min(96)]);
                panic!("a 200 {declared} body does not sniff: {error}: {body:?}")
            });
        assert_eq!(
            sniffed.media_type.as_mime(),
            declared,
            "the body is not the type the response names"
        );
    }
});
