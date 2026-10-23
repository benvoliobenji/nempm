//! End-to-end tests: a local HTTP server stands in for GitHub Releases and a
//! temporary folder poses as the USB drive.
//!
//! The Nemesis layout under test: a stick holds one package at a time. A Core
//! stick is just the Core `.tar` at the root; any other package is its
//! `NEMESIS.data` (from the same release) at the root plus
//! `packages/<id>.zip`, or a zip unpacked into the root that brings its own
//! `NEMESIS.data`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nempm_core::cache::Downloader;
use nempm_core::check::{self, FixAction, IssueCode};
use nempm_core::drives::DriveInfo;
use nempm_core::install::{self, InstallOptions, InstallRequest};
use nempm_core::model::{DataAsset, Index, Package, PackageFormat, Profile};
use nempm_core::progress::{CancelToken, Progress, ProgressFn, Stage};
use nempm_core::{Error, http, index, manifest, uninstall};
use sha2::{Digest, Sha256};

// ---------- fixtures ----------

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn make_tar(payload_len: usize) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let data: Vec<u8> = (0..payload_len).map(|i| (i % 253) as u8).collect();
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "nemesis/core.bin", &data[..]).unwrap();
    builder.into_inner().unwrap()
}

fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        if name.ends_with('/') {
            w.add_directory(*name, opts).unwrap();
        } else {
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

fn profile() -> Profile {
    serde_json::from_str(include_str!("fixtures/nemesis.json")).unwrap()
}

fn core_package(body: &[u8], base: &str) -> Package {
    Package {
        id: "nemesis-core".into(),
        name: "Nemesis Core".into(),
        description: String::new(),
        version: "1.0.0".into(),
        format: PackageFormat::Tar,
        core: true,
        url: format!("{base}/core/ota_package.tar"),
        sha256: sha(body),
        size: body.len() as u64,
        data: None,
        extract: false,
        slot: "core".into(),
        profiles: vec!["nemesis".into()],
        requires: vec![],
        homepage: None,
    }
}

fn tool_package(id: &str, body: &[u8], data: &[u8], base: &str) -> Package {
    Package {
        id: id.into(),
        name: id.into(),
        description: String::new(),
        version: "1.0.0".into(),
        format: PackageFormat::Zip,
        core: false,
        url: format!("{base}/{id}/{id}.zip"),
        sha256: sha(body),
        size: body.len() as u64,
        data: Some(DataAsset {
            url: format!("{base}/{id}/NEMESIS.data"),
            sha256: sha(data),
            size: data.len() as u64,
        }),
        extract: false,
        slot: "packages".into(),
        profiles: vec!["nemesis".into()],
        requires: vec!["nemesis-core".into()],
        homepage: None,
    }
}

// ---------- test server ----------

/// (path, Range header) for every request the server saw.
type Hits = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Serves fixed files; supports `Range: bytes=N-`; "flaky" paths drop the
/// connection partway through their first response.
struct Server {
    base: String,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    flaky: Arc<Mutex<HashMap<String, usize>>>,
    hits: Hits,
}

struct FailAfter {
    data: Cursor<Vec<u8>>,
    left: usize,
}

impl Read for FailAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "simulated drop",
            ));
        }
        let n = buf.len().min(self.left);
        let read = self.data.read(&mut buf[..n])?;
        self.left -= read;
        Ok(read)
    }
}

impl Server {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let flaky: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let hits: Hits = Arc::default();
        let (f, fl, h) = (files.clone(), flaky.clone(), hits.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let path = req.url().to_string();
                let range = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Range"))
                    .map(|h| h.value.as_str().to_string());
                h.lock().unwrap().push((path.clone(), range.clone()));
                let Some(body) = f.lock().unwrap().get(&path).cloned() else {
                    let _ = req.respond(tiny_http::Response::empty(404));
                    continue;
                };
                let drop_after = fl.lock().unwrap().remove(&path);
                if let Some(after) = drop_after {
                    let len = body.len();
                    let reader = FailAfter {
                        data: Cursor::new(body),
                        left: after,
                    };
                    let resp = tiny_http::Response::new(tiny_http::StatusCode(200), vec![], reader, Some(len), None);
                    let _ = req.respond(resp);
                    continue;
                }
                let start = range
                    .as_deref()
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.strip_suffix('-'))
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                if start > 0 && start < body.len() {
                    let header = tiny_http::Header::from_bytes(
                        "Content-Range",
                        format!("bytes {start}-{}/{}", body.len() - 1, body.len()),
                    )
                    .unwrap();
                    let part = body[start..].to_vec();
                    let resp = tiny_http::Response::from_data(part)
                        .with_status_code(206)
                        .with_header(header);
                    let _ = req.respond(resp);
                } else {
                    let _ = req.respond(tiny_http::Response::from_data(body));
                }
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            files,
            flaky,
            hits,
        }
    }

    fn put(&self, path: &str, body: Vec<u8>) {
        self.files.lock().unwrap().insert(path.to_string(), body);
    }

    fn hits_for(&self, path: &str) -> Vec<Option<String>> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

