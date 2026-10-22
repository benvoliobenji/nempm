//! The local package cache, `<cache>/pkgs/<sha256>`, and downloads into it.
//!
//! A cached file is only trusted after it re-hashes correctly. Downloads go to
//! `<sha256>.part`, are hashed while streaming, resume with an HTTP Range
//! request after a dropped connection, and are renamed into place only when
//! size and hash both match.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::RANGE;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::io_ctx;
use crate::http::{check_url, network_error};
use crate::model::Package;
use crate::progress::{CancelToken, Reporter, Stage};
use crate::{Error, Result, fsutil};

const MAX_ATTEMPTS: u32 = 4;

pub fn pkgs_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("pkgs")
}

/// Total bytes in the package cache.
pub fn size(cache_dir: &Path) -> u64 {
    fs::read_dir(pkgs_dir(cache_dir))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Deletes every cached package. Returns the bytes freed.
pub fn clear(cache_dir: &Path) -> Result<u64> {
    let freed = size(cache_dir);
    match fs::remove_dir_all(pkgs_dir(cache_dir)) {
        Ok(()) => Ok(freed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(crate::error::io_error("clearing the download cache", e)),
    }
}

/// Everything a download needs from the app.
#[derive(Clone)]
pub struct Downloader {
    pub client: reqwest::Client,
    pub cache_dir: PathBuf,
    pub cancel: CancelToken,
    /// Delay before retry `n` (1-based). Tests shorten it.
    pub backoff: fn(u32) -> Duration,
}

fn default_backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << attempt.min(4))
}

impl Downloader {
    pub fn new(client: reqwest::Client, cache_dir: PathBuf, cancel: CancelToken) -> Self {
        Self {
            client,
            cache_dir,
            cancel,
            backoff: default_backoff,
        }
    }

    /// Returns the path of a verified copy of `pkg` in the cache, downloading
    /// it if needed. A cached file that no longer hashes correctly is deleted
    /// and downloaded again.
    pub async fn ensure(&self, pkg: &Package, reporter: &Arc<Reporter>) -> Result<PathBuf> {
        let dir = pkgs_dir(&self.cache_dir);
        fs::create_dir_all(&dir).map_err(io_ctx("creating the download cache"))?;
        let final_path = dir.join(&pkg.sha256);

        if final_path.exists() {
            reporter.stage(Stage::Verify, 0, pkg.size);
            let (sha, len) = self.hash(&final_path, Stage::Verify, pkg.size, reporter).await?;
            if sha == pkg.sha256 && len == pkg.size {
                reporter.stage(Stage::Verify, pkg.size, pkg.size);
                return Ok(final_path);
            }
            fsutil::remove_file_if_exists(&final_path).map_err(io_ctx("removing a damaged cached file"))?;
        }

        self.download(pkg, &dir, reporter).await?;
        Ok(final_path)
    }

    async fn hash(&self, path: &Path, stage: Stage, total: u64, reporter: &Arc<Reporter>) -> Result<(String, u64)> {
        let path = path.to_path_buf();
        let reporter = reporter.clone();
        let cancel = self.cancel.clone();
        tokio::task::spawn_blocking(move || {
            fsutil::hash_file(&path, false, |n| reporter.bytes(stage, n, total), &cancel)
        })
        .await
        .map_err(|e| Error::InvalidData(format!("Hashing task failed: {e}")))?
    }

    async fn download(&self, pkg: &Package, dir: &Path, reporter: &Arc<Reporter>) -> Result<()> {
        check_url(&pkg.url)?;
        let part = dir.join(format!("{}.part", pkg.sha256));
        let mut attempt = 0;
        let hasher = loop {
            attempt += 1;
            match self.attempt(pkg, &part, reporter).await {
                Ok(hasher) => break hasher,
                Err(e) if e.is_retryable() && attempt < MAX_ATTEMPTS => {
                    reporter.stage(Stage::Download, fsutil_len(&part), pkg.size);
                    let wait = (self.backoff)(attempt);
                    let deadline = tokio::time::Instant::now() + wait;
                    while tokio::time::Instant::now() < deadline {
                        self.cancel.check()?;
                        tokio::time::sleep(Duration::from_millis(100).min(wait)).await;
                    }
                }
                Err(e) => return Err(e),
            }
        };

        let actual = hex::encode(hasher.finalize());
        let len = fsutil_len(&part);
        if len != pkg.size {
            let _ = fs::remove_file(&part);
            return Err(Error::SizeMismatch {
                id: pkg.id.clone(),
                expected: pkg.size,
                actual: len,
            });
        }
        if actual != pkg.sha256 {
            let _ = fs::remove_file(&part);
            return Err(Error::HashMismatch {
                id: pkg.id.clone(),
                expected: pkg.sha256.clone(),
                actual,
            });
        }
        let final_path = dir.join(&pkg.sha256);
        fs::rename(&part, &final_path).map_err(io_ctx("saving the download"))?;
        reporter.stage(Stage::Verify, pkg.size, pkg.size);
        Ok(())
    }

