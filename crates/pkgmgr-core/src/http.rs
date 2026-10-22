//! The one HTTP client: rustls (ring), timeouts that catch a stalled
//! connection, and an HTTPS-only redirect policy.

use std::time::Duration;

use reqwest::redirect;
use url::Url;

use crate::{Error, Result};

/// The client type, re-exported so the app needs no direct reqwest dependency.
pub use reqwest::Client as HttpClient;

pub const USER_AGENT: &str = concat!("pkgmgr/", env!("CARGO_PKG_VERSION"));

/// Builds the shared client. Safe to call more than once.
pub fn client() -> Result<reqwest::Client> {
    // Matches tauri-plugin-updater; ignore the error if a provider is already set.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(20))
        // Per-read timeout: a pulled network cable surfaces as an error and
        // the download resumes from where it stopped.
        .read_timeout(Duration::from_secs(60))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if is_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("refusing to follow a redirect to a non-HTTPS URL")
            }
        }))
        .build()
        .map_err(|e| Error::Network(format!("Couldn't start the HTTP client: {e}")))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

fn is_allowed(url: &Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url))
}

/// Accepts `https://` anywhere and `http://` only on this machine (for the
/// local development registry).
pub fn check_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| Error::InvalidData(format!("Bad URL {raw}: {e}")))?;
    if is_allowed(&url) {
        Ok(url)
    } else {
        Err(Error::InvalidData(format!("Only https:// URLs are allowed: {raw}")))
    }
}

pub fn network_error(e: reqwest::Error) -> Error {
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    Error::Network(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_policy() {
        assert!(check_url("https://github.com/o/r/releases/download/v1/a.zip").is_ok());
        assert!(check_url("http://127.0.0.1:8787/index.json").is_ok());
        assert!(check_url("http://localhost/index.json").is_ok());
        assert!(check_url("http://example.com/index.json").is_err());
        assert!(check_url("file:///etc/passwd").is_err());
        assert!(check_url("not a url").is_err());
    }

    #[test]
    fn client_builds() {
        client().unwrap();
        client().unwrap();
    }
}