// ---------- harness ----------

struct World {
    server: Server,
    drive_dir: tempfile::TempDir,
    cache_dir: tempfile::TempDir,
    index: Index,
    events: Arc<Mutex<Vec<Progress>>>,
    core_body: Vec<u8>,
    tool_body: Vec<u8>,
    tool_data: Vec<u8>,
    other_body: Vec<u8>,
    other_data: Vec<u8>,
}

impl World {
    fn new() -> Self {
        Self::with_core_size(64 * 1024)
    }

    fn with_core_size(core_size: usize) -> Self {
        let server = Server::start();
        let core_body = make_tar(core_size);
        // Like a real Nemesis package, the zip already holds the stick
        // layout: packages/<x>.zip plus a folder of its own.
        let tool_body = make_zip(&[
            ("packages/", b""),
            ("packages/paytondev.launchpad.zip", b"inner package"),
            ("launchpad/", b""),
            ("launchpad/install.sh", b"#!/bin/sh\necho hi\n"),
            ("launchpad/lib/common.sh", b"x=1"),
        ]);
        let tool_data = b"example-tool's NEMESIS.data".to_vec();
        let other_body = make_zip(&[("other/readme.txt", b"another tool")]);
        let other_data = b"my.tool's NEMESIS.data, which is different".to_vec();

        let core = core_package(&core_body, &server.base);
        let tool = tool_package("example-tool", &tool_body, &tool_data, &server.base);
        let other = tool_package("my.tool", &other_body, &other_data, &server.base);
        server.put("/core/ota_package.tar", core_body.clone());
        server.put("/example-tool/example-tool.zip", tool_body.clone());
        server.put("/example-tool/NEMESIS.data", tool_data.clone());
        server.put("/my.tool/my.tool.zip", other_body.clone());
        server.put("/my.tool/NEMESIS.data", other_data.clone());
        let index = Index {
            schema_version: 1,
            generated_at: "2026-10-22T00:00:00Z".into(),
            packages: vec![core, tool, other],
            profiles: vec![profile()],
        };
        Self {
            server,
            drive_dir: tempfile::tempdir().unwrap(),
            cache_dir: tempfile::tempdir().unwrap(),
            index,
            events: Arc::default(),
            core_body,
            tool_body,
            tool_data,
            other_body,
            other_data,
        }
    }

    fn root(&self) -> &Path {
        self.drive_dir.path()
    }

    fn drive(&self, fs: &str) -> DriveInfo {
        DriveInfo {
            mount_point: self.root().to_string_lossy().into_owned(),
            label: Some("STICK".into()),
            filesystem: fs.into(),
            filesystem_raw: fs.to_uppercase(),
            total_bytes: 1 << 34,
            free_bytes: 1 << 33,
            removable: true,
            read_only: false,
            device: None,
        }
    }

    fn downloader(&self, cancel: CancelToken) -> Downloader {
        let mut d = Downloader::new(http::client().unwrap(), self.cache_dir.path().to_path_buf(), cancel);
        d.backoff = |_| Duration::from_millis(10);
        d
    }

    fn sink(&self) -> ProgressFn {
        let events = self.events.clone();
        Arc::new(move |p| events.lock().unwrap().push(p))
    }

    fn pkg_mut(&mut self, id: &str) -> &mut Package {
        self.index.packages.iter_mut().find(|p| p.id == id).unwrap()
    }

    async fn install(
        &self,
        ids: &[&str],
        fs: &str,
        options: InstallOptions,
    ) -> nempm_core::Result<install::InstallOutcome> {
        self.install_with(ids, fs, options, CancelToken::new(), self.sink())
            .await
    }

    async fn install_with(
        &self,
        ids: &[&str],
        fs: &str,
        options: InstallOptions,
        cancel: CancelToken,
        sink: ProgressFn,
    ) -> nempm_core::Result<install::InstallOutcome> {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let drive = self.drive(fs);
        let profile = profile();
        install::install(
            &self.downloader(cancel),
            sink,
            InstallRequest {
                root: self.root(),
                drive: &drive,
                profile: &profile,
                index: &self.index,
                ids: &ids,
                options: &options,
            },
        )
        .await
    }

    fn cached(&self, sha256: &str) -> PathBuf {
        self.cache_dir.path().join("pkgs").join(sha256)
    }

    fn init(&self) {
        check::init_drive(self.root(), &profile()).unwrap();
    }

    /// Every file and every folder on the drive outside `.nempm`, as
    /// `/`-separated paths.
    fn entries(&self) -> (Vec<String>, Vec<String>) {
        fn walk(root: &Path, dir: &Path, files: &mut Vec<String>, dirs: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if entry.file_name() == ".nempm" {
                    continue;
                }
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if path.is_dir() {
                    dirs.push(rel);
                    walk(root, &path, files, dirs);
                } else {
                    files.push(rel);
                }
            }
        }
        let (mut files, mut dirs) = (Vec::new(), Vec::new());
        walk(self.root(), self.root(), &mut files, &mut dirs);
        files.sort();
        dirs.sort();
        (files, dirs)
    }

    fn files(&self) -> Vec<String> {
        self.entries().0
    }

    fn dirs(&self) -> Vec<String> {
        self.entries().1
    }
}

