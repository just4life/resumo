//! The proxy to go through: the one the environment names, or else the one
//! set in the system (Windows: Settings -> Network -> Proxy, including a
//! setup script). Only HTTP proxies; a SOCKS one is not spoken here.

/// The proxy for an HTTPS request to `url` as `host:port`, or `None` to
/// connect directly. May ask the system's setup script: call it off the UI
/// thread where a wait matters.
pub fn for_url(url: &str) -> Option<String> {
    from_environment().or_else(|| system(url))
}

/// Whether the environment names a proxy (it wins over the system's).
pub fn from_environment() -> Option<String> {
    let value = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"].iter().find_map(|name| std::env::var(name).ok())?;
    address(value.trim())
}

/// `host:port` out of `http://user@host:port/`; nothing for other schemes.
fn address(value: &str) -> Option<String> {
    let rest = value.strip_prefix("http://").unwrap_or(value);
    let address = rest.split('/').next().unwrap_or_default().rsplit('@').next().unwrap_or_default();
    (!address.is_empty() && !rest.contains("://")).then(|| address.to_owned())
}

/// Picks the proxy for HTTPS out of a Windows proxy setting: one address
/// for everything (`host:port`) or a list by protocol
/// (`http=host:port;https=host:port;socks=host:port`).
#[cfg_attr(not(windows), allow(dead_code))]
fn from_setting(setting: &str) -> Option<String> {
    let entries: Vec<&str> = setting.split([';', ' ']).map(str::trim).filter(|e| !e.is_empty()).collect();
    let named = |scheme: &str| entries.iter().find_map(|e| e.split_once('=').filter(|(s, _)| s.eq_ignore_ascii_case(scheme)).map(|(_, a)| a));
    let chosen = named("https").or_else(|| named("http")).or_else(|| entries.iter().find(|e| !e.contains('=')).copied())?;
    address(chosen.trim())
}

#[cfg(not(windows))]
fn system(_url: &str) -> Option<String> {
    None
}

