//! The few HTTPS requests the player makes itself (libVLC does the rest).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Sends one request and returns the status and the body as text. An HTTP
/// error status is not an `Err`; only a failure to get an answer is.
/// Blocks: call it off the UI thread.
pub fn request(method: Method, url: &str, headers: &[(&str, &str)], body: Option<&str>) -> Result<(u16, String), String> {
    // The system's certificates and proxy, so it works wherever a browser does.
    let mut agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).try_proxy_from_env(true);
    let proxy = crate::proxy::for_url(url);
    if crate::proxy::from_environment().is_none() {
        if let Some(proxy) = &proxy {
            agent = agent.proxy(ureq::Proxy::new(format!("http://{proxy}")).map_err(|e| e.to_string())?);
        }
    }
    let agent = agent.build();
    // Which way the request went is half of why it failed.
    let route = |e: ureq::Error| {
        // Without the address: the caller knows it, and the message is long as it is.
        let reason = match &e {
            ureq::Error::Transport(t) => t.message().map(str::to_owned).unwrap_or_else(|| t.kind().to_string()),
            other => other.to_string(),
        };
        match &proxy {
            Some(proxy) => format!("через прокси {proxy}: {reason}"),
            None => format!("без прокси: {reason}"),
        }
    };
    let mut request = match method {
        Method::Get => agent.get(url),
        Method::Post => agent.post(url),
    };
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let result = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(route(e)),
    };
    let status = response.status();
    response.into_string().map(|text| (status, text)).map_err(|e| route(e.into()))
}

/// A connection to read from and write to.
pub trait Stream: Read + Write + Send {}

impl<T: Read + Write + Send> Stream for T {}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens an encrypted connection to `host`, through the proxy the
/// environment or the system names if there is one. Reads that get nothing for
/// `read_timeout` fail with `WouldBlock` or `TimedOut` and may be repeated.
/// Blocks: call it off the UI thread.
pub fn tls_connect(host: &str, port: u16, read_timeout: Duration) -> Result<Box<dyn Stream>, String> {
    let text = |e: io::Error| e.to_string();
    let tcp = match crate::proxy::for_url(&format!("https://{host}/")) {
        Some(proxy) => through_proxy(&proxy, host, port).map_err(text)?,
        None => dial(&format!("{host}:{port}")).map_err(text)?,
    };
    tcp.set_read_timeout(Some(read_timeout)).map_err(text)?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT)).map_err(text)?;
    let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
    let session = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| e.to_string())?;
    Ok(Box::new(rustls::StreamOwned::new(session, tcp)))
}

fn dial(address: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "адрес не найден");
    for addr in address.to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Asks the proxy for a tunnel to `host:port`.
fn through_proxy(proxy: &str, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut tcp = dial(proxy)?;
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    tcp.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes())?;
    // The answer byte by byte, so as not to take any of what follows it.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if tcp.read(&mut byte)? == 0 || head.len() > 8192 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_owned();
    if status.split(' ').nth(1) == Some("200") {
        Ok(tcp)
    } else {
        Err(io::Error::other(format!("прокси ответил «{status}»")))
    }
}

/// The system's certificates, read once.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            let certs = rustls_native_certs::load_native_certs().map_err(|e| format!("нет сертификатов системы ({e})"))?;
            roots.add_parsable_certificates(certs);
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}