fn swap() -> InstallOptions {
    InstallOptions {
        replace_installed: true,
        ..Default::default()
    }
}

fn no_partials_anywhere(root: &Path) {
    assert!(
        check::find_partials(root, &profile()).is_empty(),
        "leftover partial files"
    );
}

// ---------- tests ----------

/// A Core stick holds only the Core .tar, at the root, under its release name.
#[tokio::test(flavor = "multi_thread")]
async fn core_stick_holds_only_the_tar() {
    for fs in ["fat32", "exfat"] {
        let w = World::new();
        w.init();
        w.install(&["nemesis-core"], fs, InstallOptions::default())
            .await
            .unwrap();
        assert_eq!(w.files(), ["ota_package.tar"]);
        assert_eq!(fs::read(w.root().join("ota_package.tar")).unwrap(), w.core_body);
        let rec = manifest::load(w.root()).unwrap().installed;
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].paths, ["ota_package.tar"]);
        assert_eq!(rec[0].data, None);
    }
}

/// A package stick holds the package's own NEMESIS.data and its zip.
#[tokio::test(flavor = "multi_thread")]
async fn tool_stick_holds_its_data_file_and_zip() {
    for fs in ["fat32", "exfat"] {
        let w = World::new();
        w.init();
        let out = w
            .install(&["example-tool"], fs, InstallOptions::default())
            .await
            .unwrap();
        assert_eq!(out.installed.len(), 1, "requires is informational: Core isn't added");
        assert_eq!(w.files(), ["NEMESIS.data", "packages/example-tool.zip"]);
        assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.tool_data);
        assert_eq!(
            fs::read(w.root().join("packages/example-tool.zip")).unwrap(),
            w.tool_body
        );

        let rec = manifest::load(w.root()).unwrap().get("example-tool").unwrap().clone();
        assert_eq!(rec.paths, ["NEMESIS.data", "packages/example-tool.zip"]);
        assert_eq!(rec.data.as_deref(), Some("NEMESIS.data"));
        assert_eq!(rec.size, (w.tool_body.len() + w.tool_data.len()) as u64);

        let report = check::check_drive(w.root(), &w.drive(fs), &profile(), Some(&w.index));
        assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);
        no_partials_anywhere(w.root());

        let events = w.events.lock().unwrap();
        for stage in [
            Stage::Queued,
            Stage::Download,
            Stage::Verify,
            Stage::Copy,
            Stage::Readback,
            Stage::Done,
        ] {
            assert!(events.iter().any(|e| e.stage == stage), "missing stage {stage:?}");
        }
        // Both files count toward one running total.
        let total = rec.size;
        assert!(events.iter().all(|e| e.stage == Stage::Error || e.bytes_total == total));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn swapping_packages_needs_consent_then_replaces_everything() {
    let w = World::new();
    w.init();
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let err = w
        .install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Occupied { ref current } if current == "Nemesis Core"),
        "{err}"
    );
    assert_eq!(w.files(), ["ota_package.tar"], "nothing changes without consent");

    let out = w.install(&["example-tool"], "exfat", swap()).await.unwrap();
    assert_eq!(out.removed, ["nemesis-core"]);
    assert_eq!(w.files(), ["NEMESIS.data", "packages/example-tool.zip"]);

    // Swap to a different tool: its own NEMESIS.data replaces the old one.
    w.install(&["my.tool"], "exfat", swap()).await.unwrap();
    assert_eq!(w.files(), ["NEMESIS.data", "packages/my.tool.zip"]);
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.other_data);
    assert_eq!(fs::read(w.root().join("packages/my.tool.zip")).unwrap(), w.other_body);
    let m = manifest::load(w.root()).unwrap();
    assert_eq!(m.installed.len(), 1);
    assert_eq!(m.installed[0].id, "my.tool");

    // And back to Core: nothing of the tool is left, packages/ included.
    w.install(&["nemesis-core"], "exfat", swap()).await.unwrap();
    assert_eq!(w.files(), ["ota_package.tar"]);
    assert!(w.dirs().is_empty(), "left behind: {:?}", w.dirs());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_package_per_install() {
    let w = World::new();
    w.init();
    let err = w
        .install(&["example-tool", "my.tool"], "exfat", swap())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Profile(_)), "{err}");
    assert!(w.files().is_empty());
}

/// Downloads happen before the drive is touched: a bad download during a swap
/// leaves the old package exactly as it was.
#[tokio::test(flavor = "multi_thread")]
async fn failed_download_during_a_swap_keeps_the_old_package() {
    let mut w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    w.pkg_mut("my.tool").data.as_mut().unwrap().sha256 = "0".repeat(64);

    let err = w.install(&["my.tool"], "exfat", swap()).await.unwrap_err();
    assert!(
        matches!(err, Error::HashMismatch { ref id, .. } if id.contains("data file")),
        "{err}"
    );
    assert_eq!(w.files(), ["NEMESIS.data", "packages/example-tool.zip"]);
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.tool_data);
    assert_eq!(manifest::load(w.root()).unwrap().installed[0].id, "example-tool");
}

