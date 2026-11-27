//! The dock's view of the shared WinHTTP client: the client lives in oled-sys, the
//! User-Agent is the dock's.

pub use oled_sys::winhttp::{split_https_url, HttpError, Response};

/// `mini-oled-dock/<version>` (GitHub's API rejects requests without a User-Agent).
pub fn user_agent() -> String {
    format!("mini-oled-dock/{}", env!("CARGO_PKG_VERSION"))
}

/// Blocking HTTPS GET with the dock's User-Agent; see `oled_sys::winhttp::get`.
pub fn get(url: &str, headers: &[(&str, &str)], cap: u64) -> Result<Response, HttpError> {
    oled_sys::winhttp::get(&user_agent(), url, headers, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_the_dock_version() {
        assert_eq!(user_agent(), format!("mini-oled-dock/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    #[ignore = "network: run with --ignored"]
    fn live_latest_release() {
        let r =
            get(crate::update_release::LATEST_API_URL, &[("Accept", "application/vnd.github+json")], 1 << 20).unwrap();
        assert!(r.status == 200 || r.status == 403, "{}", r.status);
    }
}
