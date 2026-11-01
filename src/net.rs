//! Shared HTTP client and URL helpers.

use std::time::Duration;

/// Client for the internet (catalogue, downloads). Trusts the system's certificates too
/// and takes a proxy from `HTTPS_PROXY`/`ALL_PROXY`. No limit for a whole download, only
/// for connecting and for a pause in the data (slow connections finish big plugins);
/// small requests set their own limit (`SHORT`).
pub fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("tuxdeck/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()
            .expect("http client")
    })
    .clone()
}

/// Time limit for small requests (lists, API answers).
pub const SHORT: Duration = Duration::from_secs(30);

/// Client for requests to the local daemon: they carry the API token, so they must never
/// go through a proxy from the environment.
pub fn local() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).build().expect("http client")).clone()
}

pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_coding_round_trip() {
        let s = "a b/ä?x=1&y=%";
        assert_eq!(urldecode(&urlencode(s)), s);
        assert_eq!(urlencode("a.b-c_d~"), "a.b-c_d~");
    }
}