/// Stand-in for pulling the stick mid-swap: the new files are cut off while
/// being staged. The old package is still there and intact.
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_swap_keeps_the_old_package() {
    let w = World::with_core_size(3 * 1024 * 1024);
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let cancel = CancelToken::new();
    let trip = cancel.clone();
    let sink: ProgressFn = Arc::new(move |p: Progress| {
        if p.stage == Stage::Copy && p.bytes_done > 0 {
            trip.cancel();
        }
    });
    let err = w
        .install_with(&["nemesis-core"], "exfat", swap(), cancel, sink)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert_eq!(w.files(), ["NEMESIS.data", "packages/example-tool.zip"]);
    assert_eq!(manifest::load(w.root()).unwrap().installed[0].id, "example-tool");
    no_partials_anywhere(w.root());
}

/// Phase 2 gate: a corrupted cache file is detected and downloaded again.
#[tokio::test(flavor = "multi_thread")]
async fn corrupted_cache_is_redownloaded() {
    let w = World::new();
    w.init();
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    assert_eq!(w.server.hits_for("/core/ota_package.tar").len(), 1);

    let reinstall = InstallOptions {
        reinstall: true,
        ..Default::default()
    };
    w.install(&["nemesis-core"], "exfat", reinstall.clone()).await.unwrap();
    assert_eq!(
        w.server.hits_for("/core/ota_package.tar").len(),
        1,
        "served from the cache"
    );

    let cached = w.cached(&sha(&w.core_body));
    let mut bytes = fs::read(&cached).unwrap();
    bytes[100] ^= 0xff;
    fs::write(&cached, &bytes).unwrap();

    w.install(&["nemesis-core"], "exfat", reinstall).await.unwrap();
    assert_eq!(
        w.server.hits_for("/core/ota_package.tar").len(),
        2,
        "should download again"
    );
    assert_eq!(fs::read(&cached).unwrap(), w.core_body);
    assert_eq!(fs::read(w.root().join("ota_package.tar")).unwrap(), w.core_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_hash_fails_and_writes_nothing() {
    let mut w = World::new();
    w.init();
    w.pkg_mut("nemesis-core").sha256 = "0".repeat(64);
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HashMismatch { .. }), "{err}");
    assert!(w.files().is_empty());
    assert!(manifest::load(w.root()).unwrap().installed.is_empty());
    let leftovers: Vec<_> = fs::read_dir(w.cache_dir.path().join("pkgs")).unwrap().collect();
    assert!(leftovers.is_empty(), "a bad download must not stay in the cache");
    let events = w.events.lock().unwrap();
    assert!(events.iter().any(|e| e.stage == Stage::Error && e.id == "nemesis-core"));
}

/// Network loss mid-download: the next attempt resumes with a Range request.
#[tokio::test(flavor = "multi_thread")]
async fn dropped_connection_resumes_with_range() {
    let w = World::with_core_size(512 * 1024);
    w.init();
    w.server
        .flaky
        .lock()
        .unwrap()
        .insert("/core/ota_package.tar".into(), 200 * 1024);
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    let hits = w.server.hits_for("/core/ota_package.tar");
    assert!(hits.len() >= 2, "expected a retry, got {hits:?}");
    assert!(
        hits.iter()
            .skip(1)
            .any(|r| r.as_deref().is_some_and(|r| r.starts_with("bytes=") && r != "bytes=0-")),
        "expected a resumed Range request, got {hits:?}"
    );
    assert_eq!(fs::read(w.root().join("ota_package.tar")).unwrap(), w.core_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_asset_is_a_clear_error() {
    let mut w = World::new();
    w.init();
    let missing = format!("{}/missing.tar", w.server.base);
    w.pkg_mut("nemesis-core").url = missing;
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HttpStatus { status: 404, .. }), "{err}");
}

fn unpack() -> InstallOptions {
    InstallOptions {
        extract: BTreeMap::from([("example-tool".to_string(), true)]),
        ..Default::default()
    }
}

