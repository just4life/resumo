//! Hands libVLC a file that lies behind a proxy.
//!
//! libVLC on Windows does not go through a proxy for plain HTTPS files (it
//! does for HLS playlists), so where a proxy is in use such a file is fetched
//! here, through `http::tls_connect`, and served to libVLC from a port on
//! this computer. Requests are passed on one to one, ranges included, so
//! seeking works as it does without the relay.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use crate::http;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Resumo";
/// How long the far side may stay silent.
const SILENCE: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

/// The port listened on and the word a request must carry: other programs
/// on this computer are not to fetch through the player.
static SERVER: OnceLock<Option<(u16, String)>> = OnceLock::new();

/// The address to give libVLC in place of `url`: the same when no proxy is
/// in use or the address is not an HTTPS one.
pub fn through(url: &str) -> String {
    if !url.starts_with("https://") || crate::proxy::for_url(url).is_none() {
        return url.to_owned();
    }
    match SERVER.get_or_init(start) {
        Some((port, word)) => format!("http://127.0.0.1:{port}/{word}?u={}", encode(url)),
        None => url.to_owned(),
    }
}

fn start() -> Option<(u16, String)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let word = secret();
    let expected = word.clone();
    std::thread::Builder::new()
        .name("relay".into())
        .spawn(move || {
            for client in listener.incoming().flatten() {
                let expected = expected.clone();
                let _ = std::thread::Builder::new().name("relay request".into()).spawn(move || {
                    let _ = serve(client, &expected);
                });
            }
        })
        .ok()?;
    Some((port, word))
}

/// A word nobody else can guess, from what differs between runs.
fn secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    let state = std::collections::hash_map::RandomState::new();
    let mut word = String::new();
    for salt in 0..2u8 {
        let mut hasher = state.build_hasher();
        hasher.write_u8(salt);
        word.push_str(&format!("{:016x}", hasher.finish()));
    }
    word
}

/// Answers one request of libVLC's with what the real address answers.
fn serve(mut client: TcpStream, word: &str) -> io::Result<()> {
    client.set_read_timeout(Some(SILENCE))?;
    let head = read_head(&mut client)?;
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (request.next().unwrap_or_default(), request.next().unwrap_or_default());
    let url = target.strip_prefix('/').and_then(|t| t.strip_prefix(word)).and_then(|t| t.strip_prefix("?u=")).map(decode);
    let (Some(mut url), true) = (url, matches!(method, "GET" | "HEAD")) else {
        return client.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let range = lines.find_map(|l| l.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("range")).map(|(_, v)| v.trim().to_owned()));

    for _ in 0..=MAX_REDIRECTS {
        let (mut far, answer) = match ask(method, &url, range.as_deref()) {
            Ok(answer) => answer,
            Err(e) => {
                let text = format!("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\nX-Reason: {}\r\n\r\n", e.replace(['\r', '\n'], " "));
                return client.write_all(text.as_bytes());
            }
        };
        // A move to another address is followed here: libVLC would follow
        // it past the proxy.
        if let Some(next) = moved(&answer) {
            url = next;
            continue;
        }
        client.write_all(answer.as_bytes())?;
        return pour(&mut *far, &mut client);
    }
    client.write_all(b"HTTP/1.1 508 Loop Detected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

/// Sends the request to the real address; gives the connection, to read the
/// body from, and the answer's head.
fn ask(method: &str, url: &str, range: Option<&str>) -> Result<(Box<dyn http::Stream>, String), String> {
    let rest = url.strip_prefix("https://").ok_or("не HTTPS-адрес")?;
    let (authority, path) = match rest.find(['/', '?']) {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_owned() };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| "непонятный порт")?),
        None => (authority, 443),
    };
    let mut far = http::tls_connect(host, port, SILENCE)?;
    let range = range.map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
    let request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\n{range}Connection: close\r\n\r\n");
    far.write_all(request.as_bytes()).and_then(|()| far.flush()).map_err(|e| e.to_string())?;
    let head = read_head(&mut *far).map_err(|e| e.to_string())?;
    Ok((far, head))
}

/// Where a redirecting answer points.
fn moved(head: &str) -> Option<String> {
    let mut lines = head.lines();
    let status = lines.next()?.split(' ').nth(1)?;
    if !matches!(status, "301" | "302" | "303" | "307" | "308") {
        return None;
    }
    let location = lines.find_map(|l| l.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("location")).map(|(_, v)| v.trim()))?;
    location.starts_with("https://").then(|| location.to_owned())
}

/// Reads up to the empty line that ends a request's or an answer's head,
/// byte by byte so as not to take any of the body.
fn read_head(from: &mut (impl Read + ?Sized)) -> io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if from.read(&mut byte)? == 0 || head.len() > 32 * 1024 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// Copies the body until either side has had enough.
fn pour(from: &mut (impl Read + ?Sized), to: &mut TcpStream) -> io::Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => to.write_all(&buffer[..n])?,
            // The far side closing without a goodbye ends the body all the same.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| text.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_survives_the_trip() {
        let url = "https://rr4---sn-x.googlevideo.com/videoplayback?expire=1&mime=video%2Fmp4&sig=A-b_c==&n=я";
        let packed = encode(url);
        assert!(packed.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~%".contains(&b)));
        assert_eq!(decode(&packed), url);
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn a_move_is_seen() {
        assert_eq!(moved("HTTP/1.1 302 Found\r\nlocation: https://b.example/x\r\n\r\n").as_deref(), Some("https://b.example/x"));
        assert_eq!(moved("HTTP/1.1 206 Partial Content\r\nLocation: https://b.example/x\r\n\r\n"), None);
        assert_eq!(moved("HTTP/1.1 302 Found\r\nLocation: /relative\r\n\r\n"), None);
    }

    #[test]
    fn the_secret_differs() {
        assert_eq!(secret().len(), 32);
        assert_ne!(secret(), secret());
    }
}
