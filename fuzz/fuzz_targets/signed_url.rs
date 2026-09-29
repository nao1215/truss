//! The signed URL signer against the server that verifies it.
//!
//! The input picks a base URL, a source, transform options, credentials, and an expiry. When
//! `sign_public_url_with_method` accepts them, the URL it returns must:
//!
//! - be the same URL when signed again (signing is deterministic);
//! - carry, once its query is decoded, exactly the source, key, and expiry that were signed
//!   (the encoding round-trips);
//! - be accepted by a server holding the same key, with the Host header a client derives from
//!   the URL: whatever the server answers, it is not 401 and not 500 (the signer's canonical
//!   string and the verifier's agree);
//! - stop verifying when its expiry is changed by one second (the expiry is signed).
#![no_main]

use std::time::{SystemTime, UNIX_EPOCH};

use libfuzzer_sys::fuzz_target;
use truss::{
    CropRegion, Fit, MediaType, Rgba8, Rotation, SignedUrlSource, TransformOptions,
    sign_public_url_with_method,
};
use truss_fuzz::{Input, STORED_IMAGE, exchange, parse_response, quiet_config};
use url::Url;

const HOSTS: [&str; 5] = [
    "images.example.com",
    "127.0.0.1",
    "[::1]",
    "EXAMPLE.com",
    "xn--bcher-kva.example",
];
const FORMATS: [Option<MediaType>; 6] = [
    None,
    Some(MediaType::Jpeg),
    Some(MediaType::Png),
    Some(MediaType::Webp),
    Some(MediaType::Bmp),
    Some(MediaType::Svg),
];
const FITS: [Option<Fit>; 5] = [
    None,
    Some(Fit::Contain),
    Some(Fit::Cover),
    Some(Fit::Fill),
    Some(Fit::Inside),
];

fn options_from(input: &mut Input<'_>) -> TransformOptions {
    let mut options = TransformOptions::default();
    let flags = input.u16();
    options.format = FORMATS[usize::from(input.byte()) % FORMATS.len()];
    options.fit = FITS[usize::from(input.byte()) % FITS.len()];
    let (width, height) = (input.byte(), input.byte());
    if flags & 0x0001 != 0 {
        options.width = Some(u32::from(width % 64) + 1);
    }
    if flags & 0x0002 != 0 {
        options.height = Some(u32::from(height % 64) + 1);
    }
    let quality = input.byte();
    if flags & 0x0004 != 0 {
        options.quality = Some(quality);
    }
    let rotate = input.byte();
    options.rotate = Rotation::from_degrees((i32::from(rotate) - 128) * 7);
    options.auto_orient = flags & 0x0008 == 0;
    options.strip_metadata = flags & 0x0010 == 0;
    options.preserve_exif = flags & 0x0020 != 0;
    options.grayscale = flags & 0x0040 != 0;
    options.without_enlargement = flags & 0x0080 != 0;
    let (r, g, b, a) = (input.byte(), input.byte(), input.byte(), input.byte());
    if flags & 0x0100 != 0 {
        options.background = Some(Rgba8 { r, g, b, a });
    }
    let sigma = input.byte();
    if flags & 0x0200 != 0 {
        options.blur = Some(f32::from(sigma) / 16.0);
    }
    if flags & 0x0400 != 0 {
        options.sharpen = Some(f32::from(sigma) / 16.0);
    }
    let (x, y) = (input.byte(), input.byte());
    if flags & 0x0800 != 0 {
        options.crop = Some(CropRegion {
            x: u32::from(x & 0x0f),
            y: u32::from(y & 0x0f),
            width: u32::from(x >> 4) + 1,
            height: u32::from(y >> 4) + 1,
        });
    }
    options
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs()
}

/// Sends the signed URL the way a client that stripped the base path would, and returns the
/// status, or `None` when the connection gave no response.
fn fetch(
    method: &str,
    route: &str,
    query: &str,
    authority: &str,
    key: &str,
    secret: &str,
) -> Option<u16> {
    let request = format!(
        "{method} {route}?{query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    );
    let config = quiet_config(None).with_signed_url_credentials(key, secret);
    parse_response(&exchange(config, request.as_bytes())).map(|response| response.status)
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let selector = input.byte();
    let method = if selector & 0x01 == 0 { "GET" } else { "HEAD" };
    let scheme = if selector & 0x02 == 0 {
        "https"
    } else {
        "http"
    };
    let host_choice = input.byte();
    let host = match HOSTS.get(usize::from(host_choice)) {
        Some(host) => (*host).to_string(),
        None => input.string(24),
    };
    let port = input.u16();
    let port = if selector & 0x04 != 0 {
        format!(":{port}")
    } else {
        String::new()
    };
    let prefix = input.string(16);
    let base_url = format!("{scheme}://{host}{port}/{prefix}");

    let path = match selector & 0x18 {
        0x00 => STORED_IMAGE.to_string(),
        0x08 => format!("/{STORED_IMAGE}"),
        _ => input.string(32),
    };
    let version = (selector & 0x20 != 0).then(|| input.string(16));
    let key_id = input.string(16);
    let secret = input.string(16);
    let preset = (selector & 0x40 != 0).then(|| input.string(8));
    let expires = if selector & 0x80 != 0 {
        u64::MAX
    } else {
        now() + 3_600 + u64::from(input.u32())
    };
    let options = options_from(&mut input);

    let source = SignedUrlSource::Path {
        path: path.clone(),
        version: version.clone(),
    };
    let sign = || {
        sign_public_url_with_method(
            method,
            &base_url,
            source.clone(),
            &options,
            &key_id,
            &secret,
            expires,
            None,
            preset.as_deref(),
        )
    };
    let Ok(signed) = sign() else {
        return;
    };
    assert_eq!(sign().as_ref(), Ok(&signed), "signing is not deterministic");

    let url = Url::parse(&signed).expect("the signed URL parses");
    let query = url.query().expect("the signed URL has a query");
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let get = |name: &str| {
        let mut values = pairs
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value);
        let first = values.next().cloned();
        assert!(
            values.next().is_none(),
            "`{name}` appears twice in {signed}"
        );
        first
    };
    assert_eq!(get("path"), Some(path), "the path does not round-trip");
    assert_eq!(get("version"), version, "the version does not round-trip");
    assert_eq!(
        get("keyId"),
        Some(key_id.clone()),
        "the key does not round-trip"
    );
    assert_eq!(
        get("expires"),
        Some(expires.to_string()),
        "the expiry does not round-trip"
    );
    assert_eq!(get("preset"), preset, "the preset does not round-trip");

    let authority = match url.port() {
        Some(port) => format!(
            "{}:{port}",
            url.host_str().expect("the signed URL has a host")
        ),
        None => url
            .host_str()
            .expect("the signed URL has a host")
            .to_string(),
    };
    let route = "/images/by-path";
    if let Some(status) = fetch(method, route, query, &authority, &key_id, &secret) {
        assert_ne!(status, 401, "the server refused the signature of {signed}");
        assert_ne!(status, 500, "the server failed on {signed}");
    }

    let tampered_expires = if expires == u64::MAX {
        expires - 1
    } else {
        expires + 1
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in &pairs {
        if name == "expires" {
            serializer.append_pair(name, &tampered_expires.to_string());
        } else {
            serializer.append_pair(name, value);
        }
    }
    let tampered = serializer.finish();
    if let Some(status) = fetch(method, route, &tampered, &authority, &key_id, &secret) {
        // The signature is checked before anything else in the query is read.
        assert_eq!(status, 401, "a changed expiry still verified: {signed}");
    }
});