    /// One HTTP request, resuming from whatever `part` already holds.
    async fn attempt(&self, pkg: &Package, part: &Path, reporter: &Arc<Reporter>) -> Result<Sha256> {
        self.cancel.check()?;
        let mut existing = fsutil_len(part);
        if existing > pkg.size {
            let _ = fs::remove_file(part);
            existing = 0;
        }

        let mut hasher = Sha256::new();
        if existing > 0 {
            // Re-hash what we already have so the final hash covers it.
            let p = part.to_path_buf();
            let (h, n) = tokio::task::spawn_blocking(move || -> io::Result<(Sha256, u64)> {
                let mut h = Sha256::new();
                let mut f = fs::File::open(&p)?;
                let mut buf = vec![0u8; fsutil::CHUNK];
                let mut n = 0u64;
                loop {
                    let r = f.read(&mut buf)?;
                    if r == 0 {
                        break;
                    }
                    h.update(&buf[..r]);
                    n += r as u64;
                }
                Ok((h, n))
            })
            .await
            .map_err(|e| Error::InvalidData(format!("Hashing task failed: {e}")))?
            .map_err(io_ctx("reading a partial download"))?;
            hasher = h;
            existing = n;
            if existing == pkg.size {
                return Ok(hasher);
            }
        }

        let mut request = self.client.get(&pkg.url);
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let resp = request.send().await.map_err(network_error)?;
        let status = resp.status();

        let (file, mut done) = match status {
            StatusCode::PARTIAL_CONTENT if existing > 0 && range_starts_at(&resp, existing) => {
                let f = tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(part)
                    .await
                    .map_err(io_ctx("resuming the download"))?;
                (f, existing)
            }
            StatusCode::PARTIAL_CONTENT => {
                let _ = fs::remove_file(part);
                return Err(Error::Network(
                    "The server resumed at the wrong offset; starting over".into(),
                ));
            }
            s if s.is_success() => {
                // Full body: start over.
                hasher = Sha256::new();
                let f = tokio::fs::File::create(part)
                    .await
                    .map_err(io_ctx("saving the download"))?;
                (f, 0)
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                let _ = fs::remove_file(part);
                return Err(Error::Network("The server refused to resume; starting over".into()));
            }
            s => {
                return Err(Error::HttpStatus {
                    status: s.as_u16(),
                    url: pkg.url.clone(),
                });
            }
        };

        let mut out = tokio::io::BufWriter::with_capacity(fsutil::CHUNK, file);
        reporter.stage(Stage::Download, done, pkg.size);
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if self.cancel.is_cancelled() {
                // Keep the .part file: the next attempt resumes from it.
                let _ = out.flush().await;
                return Err(Error::Cancelled);
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = out.flush().await;
                    return Err(network_error(e));
                }
            };
            done += chunk.len() as u64;
            if done > pkg.size {
                drop(out);
                let _ = fs::remove_file(part);
                return Err(Error::SizeMismatch {
                    id: pkg.id.clone(),
                    expected: pkg.size,
                    actual: done,
                });
            }
            out.write_all(&chunk).await.map_err(io_ctx("saving the download"))?;
            hasher.update(&chunk);
            reporter.bytes(Stage::Download, done, pkg.size);
        }
        out.flush().await.map_err(io_ctx("saving the download"))?;
        out.get_ref().sync_all().await.map_err(io_ctx("saving the download"))?;

        if done < pkg.size {
            return Err(Error::Network(format!(
                "The connection closed after {done} of {} bytes",
                pkg.size
            )));
        }
        Ok(hasher)
    }
}

fn range_starts_at(resp: &reqwest::Response, offset: u64) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes "))
        .and_then(|v| v.split('-').next())
        .and_then(|start| start.trim().parse::<u64>().ok())
        == Some(offset)
}

fn fsutil_len(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
