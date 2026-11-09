//! Blocking HTTPS GET through WinHTTP (`winhttp.dll`, delay-loaded).

// The updater is wired in by later tasks; until then this is unused.
#![allow(dead_code)]

use std::ffi::c_void;
use std::fmt;

use windows::core::{Error, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpQueryOption, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_OPTION_URL,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Timeouts in ms: name resolution, connect, send, receive (spec values).
const TIMEOUTS_MS: [i32; 4] = [10_000, 10_000, 30_000, 30_000];
/// Largest single `WinHttpReadData` call.
const READ_CHUNK: usize = 16 * 1024;

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// URL of the last request after redirects (asset downloads end on `*.githubusercontent.com`).
    pub final_url: String,
}

#[derive(Debug)]
pub enum HttpError {
    /// Not a plain `https://host/path` URL, or a header with CR/LF. Nothing was sent.
    BadUrl,
    /// A WinHTTP call failed: `<function>: <error>`.
    Network(String),
    /// The body grew past the caller's cap; reading stopped there.
    TooLarge,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::BadUrl => f.write_str("bad URL"),
            HttpError::Network(e) => f.write_str(e),
            HttpError::TooLarge => f.write_str("response too large"),
        }
    }
}

/// `mini-oled-dock/<version>` (GitHub's API rejects requests without a User-Agent).
pub fn user_agent() -> String {
    format!("mini-oled-dock/{}", env!("CARGO_PKG_VERSION"))
}

/// `https://host[/path][?query]` into `(host, path+query)`, the path defaulting to `/`.
/// Only lowercase `https://`, no user info, no port, no fragment, no whitespace or
/// control characters, and a host made of ASCII letters, digits, `.` and `-`.
pub fn split_https_url(url: &str) -> Option<(String, String)> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.contains('#') {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(end);
    let host_ok = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-']);
    if !host_ok {
        return None;
    }
    let path = match tail.chars().next() {
        None => String::from("/"),
        Some('/') => tail.to_string(),
        Some(_) => format!("/{tail}"),
    };
    Some((host.to_string(), path))
}

/// A WinHTTP handle closed on drop.
struct Handle(*mut c_void);

impl Handle {
    /// `name` is the function that returned `raw`, for the error text when it is null.
    fn new(raw: *mut c_void, name: &str) -> Result<Handle, HttpError> {
        if raw.is_null() {
            Err(net_err(name, Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP, is non-null and is closed exactly once here.
        // Handles drop in reverse order (request, connection, session), as WinHTTP expects.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn net_err(name: &str, e: Error) -> HttpError {
    HttpError::Network(format!("{name}: {e}"))
}

fn set_u32_option(h: &Handle, option: u32, value: u32) -> windows::core::Result<()> {
    let bytes = value.to_ne_bytes();
    // SAFETY: `h` is a live WinHTTP handle and the buffer is a DWORD, as these options require.
    unsafe { WinHttpSetOption(Some(h.0), option, Some(&bytes)) }
}

/// Blocking HTTPS GET of `url` with `headers`, following redirects (never HTTPS to HTTP).
/// Stops with `TooLarge` once the body exceeds `cap` bytes. A non-2xx status is still
/// `Ok`: the caller decides. Never call it on the UI thread.
pub fn get(url: &str, headers: &[(&str, &str)], cap: u64) -> Result<Response, HttpError> {
    let (host, path) = split_https_url(url).ok_or(HttpError::BadUrl)?;
    let bad_header = |s: &str| s.is_empty() || s.contains(['\r', '\n']);
    if headers.iter().any(|(n, v)| bad_header(n) || n.contains(':') || v.contains(['\r', '\n'])) {
        return Err(HttpError::BadUrl);
    }

    let agent = HSTRING::from(user_agent());
    // SAFETY: `agent` outlives the call; no proxy name or bypass list, synchronous session.
    let raw = unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0) };
    let session = Handle::new(raw, "WinHttpOpen")?;
    let [resolve, connect, send, receive] = TIMEOUTS_MS;
    // SAFETY: `session` is a live session handle.
    unsafe { WinHttpSetTimeouts(session.0, resolve, connect, send, receive) }
        .map_err(|e| net_err("WinHttpSetTimeouts", e))?;
    let tls12_13 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
    // Builds of Windows without TLS 1.3 in WinHTTP reject the flag: fall back to TLS 1.2 only.
    set_u32_option(&session, WINHTTP_OPTION_SECURE_PROTOCOLS, tls12_13)
        .or_else(|_| set_u32_option(&session, WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2))
        .map_err(|e| net_err("WinHttpSetOption(SECURE_PROTOCOLS)", e))?;
    set_u32_option(&session, WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP)
        .map_err(|e| net_err("WinHttpSetOption(REDIRECT_POLICY)", e))?;

    let host_w = HSTRING::from(host);
    // SAFETY: `session` is live and `host_w` outlives the call.
    let raw = unsafe { WinHttpConnect(session.0, &host_w, INTERNET_DEFAULT_HTTPS_PORT, 0) };
    let connection = Handle::new(raw, "WinHttpConnect")?;
    let path_w = HSTRING::from(path);
    // SAFETY: `connection` is live, the strings outlive the call, a null accept-types list
    // means "none", and WINHTTP_FLAG_SECURE makes it TLS.
    let raw = unsafe {
        WinHttpOpenRequest(
            connection.0,
            windows::core::w!("GET"),
            &path_w,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    };
    let request = Handle::new(raw, "WinHttpOpenRequest")?;
    for (name, value) in headers {
        let line: Vec<u16> = format!("{name}: {value}").encode_utf16().collect();
        // SAFETY: `request` is live; the wrapper passes the slice length, no terminator needed.
        unsafe { WinHttpAddRequestHeaders(request.0, &line, WINHTTP_ADDREQ_FLAG_ADD) }
            .map_err(|e| net_err("WinHttpAddRequestHeaders", e))?;
    }

    // SAFETY: `request` is live; no extra headers and no body for a GET.
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.map_err(|e| net_err("WinHttpSendRequest", e))?;
    // SAFETY: `request` is live and was sent; the reserved argument must be null.
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
        .map_err(|e| net_err("WinHttpReceiveResponse", e))?;

    let mut status = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: with WINHTTP_QUERY_FLAG_NUMBER the buffer is one DWORD of `len` bytes; no
    // header name and no index are needed for the status code.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .map_err(|e| net_err("WinHttpQueryHeaders", e))?;
    let status = u16::try_from(status).map_err(|_| HttpError::Network(format!("bad HTTP status {status}")))?;

    let final_url = final_url(&request)?;

    let mut body = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let mut avail = 0u32;
        // SAFETY: `request` is live and has a response; `avail` is a valid out pointer.
        unsafe { WinHttpQueryDataAvailable(request.0, &mut avail) }
            .map_err(|e| net_err("WinHttpQueryDataAvailable", e))?;
        if avail == 0 {
            break;
        }
        let want = (avail as usize).min(chunk.len());
        let mut read = 0u32;
        // SAFETY: `chunk` holds at least `want` bytes and `read` is a valid out pointer.
        unsafe { WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), want as u32, &mut read) }
            .map_err(|e| net_err("WinHttpReadData", e))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read as usize]);
        if body.len() as u64 > cap {
            return Err(HttpError::TooLarge);
        }
    }
    Ok(Response { status, body, final_url })
}

