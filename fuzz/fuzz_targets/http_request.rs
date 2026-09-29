//! Arbitrary bytes as an HTTP connection to the server.
//!
//! The server holds signing keys but no bearer token, so every route is reachable, the signed
//! GET routes check signatures, and nothing can make it fetch a remote URL: a signature cannot
//! be forged, and the private routes are refused from their headers.
//!
//! Beyond not panicking: whatever arrives, the server answers with a well-formed HTTP/1.1
//! response or closes, and never with a 500, which is reserved for its own defects.
#![no_main]

use libfuzzer_sys::fuzz_target;
use truss_fuzz::{exchange, parse_response, quiet_config};

fuzz_target!(|data: &[u8]| {
    let config = quiet_config(None).with_signed_url_credentials("fuzz-key", "fuzz-secret");
    if let Some(response) = parse_response(&exchange(config, data)) {
        assert_ne!(
            response.status,
            500,
            "the server failed: {}",
            String::from_utf8_lossy(&response.body)
        );
    }
});