/// Unpacked, a zip goes straight into the drive root next to NEMESIS.data,
/// keeping its own folders.
#[tokio::test(flavor = "multi_thread")]
async fn unpacking_goes_into_the_drive_root() {
    let w = World::new();
    w.init();
    w.install(&["example-tool"], "fat32", unpack()).await.unwrap();
    assert_eq!(
        w.files(),
        [
            "NEMESIS.data",
            "launchpad/install.sh",
            "launchpad/lib/common.sh",
            "packages/paytondev.launchpad.zip"
        ]
    );
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.tool_data);
    assert_eq!(
        fs::read(w.root().join("launchpad/install.sh")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    let rec = manifest::load(w.root()).unwrap().get("example-tool").unwrap().clone();
    assert!(rec.extracted);
    assert_eq!(rec.paths.len(), 4, "NEMESIS.data plus three unpacked files");
    assert_eq!(rec.data.as_deref(), Some("NEMESIS.data"));
    assert!(!rec.dirs.iter().any(String::is_empty), "the drive root is never owned");
    assert!(!manifest::staging_dir(w.root()).exists(), "staging cleaned up");
    let report = check::check_drive(w.root(), &w.drive("fat32"), &profile(), Some(&w.index));
    assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);
    let events = w.events.lock().unwrap().clone();
    assert!(events.iter().any(|e| e.stage == Stage::Extract));

    // Back to the plain zip: the unpacked files and their folders go.
    w.install(&["example-tool"], "fat32", InstallOptions::default())
        .await
        .unwrap();
    assert_eq!(w.files(), ["NEMESIS.data", "packages/example-tool.zip"]);
    assert_eq!(w.dirs(), ["packages"]);

    uninstall::uninstall(w.root(), "example-tool", &[]).unwrap();
    assert!(w.files().is_empty(), "uninstall removes NEMESIS.data too");
    assert!(w.dirs().is_empty(), "and the folders it leaves empty");
}

/// Replacing an unpacked package with Core leaves only the Core .tar: every
/// unpacked file goes, and so do the folders that leaves empty.
#[tokio::test(flavor = "multi_thread")]
async fn swapping_an_unpacked_package_for_core_leaves_only_the_tar() {
    let w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", unpack()).await.unwrap();
    fs::write(w.root().join("launchpad/notes.txt"), b"the user's own file").unwrap();

    w.install(&["nemesis-core"], "exfat", swap()).await.unwrap();
    assert_eq!(w.files(), ["launchpad/notes.txt", "ota_package.tar"]);
    assert_eq!(
        w.dirs(),
        ["launchpad"],
        "only the folder still holding the user's file stays"
    );
    fs::remove_file(w.root().join("launchpad/notes.txt")).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unpacking_wont_overwrite_files_it_didnt_write() {
    let w = World::new();
    w.init();
    fs::create_dir_all(w.root().join("launchpad")).unwrap();
    fs::write(w.root().join("launchpad/install.sh"), b"copied by hand").unwrap();

    let err = w.install(&["example-tool"], "exfat", unpack()).await.unwrap_err();
    assert!(
        matches!(err, Error::Conflict { ref path } if path.contains("launchpad/install.sh")),
        "{err}"
    );
    assert_eq!(w.files(), ["launchpad/install.sh"], "nothing else was written");
    assert_eq!(
        fs::read(w.root().join("launchpad/install.sh")).unwrap(),
        b"copied by hand"
    );
    no_partials_anywhere(w.root());

    let mut opts = unpack();
    opts.replace_untracked = true;
    w.install(&["example-tool"], "exfat", opts).await.unwrap();
    assert_eq!(
        fs::read(w.root().join("launchpad/install.sh")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
}

/// A file in the way is found before anything comes off the stick, so the
/// package already there stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_conflict_during_a_swap_keeps_the_old_package() {
    let w = World::new();
    w.init();
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    fs::create_dir_all(w.root().join("packages")).unwrap();
    fs::write(w.root().join("packages/paytondev.launchpad.zip"), b"copied by hand").unwrap();

    let mut opts = unpack();
    opts.replace_installed = true;
    let err = w.install(&["example-tool"], "exfat", opts).await.unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err}");
    assert_eq!(w.files(), ["ota_package.tar", "packages/paytondev.launchpad.zip"]);
    assert_eq!(manifest::load(w.root()).unwrap().installed[0].id, "nemesis-core");
}

#[tokio::test(flavor = "multi_thread")]
async fn unpacking_refuses_the_app_folder_and_the_data_file() {
    for (entry, expect_unsafe) in [(".nempm/manifest.json", true), ("NEMESIS.data", false)] {
        let mut w = World::new();
        w.init();
        let bad = make_zip(&[("launchpad/ok.sh", b"fine"), (entry, b"overwrite")]);
        w.server.put("/example-tool/example-tool.zip", bad.clone());
        let tool = w.pkg_mut("example-tool");
        tool.sha256 = sha(&bad);
        tool.size = bad.len() as u64;
        let err = w.install(&["example-tool"], "exfat", unpack()).await.unwrap_err();
        if expect_unsafe {
            assert!(matches!(err, Error::UnsafeArchive { .. }), "{entry}: {err}");
        } else {
            assert!(
                matches!(err, Error::InvalidData(ref m) if m.contains("NEMESIS.data")),
                "{entry}: {err}"
            );
        }
        assert!(w.files().is_empty(), "{entry}: nothing written");
        assert!(manifest::load(w.root()).is_ok(), "{entry}: manifest untouched");
    }
}

/// Makes example-tool a release with just a zip: no NEMESIS.data asset.
fn zip_only(w: &mut World, files: &[(&str, &[u8])]) {
    let body = make_zip(files);
    w.server.put("/example-tool/example-tool.zip", body.clone());
    let tool = w.pkg_mut("example-tool");
    tool.sha256 = sha(&body);
    tool.size = body.len() as u64;
    tool.data = None;
}

/// A release can be just a zip holding the stick layout, NEMESIS.data
/// included. It is always unpacked into the root, so the data file lands
/// where the device looks for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_zip_that_carries_its_own_data_file_is_unpacked_into_the_root() {
    let mut w = World::new();
    w.init();
    zip_only(
        &mut w,
        &[
            ("NEMESIS.data", b"bundled data"),
            ("launchpad/install.sh", b"#!/bin/sh\n"),
            ("packages/paytondev.launchpad.zip", b"inner package"),
        ],
    );
    // Asking for the plain zip doesn't stop it being unpacked.
    let keep_zipped = InstallOptions {
        extract: BTreeMap::from([("example-tool".to_string(), false)]),
        ..Default::default()
    };
    w.install(&["example-tool"], "exfat", keep_zipped).await.unwrap();
    assert_eq!(
        w.files(),
        [
            "NEMESIS.data",
            "launchpad/install.sh",
            "packages/paytondev.launchpad.zip"
        ]
    );
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), b"bundled data");
    assert!(w.server.hits_for("/example-tool/NEMESIS.data").is_empty());
    let rec = manifest::load(w.root()).unwrap().get("example-tool").unwrap().clone();
    assert!(rec.extracted);
    assert_eq!(rec.data.as_deref(), Some("NEMESIS.data"));
    assert_eq!(rec.paths.len(), 3);
    let report = check::check_drive(w.root(), &w.drive("exfat"), &profile(), Some(&w.index));
    assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);

    // Swapped for Core, nothing of it stays, NEMESIS.data included.
    w.install(&["nemesis-core"], "exfat", swap()).await.unwrap();
    assert_eq!(w.files(), ["ota_package.tar"]);
    assert!(w.dirs().is_empty());
}