#[cfg(windows)]
fn system(url: &str) -> Option<String> {
    let settings = windows::settings().unwrap_or(windows::Settings { script: None, proxy: None });
    // A setup script, when there is one, decides for each address.
    if let Some(script) = &settings.script {
        if let Some(answer) = windows::ask_script(script, url) {
            return answer.as_deref().and_then(from_setting);
        }
    }
    // Some programs write the proxy only where the system used to keep it:
    // WinHTTP does not always see it there.
    settings.proxy.or_else(windows::registry_proxy).as_deref().and_then(from_setting)
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    struct IeConfig {
        auto_detect: i32,
        auto_config_url: *mut u16,
        proxy: *mut u16,
        proxy_bypass: *mut u16,
    }

    #[repr(C)]
    struct AutoProxyOptions {
        flags: u32,
        auto_detect_flags: u32,
        auto_config_url: *const u16,
        reserved: *mut c_void,
        reserved2: u32,
        auto_logon_if_challenged: i32,
    }

    #[repr(C)]
    struct ProxyInfo {
        access_type: u32,
        proxy: *mut u16,
        proxy_bypass: *mut u16,
    }

    const ACCESS_TYPE_NO_PROXY: u32 = 1;
    const AUTOPROXY_CONFIG_URL: u32 = 2;
    /// Milliseconds the setup script gets at each step.
    const SCRIPT_TIMEOUT: i32 = 4000;

    #[link(name = "winhttp")]
    extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut IeConfig) -> i32;
        fn WinHttpOpen(agent: *const u16, access_type: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> *mut c_void;
        fn WinHttpSetTimeouts(handle: *mut c_void, resolve: i32, connect: i32, send: i32, receive: i32) -> i32;
        fn WinHttpGetProxyForUrl(session: *mut c_void, url: *const u16, options: *mut AutoProxyOptions, info: *mut ProxyInfo) -> i32;
        fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(key: isize, subkey: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
    }

    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;
    const RRF_RT_REG_DWORD: u32 = 0x10;
    const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    /// The proxy as the registry holds it (`ProxyEnable`, `ProxyServer`).
    pub fn registry_proxy() -> Option<String> {
        let key = wide(INTERNET_SETTINGS);
        let mut enabled = 0u32;
        let mut size = 4u32;
        // SAFETY: the buffers are ours and as large as `size` says.
        let read = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), wide("ProxyEnable").as_ptr(), RRF_RT_REG_DWORD, ptr::null_mut(), (&mut enabled as *mut u32).cast(), &mut size)
        };
        if read != 0 || enabled == 0 {
            return None;
        }
        let mut text = [0u16; 1024];
        let mut size = (text.len() * 2) as u32;
        // SAFETY: as above; the system writes at most `size` bytes.
        let read = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), wide("ProxyServer").as_ptr(), RRF_RT_REG_SZ, ptr::null_mut(), text.as_mut_ptr().cast(), &mut size)
        };
        if read != 0 {
            return None;
        }
        let length = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..length])).filter(|s| !s.trim().is_empty())
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalFree(memory: *mut c_void) -> *mut c_void;
    }

    pub struct Settings {
        /// The address of the setup script (PAC).
        pub script: Option<String>,
        /// The proxy set by hand, as the system writes it.
        pub proxy: Option<String>,
    }

    /// What the user set for the system; `None` when it cannot be read.
    pub fn settings() -> Option<Settings> {
        let mut config = IeConfig { auto_detect: 0, auto_config_url: ptr::null_mut(), proxy: ptr::null_mut(), proxy_bypass: ptr::null_mut() };
        // SAFETY: the structure is ours; the strings it gets are taken and freed below.
        if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } == 0 {
            return None;
        }
        let settings = Settings { script: take(config.auto_config_url), proxy: take(config.proxy) };
        take(config.proxy_bypass);
        Some(settings)
    }

    /// Asks the setup script which proxy to use for `url`. `None` when the
    /// script gave no answer, `Some(None)` when it said to connect directly.
    pub fn ask_script(script: &str, url: &str) -> Option<Option<String>> {
        let agent = wide("Resumo");
        let script = wide(script);
        let url = wide(url);
        // SAFETY: all the strings are zero-terminated and outlive the calls;
        // the session is closed and the answer's strings are freed.
        unsafe {
            let session = WinHttpOpen(agent.as_ptr(), ACCESS_TYPE_NO_PROXY, ptr::null(), ptr::null(), 0);
            if session.is_null() {
                return None;
            }
            WinHttpSetTimeouts(session, SCRIPT_TIMEOUT, SCRIPT_TIMEOUT, SCRIPT_TIMEOUT, SCRIPT_TIMEOUT);
            let mut options = AutoProxyOptions {
                flags: AUTOPROXY_CONFIG_URL,
                auto_detect_flags: 0,
                auto_config_url: script.as_ptr(),
                reserved: ptr::null_mut(),
                reserved2: 0,
                auto_logon_if_challenged: 1,
            };
            let mut info = ProxyInfo { access_type: 0, proxy: ptr::null_mut(), proxy_bypass: ptr::null_mut() };
            let found = WinHttpGetProxyForUrl(session, url.as_ptr(), &mut options, &mut info) != 0;
            WinHttpCloseHandle(session);
            let proxy = take(info.proxy);
            take(info.proxy_bypass);
            found.then(|| proxy.filter(|_| info.access_type != ACCESS_TYPE_NO_PROXY))
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Copies a string the system allocated and frees it.
    fn take(text: *mut u16) -> Option<String> {
        if text.is_null() {
            return None;
        }
        // SAFETY: a zero-terminated string from WinHTTP, to be freed with GlobalFree.
        unsafe {
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            let copy = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            GlobalFree(text.cast());
            Some(copy).filter(|s| !s.trim().is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxy_is_read_from_a_system_setting() {
        assert_eq!(from_setting("127.0.0.1:10809").as_deref(), Some("127.0.0.1:10809"));
        assert_eq!(from_setting("http://proxy.example:3128/").as_deref(), Some("proxy.example:3128"));
        assert_eq!(from_setting("http=a:1;https=b:2;ftp=c:3").as_deref(), Some("b:2"));
        assert_eq!(from_setting("ftp=c:3;http=a:1").as_deref(), Some("a:1"));
        assert_eq!(from_setting("HTTPS=http://b:2").as_deref(), Some("b:2"));
        // A script may offer several: the first is taken.
        assert_eq!(from_setting("a:1;b:2").as_deref(), Some("a:1"));
        // SOCKS is not spoken.
        assert_eq!(from_setting("socks=127.0.0.1:1080"), None);
        assert_eq!(from_setting("socks5://127.0.0.1:1080"), None);
        assert_eq!(from_setting(""), None);
    }

    #[test]
    fn an_address_is_taken_out_of_a_proxy_url() {
        assert_eq!(address("http://user:pass@host:8080/").as_deref(), Some("host:8080"));
        assert_eq!(address("host:8080").as_deref(), Some("host:8080"));
        assert_eq!(address("socks5://host:1080"), None);
    }
}