/// URL of the request after redirects (`WINHTTP_OPTION_URL`).
fn final_url(request: &Handle) -> Result<String, HttpError> {
    let mut bytes = 0u32;
    // SAFETY: a null buffer only asks for the size in bytes; the call "fails" with
    // ERROR_INSUFFICIENT_BUFFER and sets `bytes`.
    let _ = unsafe { WinHttpQueryOption(request.0, WINHTTP_OPTION_URL, None, &mut bytes) };
    if bytes == 0 {
        return Err(HttpError::Network(String::from("WinHttpQueryOption(URL): empty")));
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `len` bytes, as the size query asked for.
    unsafe { WinHttpQueryOption(request.0, WINHTTP_OPTION_URL, Some(buf.as_mut_ptr().cast()), &mut len) }
        .map_err(|e| net_err("WinHttpQueryOption(URL)", e))?;
    let chars = (len as usize / 2).min(buf.len());
    let s = &buf[..chars];
    let s = s.iter().position(|&c| c == 0).map_or(s, |n| &s[..n]);
    Ok(String::from_utf16_lossy(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls_only() {
        assert_eq!(
            split_https_url("https://api.github.com/repos/a/b/releases/latest"),
            Some(("api.github.com".into(), "/repos/a/b/releases/latest".into()))
        );
        assert_eq!(split_https_url("https://github.com"), Some(("github.com".into(), "/".into())));
        for bad in [
            "http://github.com/x",
            "https://",
            "https://user@github.com/x",
            "https://github.com:8443/x",
            "ftp://x/y",
            "https://git hub.com/x",
            "HTTPS://github.com/x",
        ] {
            assert_eq!(split_https_url(bad), None, "{bad}");
        }
        assert_eq!(user_agent(), format!("mini-oled-dock/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn bad_urls_fail_before_any_network_call() {
        assert!(matches!(get("http://github.com/x", &[], 1024), Err(HttpError::BadUrl)));
        assert!(matches!(get("https://github.com/x", &[("X-Evil", "a\r\nHost: b")], 1024), Err(HttpError::BadUrl)));
        assert_eq!(HttpError::TooLarge.to_string(), "response too large");
        assert_eq!(HttpError::Network("WinHttpOpen: x".into()).to_string(), "WinHttpOpen: x");
    }
}