/// Without a NEMESIS.data asset or one in the zip, the install stops before
/// the package already on the stick comes off.
#[tokio::test(flavor = "multi_thread")]
async fn a_zip_without_any_data_file_is_refused() {
    let mut w = World::new();
    w.init();
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    // Zipping a folder puts everything one level down: still not at the root.
    zip_only(
        &mut w,
        &[
            ("launchpad/NEMESIS.data", b"wrong place"),
            ("launchpad/install.sh", b"x"),
        ],
    );

    let err = w.install(&["example-tool"], "exfat", swap()).await.unwrap_err();
    assert!(
        matches!(err, Error::InvalidData(ref m) if m.contains("has no NEMESIS.data")),
        "{err}"
    );
    assert_eq!(w.files(), ["ota_package.tar"]);
    assert_eq!(manifest::load(w.root()).unwrap().installed[0].id, "nemesis-core");
    no_partials_anywhere(w.root());
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_slip_archive_is_rejected_before_writing() {
    let mut w = World::new();
    w.init();
    let evil = make_zip(&[("ok.txt", b"fine"), ("../../escaped.txt", b"gotcha")]);
    w.server.put("/example-tool/example-tool.zip", evil.clone());
    let tool = w.pkg_mut("example-tool");
    tool.sha256 = sha(&evil);
    tool.size = evil.len() as u64;
    let opts = InstallOptions {
        extract: BTreeMap::from([("example-tool".to_string(), true)]),
        ..Default::default()
    };
    let err = w.install(&["example-tool"], "exfat", opts).await.unwrap_err();
    assert!(matches!(err, Error::UnsafeArchive { .. }), "{err}");
    assert!(w.files().is_empty(), "the staged NEMESIS.data is discarded too");
    assert!(!w.root().parent().unwrap().join("escaped.txt").exists());
    no_partials_anywhere(w.root());
}

#[tokio::test(flavor = "multi_thread")]
async fn fat32_size_limit_is_checked_before_downloading() {
    let mut w = World::new();
    w.init();
    w.pkg_mut("nemesis-core").size = 5_000_000_000;
    let err = w
        .install(&["nemesis-core"], "fat32", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::FileTooLarge { .. }), "{err}");
    assert!(
        w.server.hits_for("/core/ota_package.tar").is_empty(),
        "no download attempted"
    );

    let report = check::check_drive(w.root(), &w.drive("fat32"), &profile(), Some(&w.index));
    assert!(report.issues.iter().any(|i| i.code == IssueCode::TooLargeForFilesystem));
}

