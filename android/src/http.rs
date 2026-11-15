//! The few HTTPS requests the player makes itself (libVLC does the rest).

use std::io::{Read, Write};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Sends one request and returns the status and the body as text. An HTTP
/// error status is not an `Err`; only a failure to get an answer is.
/// Blocks: call it off the UI thread.
///
/// Android's own HTTP client does the work, with the system's certificates
/// and proxy, so no TLS library has to be built into the app.
#[cfg(target_os = "android")]
pub fn request(method: Method, url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Result<(u16, String), String> {
    crate::platform::http_request(method == Method::Post, url, headers, body)
}

/// The desktop build of this crate is for its tests only and has no network.
#[cfg(not(target_os = "android"))]
pub fn request(_method: Method, _url: &str, _headers: &[(&str, &str)], _body: Option<&str>) -> Result<(u16, String), String> {
    Err("сеть есть только в сборке для Android".to_owned())
}

/// A connection to read from and write to.
pub trait Stream: Read + Write + Send {}

impl<T: Read + Write + Send> Stream for T {}

/// Opens an encrypted connection to `host`, by Android's own TLS. Reads that
/// get nothing for `read_timeout` fail with `TimedOut` and may be repeated.
/// Blocks: call it off the UI thread.
#[cfg(target_os = "android")]
pub fn tls_connect(host: &str, port: u16, read_timeout: Duration) -> Result<Box<dyn Stream>, String> {
    crate::platform::tls_connect(host, port, read_timeout).map(|socket| Box::new(socket) as Box<dyn Stream>)
}

/// The desktop build of this crate is for its tests only and has no network.
#[cfg(not(target_os = "android"))]
pub fn tls_connect(_host: &str, _port: u16, _read_timeout: Duration) -> Result<Box<dyn Stream>, String> {
    Err("сеть есть только в сборке для Android".to_owned())
}
