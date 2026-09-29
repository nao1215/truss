//! Helpers shared by the fuzz targets.
//!
//! Everything here goes through truss's public API. The HTTP targets drive a real server with
//! [`truss::serve_once_with_config`] over a loopback socket, which is the only public way to
//! reach the request parser, the query and signature checks, and the multipart parser.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use truss::{LogHandler, ServerConfig};

/// The one image the storage root holds, so a request naming it reaches the transform.
pub const STORED_IMAGE: &str = "sample.png";

const STORED_IMAGE_BYTES: &[u8] = include_bytes!("../../integration/fixtures/sample.png");

/// Reads bytes off the front of a fuzz input, returning zero once it runs out.
///
/// Running out is not an error: a short input simply takes the defaults, which keeps every
/// input meaningful to the target rather than rejecting the short ones.
pub struct Input<'a> {
    bytes: &'a [u8],
}

impl<'a> Input<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn byte(&mut self) -> u8 {
        match self.bytes.split_first() {
            Some((first, rest)) => {
                self.bytes = rest;
                *first
            }
            None => 0,
        }
    }

    pub fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.byte(), self.byte()])
    }

    pub fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    /// A string of at most `max` bytes, taken as a length byte and then that many bytes,
    /// with invalid UTF-8 replaced.
    pub fn string(&mut self, max: usize) -> String {
        let len = usize::from(self.byte()).min(max).min(self.bytes.len());
        let (taken, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        String::from_utf8_lossy(taken).into_owned()
    }

    pub fn rest(self) -> &'a [u8] {
        self.bytes
    }
}

/// A storage root holding [`STORED_IMAGE`], shared by every iteration of the process.
pub fn storage_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join("truss-fuzz-storage");
        std::fs::create_dir_all(&root).expect("create the fuzz storage root");
        // Written under a per-process name and renamed into place, so parallel fuzz jobs
        // never read a half-written file.
        let staging = root.join(format!(".{}.{}", STORED_IMAGE, std::process::id()));
        std::fs::write(&staging, STORED_IMAGE_BYTES).expect("write the stored image");
        std::fs::rename(&staging, root.join(STORED_IMAGE)).expect("publish the stored image");
        root.canonicalize()
            .expect("canonicalize the fuzz storage root")
    })
    .clone()
}

/// A server configuration that stays quiet and cheap per request.
///
/// The limits are what keep one input from costing seconds: a small decode budget, a short
/// transform deadline, and no cache or compression work.
pub fn quiet_config(bearer_token: Option<&str>) -> ServerConfig {
    let mut config = ServerConfig::new(storage_root(), bearer_token.map(str::to_string));
    let silent: LogHandler = Arc::new(|_: &str| {});
    config.log_handler = Some(silent);
    config.max_input_pixels = 1 << 20;
    config.max_upload_bytes = 1 << 20;
    config.transform_deadline_secs = 2;
    config.enable_compression = false;
    config.keep_alive_max_requests = 4;
    config
}

/// Sends `request` to a server that answers one connection, half-closes, and returns every
/// byte the server wrote back.
///
/// The server may close before it has read the whole request (a request refused from its
/// headers), and a reset can then discard the response, so an empty result is possible and
/// is not by itself a defect.
pub fn exchange(config: ServerConfig, request: &[u8]) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
    let address = listener.local_addr().expect("read the listener address");
    std::thread::scope(|scope| {
        let server = scope.spawn(move || truss::serve_once_with_config(listener, config));
        let mut stream = TcpStream::connect(address).expect("connect to the fuzz server");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set the client read timeout");
        let _ = stream.write_all(request);
        let _ = stream.shutdown(Shutdown::Write);
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        drop(stream);
        let _ = server.join().expect("the server thread panicked");
        response
    })
}

/// The first response in a byte stream a server wrote.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Parses the first HTTP/1.1 response in `bytes`, panicking when the server wrote something
/// that is not one. An empty stream is `None`.
pub fn parse_response(bytes: &[u8]) -> Option<Response> {
    if bytes.is_empty() {
        return None;
    }
    let head_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("response has no end of headers: {:?}", preview(bytes)));
    let head = std::str::from_utf8(&bytes[..head_end])
        .unwrap_or_else(|_| panic!("response head is not UTF-8: {:?}", preview(bytes)));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("malformed status line: {status_line:?}"));
    let headers: Vec<(String, String)> = lines
        .map(|line| {
            let (name, value) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("malformed header line: {line:?}"));
            (name.trim().to_string(), value.trim().to_string())
        })
        .collect();
    let rest = &bytes[head_end + 4..];
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| {
            value
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("malformed Content-Length: {value:?}"))
        });
    let body = match length {
        Some(length) => rest[..length.min(rest.len())].to_vec(),
        None => Vec::new(),
    };
    Some(Response {
        status,
        headers,
        body,
    })
}

fn preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).into_owned()
}