#[tokio::test(flavor = "multi_thread")]
async fn not_enough_space_is_checked_before_downloading() {
    let mut w = World::new();
    w.init();
    w.pkg_mut("nemesis-core").size = 1 << 55;
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotEnoughSpace { .. }), "{err}");
    assert!(w.server.hits_for("/core/ota_package.tar").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_replace_the_package_and_its_data_file() {
    let mut w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let v2 = make_zip(&[("README.txt", b"version two")]);
    let v2_data = b"NEMESIS.data for 1.1.0".to_vec();
    w.server.put("/example-tool/v2/example-tool.zip", v2.clone());
    w.server.put("/example-tool/v2/NEMESIS.data", v2_data.clone());
    let base = w.server.base.clone();
    let tool = w.pkg_mut("example-tool");
    tool.version = "1.1.0".into();
    tool.sha256 = sha(&v2);
    tool.size = v2.len() as u64;
    tool.url = format!("{base}/example-tool/v2/example-tool.zip");
    tool.data = Some(DataAsset {
        url: format!("{base}/example-tool/v2/NEMESIS.data"),
        sha256: sha(&v2_data),
        size: v2_data.len() as u64,
    });

    let state = check::read_drive(w.root(), Some(&w.index));
    assert_eq!(state.updates_available, 1);

    let out = w
        .install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    assert!(out.removed.is_empty(), "an update isn't a swap");
    assert_eq!(fs::read(w.root().join("packages/example-tool.zip")).unwrap(), v2);
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), v2_data);
    assert_eq!(
        manifest::load(w.root()).unwrap().get("example-tool").unwrap().version,
        "1.1.0"
    );
    assert_eq!(check::read_drive(w.root(), Some(&w.index)).updates_available, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn untracked_files_need_consent() {
    let w = World::new();
    w.init();
    fs::write(w.root().join("NEMESIS.data"), b"copied by hand").unwrap();
    let report = check::check_drive(w.root(), &w.drive("exfat"), &profile(), Some(&w.index));
    let untracked: Vec<_> = report
        .issues
        .iter()
        .filter(|i| i.code == IssueCode::UntrackedFile)
        .collect();
    assert_eq!(untracked.len(), 1, "reported once, not once per package");

    let err = w
        .install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err}");
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), b"copied by hand");

    let opts = InstallOptions {
        replace_untracked: true,
        ..Default::default()
    };
    w.install(&["example-tool"], "exfat", opts).await.unwrap();
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.tool_data);
}

#[tokio::test(flavor = "multi_thread")]
async fn uninitialized_drive_is_refused() {
    let w = World::new();
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotInitialized));
}

/// Stand-in for pulling the stick mid-copy on an empty stick: the manifest
/// stays valid, nothing is recorded, and no partial file is left behind.
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_copy_leaves_a_valid_drive() {
    let w = World::with_core_size(3 * 1024 * 1024);
    w.init();
    let cancel = CancelToken::new();
    let trip = cancel.clone();
    let events = w.events.clone();
    let sink: ProgressFn = Arc::new(move |p: Progress| {
        if p.stage == Stage::Copy && p.bytes_done > 0 {
            trip.cancel();
        }
        events.lock().unwrap().push(p);
    });
    let err = w
        .install_with(&["nemesis-core"], "exfat", InstallOptions::default(), cancel, sink)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(manifest::load(w.root()).unwrap().installed.is_empty());
    assert!(w.files().is_empty());
    no_partials_anywhere(w.root());
    let events = w.events.lock().unwrap();
    assert!(
        events.iter().any(|e| e.id == "nemesis-core" && e.stage == Stage::Error),
        "the UI gets a terminal event"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_change_is_detected_and_migrated() {
    let w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let mut v3 = profile();
    v3.version += 1;
    let slot = v3.slots.get_mut("packages").unwrap();
    slot.path = "tools".into();
    slot.extract_dir = Some("tools/{id}".into());
    slot.data_file = Some("data/NEMESIS.data".into());

    let report = check::check_drive(w.root(), &w.drive("exfat"), &v3, Some(&w.index));
    let issue = report
        .issues
        .iter()
        .find(|i| i.code == IssueCode::ProfileOutdated)
        .unwrap();
    assert_eq!(issue.fix, Some(FixAction::MigrateProfile));

    // Installs are refused until the drive is migrated.
    let mut idx = w.index.clone();
    idx.profiles = vec![v3.clone()];
    let ids = vec!["example-tool".to_string()];
    let drive = w.drive("exfat");
    let options = InstallOptions {
        reinstall: true,
        ..Default::default()
    };
    let err = install::install(
        &w.downloader(CancelToken::new()),
        w.sink(),
        InstallRequest {
            root: w.root(),
            drive: &drive,
            profile: &v3,
            index: &idx,
            ids: &ids,
            options: &options,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Profile(_)), "{err}");

    check::fix_drive(w.root(), &v3, Some(&idx), &[FixAction::MigrateProfile]).unwrap();
    assert_eq!(w.files(), ["data/NEMESIS.data", "tools/example-tool.zip"]);
    let m = manifest::load(w.root()).unwrap();
    assert_eq!(m.profile.version, v3.version);
    let rec = m.get("example-tool").unwrap();
    assert_eq!(rec.paths, ["data/NEMESIS.data", "tools/example-tool.zip"]);
    assert_eq!(rec.data.as_deref(), Some("data/NEMESIS.data"));
    let report = check::check_drive(w.root(), &w.drive("exfat"), &v3, Some(&idx));
    assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);
}

#[tokio::test(flavor = "multi_thread")]
async fn index_fetch_uses_etag_and_falls_back_to_cache() {
    let body = serde_json::to_vec(&World::new().index).unwrap();
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            if n >= 2 {
                // From the third request on, the registry is down.
                let _ = req.respond(tiny_http::Response::empty(503));
                continue;
            }
            let matches = req
                .headers()
                .iter()
                .any(|h| h.field.equiv("If-None-Match") && h.value.as_str() == "\"v1\"");
            let etag = tiny_http::Header::from_bytes("ETag", "\"v1\"").unwrap();
            let resp = if matches {
                tiny_http::Response::from_data(Vec::new())
                    .with_status_code(304)
                    .with_header(etag)
            } else {
                tiny_http::Response::from_data(body.clone()).with_header(etag)
            };
            let _ = req.respond(resp);
        }
    });
    let url = format!("http://127.0.0.1:{port}/index.json");
    let cache = tempfile::tempdir().unwrap();
    let client = http::client().unwrap();

    let first = index::fetch(&client, &url, cache.path()).await.unwrap();
    assert_eq!(first.source, index::IndexSource::Network);
    assert_eq!(first.index.packages.len(), 3);

    let second = index::fetch(&client, &url, cache.path()).await.unwrap();
    assert_eq!(second.source, index::IndexSource::NotModified);

    // Third request: the server is down → offline, from cache.
    let third = index::fetch(&client, &url, cache.path()).await.unwrap();
    assert_eq!(third.source, index::IndexSource::Cache);
    assert!(third.offline);
    assert!(third.error.is_some());
    assert_eq!(third.index.packages.len(), 3);

    // No cache and no server: an error, not a crash.
    let empty = tempfile::tempdir().unwrap();
    assert!(index::fetch(&client, &url, empty.path()).await.is_err());

    // The saved copy belongs to its URL: switching the index URL must not
    // show the old registry's packages as an "offline" list.
    assert!(index::cached(cache.path(), &url).is_some());
    assert!(index::cached(cache.path(), "https://example.com/other/index.json").is_none());
    let other = format!("http://127.0.0.1:{port}/elsewhere.json");
    assert!(index::fetch(&client, &other, cache.path()).await.is_err());
}

