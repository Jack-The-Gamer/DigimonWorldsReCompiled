//! A small blocking HTTP client for index feeds, GitHub's API and mod
//! downloads.
//!
//! TLS uses rustls with the operating system's certificate verifier, and
//! proxies come from the usual environment variables (and, on Windows, the
//! system proxy settings), so the launcher works wherever a browser does.

use anyhow::{anyhow, bail, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Sent with every request; GitHub's API requires a User-Agent.
pub const USER_AGENT: &str = concat!("DigimonWorldsReCompiled/", env!("CARGO_PKG_VERSION"));

/// Largest feed / API response read into memory.
pub const MAX_TEXT: u64 = 32 << 20;
/// Largest mod download.
pub const MAX_DOWNLOAD: u64 = 1 << 30;

/// Shared progress for a download (read by the UI thread).
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    /// 0 when the server did not say.
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        let t = self.total.load(Ordering::Relaxed);
        (t > 0).then(|| (self.done.load(Ordering::Relaxed) as f32 / t as f32).min(1.0))
    }
}

#[derive(Clone)]
pub struct Http {
    agent: ureq::Agent,
    /// Base URL of GitHub's REST API (overridable for testing with
    /// `DWRC_GITHUB_API`).
    pub github_api: String,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

fn describe(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Io(e) => anyhow!("could not connect ({e})"),
        ureq::Error::Timeout(_) => anyhow!("the server took too long to answer"),
        ureq::Error::HostNotFound => anyhow!("could not find the server (are you online?)"),
        ureq::Error::BodyExceedsLimit(n) => anyhow!("the response is larger than {} MB", n >> 20),
        other => anyhow!("{other}"),
    }
}

impl Http {
    pub fn new() -> Self {
        use ureq::tls::{RootCerts, TlsConfig};
        let tls = TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(USER_AGENT)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        let github_api = std::env::var("DWRC_GITHUB_API")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.github.com".to_string())
            .trim_end_matches('/')
            .to_string();
        Self { agent: config.new_agent(), github_api }
    }

    fn get(&self, url: &str, accept: Option<&str>) -> Result<ureq::http::Response<ureq::Body>> {
        let mut req = self.agent.get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        if url.starts_with(&self.github_api) {
            req = req.header("X-GitHub-Api-Version", "2022-11-28");
        }
        let resp = req.call().map_err(describe)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let h = |n: &str| resp.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
            if (status == 403 || status == 429) && h("x-ratelimit-remaining").as_deref() == Some("0") {
                let when = h("x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok()).map(|reset| {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let mins = reset.saturating_sub(now).div_ceil(60).max(1);
                    format!(" in about {mins} minute{}", if mins == 1 { "" } else { "s" })
                });
                bail!("GitHub's hourly limit for anonymous requests was reached; try again{}", when.unwrap_or_else(|| " later".into()));
            }
            bail!(
                "{} (HTTP {status})",
                match status {
                    404 => "not found",
                    401 | 403 => "access denied",
                    500..=599 => "the server had a problem",
                    _ => "request failed",
                }
            );
        }
        Ok(resp)
    }

    /// GET a text resource (a feed, a description, an API response).
    pub fn get_text(&self, url: &str, accept: Option<&str>) -> Result<String> {
        let mut resp = self.get(url, accept)?;
        resp.body_mut().with_config().limit(MAX_TEXT).read_to_string().map_err(describe)
    }

    /// GET a small binary resource (thumbnails).
    pub fn get_bytes(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut resp = self.get(url, None)?;
        resp.body_mut().with_config().limit(limit).read_to_vec().map_err(describe)
    }

    /// GET from GitHub's REST API (`path` starts with `/`).
    pub fn github(&self, path: &str) -> Result<String> {
        self.get_text(&format!("{}{path}", self.github_api), Some("application/vnd.github+json"))
    }

    /// Streams `url` into `dest`, updating `progress`. The file is written
    /// next to `dest` first and renamed when complete.
    pub fn download(&self, url: &str, dest: &Path, progress: &Progress) -> Result<()> {
        let resp = self.get(url, Some("application/octet-stream"))?;
        let total = resp.body().content_length().unwrap_or(0);
        if total > MAX_DOWNLOAD {
            bail!("the download is too large ({} MB)", total >> 20);
        }
        progress.total.store(total, Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        let part = dest.with_extension("part");
        let result = (|| -> Result<()> {
            let mut reader = resp.into_body().into_with_config().limit(MAX_DOWNLOAD).reader();
            let mut out = std::io::BufWriter::new(std::fs::File::create(&part)?);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                if progress.cancel.load(Ordering::Relaxed) {
                    bail!("cancelled");
                }
                let n = reader.read(&mut buf).map_err(|e| anyhow!("download interrupted ({e})"))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                progress.done.fetch_add(n as u64, Ordering::Relaxed);
            }
            out.flush()?;
            drop(out);
            if total > 0 && progress.done.load(Ordering::Relaxed) != total {
                bail!("the download was incomplete");
            }
            std::fs::rename(&part, dest)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        result
    }
}
