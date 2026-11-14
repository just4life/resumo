//! The few HTTPS requests the player makes itself (libVLC does the rest).

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
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).try_proxy_from_env(true).build();
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
        Err(e) => return Err(e.to_string()),
    };
    let status = response.status();
    response.into_string().map(|text| (status, text)).map_err(|e| e.to_string())
}