#[test]
fn bad_index_entries_are_dropped_with_warnings() {
    let mut idx = World::new().index;
    idx.packages[1].id = "../evil".into();
    let bytes = serde_json::to_vec(&idx).unwrap();
    let (parsed, warnings) = index::parse(&bytes).unwrap();
    assert_eq!(parsed.packages.len(), 2);
    assert_eq!(warnings.len(), 1);

    idx.schema_version = 99;
    let bytes = serde_json::to_vec(&idx).unwrap();
    assert!(index::parse(&bytes).is_err());
}

/// A stick written with layout version 2, which unpacked zips into
/// packages/<id>/, migrates to the current layout (unpacked into the root).
#[tokio::test(flavor = "multi_thread")]
async fn a_stick_unpacked_under_layout_2_migrates_to_the_root() {
    let w = World::new();
    let current = profile();
    let mut v2 = current.clone();
    v2.version = current.version - 1;
    v2.slots.get_mut("packages").unwrap().extract_dir = Some("packages/{id}".into());
    let mut idx2 = w.index.clone();
    idx2.profiles = vec![v2.clone()];
    check::init_drive(w.root(), &v2).unwrap();
    let ids = vec!["example-tool".to_string()];
    let drive = w.drive("exfat");
    install::install(
        &w.downloader(CancelToken::new()),
        w.sink(),
        InstallRequest {
            root: w.root(),
            drive: &drive,
            profile: &v2,
            index: &idx2,
            ids: &ids,
            options: &unpack(),
        },
    )
    .await
    .unwrap();
    // Make the record look exactly like the previous build wrote it.
    let mut m = manifest::load(w.root()).unwrap();
    let rec = &mut m.installed[0];
    rec.unpacked_into = None;
    rec.dirs = vec!["packages/example-tool".into()];
    manifest::save(w.root(), &mut m).unwrap();
    assert!(
        w.files()
            .contains(&"packages/example-tool/launchpad/install.sh".to_string())
    );

    let report = check::check_drive(w.root(), &w.drive("exfat"), &current, Some(&w.index));
    assert!(report.issues.iter().any(|i| i.code == IssueCode::ProfileOutdated));
    check::fix_drive(w.root(), &current, Some(&w.index), &[FixAction::MigrateProfile]).unwrap();

    assert_eq!(
        w.files(),
        [
            "NEMESIS.data",
            "launchpad/install.sh",
            "launchpad/lib/common.sh",
            "packages/paytondev.launchpad.zip"
        ]
    );
    assert_eq!(w.dirs(), ["launchpad", "launchpad/lib", "packages"]);
    let m = manifest::load(w.root()).unwrap();
    assert_eq!(m.profile.version, current.version);
    assert_eq!(m.installed[0].unpacked_into.as_deref(), Some(""));
    let report = check::check_drive(w.root(), &w.drive("exfat"), &current, Some(&w.index));
    assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);

    // And from there, swapping to Core still leaves only the .tar.
    w.install(&["nemesis-core"], "exfat", swap()).await.unwrap();
    assert_eq!(w.files(), ["ota_package.tar"]);
    assert!(w.dirs().is_empty(), "left behind: {:?}", w.dirs());
}
