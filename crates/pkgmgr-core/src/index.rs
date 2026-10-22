//! Fetching `index.json`, with ETag caching and an offline fallback.

use std::fs;
use std::path::{Path, PathBuf};

use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::io_ctx;
use crate::http::{check_url, network_error};
use crate::model::Index;
use crate::profile::{validate_package, validate_profile};
use crate::{Error, Result, fsutil, manifest};

/// The newest `schemaVersion` this app understands.
pub const SUPPORTED_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum IndexSource {
    /// Downloaded just now.
    Network,
    /// The server said the cached copy is current (HTTP 304).
    NotModified,
    /// The server couldn't be reached; this is the last copy we saw.
    Cache,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IndexResult {
    pub index: Index,
    pub source: IndexSource,
    /// When the index was last fetched successfully.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fetched_at: Option<String>,
    pub offline: bool,
    /// Why the network fetch failed, when `offline` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Entries dropped because they failed validation.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheMeta {
    url: String,
    etag: Option<String>,
    fetched_at: String,
}

fn cache_paths(cache_dir: &Path) -> (PathBuf, PathBuf) {
    (cache_dir.join("index.json"), cache_dir.join("index.meta.json"))
}

/// Parses and checks an index. Invalid packages or profiles are dropped with
/// a warning rather than failing the whole index.
pub fn parse(bytes: &[u8]) -> Result<(Index, Vec<String>)> {
    let mut index: Index = serde_json::from_slice(bytes)
        .map_err(|e| Error::InvalidData(format!("The package index is malformed: {e}")))?;
    if index.schema_version > SUPPORTED_SCHEMA {
        return Err(Error::InvalidData(format!(
            "The package index uses format {} but this app understands up to {}. Update the app.",
            index.schema_version, SUPPORTED_SCHEMA
        )));
    }
    let mut warnings = Vec::new();
    index.profiles.retain(|p| match validate_profile(p) {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!("Skipped profile {}: {e}", p.id));
            false
        }
    });
    index.packages.retain(|p| match validate_package(p) {
        Ok(()) => true,
        Err(e) => {
            warnings.push(format!("Skipped package {}: {e}", p.id));
            false
        }
    });
    let mut seen = std::collections::HashSet::new();
    index.packages.retain(|p| {
        let first = seen.insert(p.id.clone());
        if !first {
            warnings.push(format!("Skipped duplicate package {}", p.id));
        }
        first
    });
    Ok((index, warnings))
}

fn load_cache(cache_dir: &Path) -> Option<(Index, Vec<String>, CacheMeta)> {
    let (data, meta) = cache_paths(cache_dir);
    let meta: CacheMeta = serde_json::from_slice(&fs::read(meta).ok()?).ok()?;
    let (index, warnings) = parse(&fs::read(data).ok()?).ok()?;
    Some((index, warnings, meta))
}

fn save_cache(cache_dir: &Path, bytes: &[u8], meta: &CacheMeta) -> Result<()> {
    fs::create_dir_all(cache_dir).map_err(io_ctx("creating the cache folder"))?;
    let (data, meta_path) = cache_paths(cache_dir);
    fsutil::write_atomic(&data, bytes).map_err(io_ctx("caching the index"))?;
    fsutil::write_atomic(&meta_path, &serde_json::to_vec(meta)?).map_err(io_ctx("caching the index"))
}

/// The cached index for `url`, without touching the network. A copy saved
/// from a different index URL is never returned.
pub fn cached(cache_dir: &Path, url: &str) -> Option<IndexResult> {
    let (index, warnings, meta) = load_cache(cache_dir).filter(|(_, _, meta)| meta.url == url)?;
    Some(IndexResult {
        index,
        source: IndexSource::Cache,
        fetched_at: Some(meta.fetched_at),
        offline: false,
        error: None,
        warnings,
    })
}

/// GETs the index, sending the cached ETag. Falls back to the cache (marked
/// `offline`) when the network or server fails.
pub async fn fetch(client: &reqwest::Client, url: &str, cache_dir: &Path) -> Result<IndexResult> {
    check_url(url)?;
    let cache = load_cache(cache_dir).filter(|(_, _, meta)| meta.url == url);

    let mut request = client.get(url);
    if let Some(etag) = cache.as_ref().and_then(|(_, _, m)| m.etag.clone()) {
        request = request.header(IF_NONE_MATCH, etag);
    }

    let failure = match request.send().await {
        Ok(resp) if resp.status() == StatusCode::NOT_MODIFIED && cache.is_some() => {
            let (index, warnings, mut meta) = cache.unwrap();
            meta.fetched_at = manifest::now();
            let (_, meta_path) = cache_paths(cache_dir);
            let _ = fsutil::write_atomic(&meta_path, &serde_json::to_vec(&meta)?);
            return Ok(IndexResult {
                index,
                source: IndexSource::NotModified,
                fetched_at: Some(meta.fetched_at),
                offline: false,
                error: None,
                warnings,
            });
        }
        Ok(resp) if resp.status().is_success() => {
            let etag = resp
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            match resp.bytes().await {
                Ok(bytes) => {
                    // A malformed index is an error even if a cache exists:
                    // the registry is broken and the user should know.
                    let (index, warnings) = parse(&bytes)?;
                    let meta = CacheMeta {
                        url: url.to_string(),
                        etag,
                        fetched_at: manifest::now(),
                    };
                    save_cache(cache_dir, &bytes, &meta)?;
                    return Ok(IndexResult {
                        index,
                        source: IndexSource::Network,
                        fetched_at: Some(meta.fetched_at),
                        offline: false,
                        error: None,
                        warnings,
                    });
                }
                Err(e) => network_error(e),
            }
        }
        Ok(resp) => Error::HttpStatus {
            status: resp.status().as_u16(),
            url: url.to_string(),
        },
        Err(e) => network_error(e),
    };

    match cache {
        Some((index, warnings, meta)) => Ok(IndexResult {
            index,
            source: IndexSource::Cache,
            fetched_at: Some(meta.fetched_at),
            offline: true,
            error: Some(failure.to_string()),
            warnings,
        }),
        None => Err(failure),
    }
}
