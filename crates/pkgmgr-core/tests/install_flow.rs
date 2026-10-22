//! End-to-end tests: a local HTTP server stands in for GitHub Releases and a
//! temporary folder poses as the USB drive.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pkgmgr_core::cache::Downloader;
use pkgmgr_core::check::{self, FixAction, IssueCode};
use pkgmgr_core::drives::DriveInfo;
use pkgmgr_core::install::{self, InstallOptions, InstallRequest};
use pkgmgr_core::model::{Index, Package, PackageFormat, Profile};
use pkgmgr_core::progress::{CancelToken, Progress, ProgressFn, Stage};
use pkgmgr_core::{Error, http, index, manifest, uninstall};
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

fn package(id: &str, format: PackageFormat, slot: &str, body: &[u8], base: &str) -> Package {
    Package {
        id: id.into(),
        name: id.into(),
        description: String::new(),
        version: "1.0.0".into(),
        format,
        core: format == PackageFormat::Tar,
        url: format!("{base}/{id}.{}", format.as_str()),
        sha256: sha(body),
        size: body.len() as u64,
        extract: false,
        slot: slot.into(),
        profiles: vec!["nemesis".into()],
        deps: if format == PackageFormat::Zip {
            vec!["nemesis-core".into()]
        } else {
            vec![]
        },
        homepage: None,
    }
}

// ---------- test server ----------

/// (path, Range header) for every request the server saw.
type Hits = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Serves fixed files; supports `Range: bytes=N-`; "flaky" paths drop the
/// connection halfway through their first response.
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
}

impl World {
    fn new() -> Self {
        Self::with_core_size(64 * 1024)
    }

    fn with_core_size(core_size: usize) -> Self {
        let server = Server::start();
        let core_body = make_tar(core_size);
        let tool_body = make_zip(&[
            ("example-tool/", b""),
            ("example-tool/run.sh", b"#!/bin/sh\necho hi\n"),
            ("example-tool/data/config.json", b"{\"a\":1}"),
            ("README.txt", b"hello"),
        ]);
        let core = package("nemesis-core", PackageFormat::Tar, "core", &core_body, &server.base);
        let tool = package("example-tool", PackageFormat::Zip, "packages", &tool_body, &server.base);
        server.put("/nemesis-core.tar", core_body.clone());
        server.put("/example-tool.zip", tool_body.clone());
        let index = Index {
            schema_version: 1,
            generated_at: "2026-10-21T00:00:00Z".into(),
            packages: vec![core, tool],
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

    async fn install(
        &self,
        ids: &[&str],
        fs: &str,
        options: InstallOptions,
    ) -> pkgmgr_core::Result<install::InstallOutcome> {
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
    ) -> pkgmgr_core::Result<install::InstallOutcome> {
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

    fn cached(&self, pkg: &str) -> PathBuf {
        let sha = &self.index.package(pkg).unwrap().sha256;
        self.cache_dir.path().join("pkgs").join(sha)
    }

    fn init(&self) {
        check::init_drive(self.root(), &profile()).unwrap();
    }
}

fn no_partials_anywhere(root: &Path) {
    assert!(
        check::find_partials(root, &profile()).is_empty(),
        "leftover partial files"
    );
}

// ---------- tests ----------

/// Phase 2 gate, part 1: both sample packages install onto FAT32- and
/// exFAT-flagged drives, in the profile's layout.
#[tokio::test(flavor = "multi_thread")]
async fn installs_core_and_tool_on_fat32_and_exfat() {
    for fs in ["fat32", "exfat"] {
        let w = World::new();
        w.init();
        let out = w
            .install(&["example-tool"], fs, InstallOptions::default())
            .await
            .unwrap();
        let ids: Vec<_> = out.installed.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["nemesis-core", "example-tool"], "core first");

        assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.core_body);
        assert_eq!(
            fs::read(w.root().join("packages/example-tool.zip")).unwrap(),
            w.tool_body
        );

        let m = manifest::load(w.root()).unwrap();
        assert_eq!(m.installed.len(), 2);
        assert_eq!(m.get("nemesis-core").unwrap().paths, ["NEMESIS.data"]);
        assert_eq!(m.get("example-tool").unwrap().paths, ["packages/example-tool.zip"]);
        assert!(!m.get("example-tool").unwrap().extracted);
        assert!(m.written_by.starts_with("pkgmgr "));

        let report = check::check_drive(w.root(), &w.drive(fs), &profile(), Some(&w.index));
        assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);
        no_partials_anywhere(w.root());

        let state = check::read_drive(w.root(), Some(&w.index));
        assert_eq!(state.installed.len(), 2);
        assert_eq!(state.updates_available, 0);

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
    }
}

/// Phase 2 gate, part 2: a corrupted cache file is detected and downloaded again.
#[tokio::test(flavor = "multi_thread")]
async fn corrupted_cache_is_redownloaded() {
    let w = World::new();
    w.init();
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    assert_eq!(w.server.hits_for("/nemesis-core.tar").len(), 1);

    // Reinstalling uses the cache: no new request.
    let reinstall = InstallOptions {
        reinstall: true,
        ..Default::default()
    };
    w.install(&["nemesis-core"], "exfat", reinstall.clone()).await.unwrap();
    assert_eq!(w.server.hits_for("/nemesis-core.tar").len(), 1);

    // Flip a byte in the cached copy.
    let cached = w.cached("nemesis-core");
    let mut bytes = fs::read(&cached).unwrap();
    bytes[100] ^= 0xff;
    fs::write(&cached, &bytes).unwrap();

    w.install(&["nemesis-core"], "exfat", reinstall).await.unwrap();
    assert_eq!(w.server.hits_for("/nemesis-core.tar").len(), 2, "should download again");
    assert_eq!(fs::read(&cached).unwrap(), w.core_body);
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.core_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_hash_fails_and_writes_nothing() {
    let mut w = World::new();
    w.init();
    w.index.packages[0].sha256 = "0".repeat(64);
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HashMismatch { .. }), "{err}");
    assert!(!w.root().join("NEMESIS.data").exists());
    assert!(manifest::load(w.root()).unwrap().installed.is_empty());
    let leftovers: Vec<_> = fs::read_dir(w.cache_dir.path().join("pkgs")).unwrap().collect();
    assert!(leftovers.is_empty(), "bad download must not stay in the cache");
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
        .insert("/nemesis-core.tar".into(), 200 * 1024);
    w.install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    let hits = w.server.hits_for("/nemesis-core.tar");
    assert!(hits.len() >= 2, "expected a retry, got {hits:?}");
    assert!(
        hits.iter()
            .skip(1)
            .any(|r| r.as_deref().is_some_and(|r| r.starts_with("bytes=") && r != "bytes=0-")),
        "expected a resumed Range request, got {hits:?}"
    );
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.core_body);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_asset_is_a_clear_error() {
    let mut w = World::new();
    w.init();
    w.index.packages[0].url = format!("{}/missing.tar", w.server.base);
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HttpStatus { status: 404, .. }), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn extraction_is_optional_and_uninstall_removes_exactly_those_files() {
    let w = World::new();
    w.init();
    let opts = InstallOptions {
        extract: BTreeMap::from([("example-tool".to_string(), true)]),
        ..Default::default()
    };
    w.install(&["example-tool"], "fat32", opts).await.unwrap();
    let base = w.root().join("packages/example-tool");
    assert_eq!(
        fs::read(base.join("example-tool/run.sh")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    assert_eq!(
        fs::read(base.join("example-tool/data/config.json")).unwrap(),
        b"{\"a\":1}"
    );
    assert!(!w.root().join("packages/example-tool.zip").exists());
    let rec = manifest::load(w.root()).unwrap().get("example-tool").unwrap().clone();
    assert!(rec.extracted);
    assert_eq!(rec.dirs, ["packages/example-tool"]);
    assert_eq!(rec.paths.len(), 3);
    assert!(!manifest::staging_dir(w.root()).exists(), "staging cleaned up");

    // Switching back to the zip replaces the folder with the file.
    w.install(&["example-tool"], "fat32", InstallOptions::default())
        .await
        .unwrap();
    assert!(w.root().join("packages/example-tool.zip").exists());
    assert!(!base.exists());

    uninstall::uninstall(w.root(), "example-tool", false).unwrap();
    assert!(!w.root().join("packages/example-tool.zip").exists());
    assert!(w.root().join("packages").is_dir(), "required folder stays");
    assert!(w.root().join("NEMESIS.data").exists(), "core untouched");
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_slip_archive_is_rejected_before_writing() {
    let mut w = World::new();
    w.init();
    let evil = make_zip(&[("ok.txt", b"fine"), ("../../escaped.txt", b"gotcha")]);
    w.server.put("/example-tool.zip", evil.clone());
    let tool = w.index.packages.iter_mut().find(|p| p.id == "example-tool").unwrap();
    tool.sha256 = sha(&evil);
    tool.size = evil.len() as u64;
    let opts = InstallOptions {
        extract: BTreeMap::from([("example-tool".to_string(), true)]),
        ..Default::default()
    };
    let err = w.install(&["example-tool"], "exfat", opts).await.unwrap_err();
    assert!(matches!(err, Error::UnsafeArchive { .. }), "{err}");
    assert!(!w.root().join("packages/example-tool").exists());
    assert!(!w.root().parent().unwrap().join("escaped.txt").exists());
    no_partials_anywhere(w.root());
}

#[tokio::test(flavor = "multi_thread")]
async fn fat32_size_limit_is_checked_before_downloading() {
    let mut w = World::new();
    w.init();
    w.index.packages[0].size = 5_000_000_000;
    let err = w
        .install(&["nemesis-core"], "fat32", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::FileTooLarge { .. }), "{err}");
    assert!(
        w.server.hits_for("/nemesis-core.tar").is_empty(),
        "no download attempted"
    );

    let report = check::check_drive(w.root(), &w.drive("fat32"), &profile(), Some(&w.index));
    assert!(report.issues.iter().any(|i| i.code == IssueCode::TooLargeForFilesystem));
}

#[tokio::test(flavor = "multi_thread")]
async fn not_enough_space_is_checked_before_downloading() {
    let mut w = World::new();
    w.init();
    w.index.packages[0].size = 1 << 55;
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotEnoughSpace { .. }), "{err}");
    assert!(w.server.hits_for("/nemesis-core.tar").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_replace_the_old_version() {
    let mut w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let v2 = make_zip(&[("README.txt", b"version two")]);
    w.server.put("/example-tool-v2.zip", v2.clone());
    let tool = w.index.packages.iter_mut().find(|p| p.id == "example-tool").unwrap();
    tool.version = "1.1.0".into();
    tool.sha256 = sha(&v2);
    tool.size = v2.len() as u64;
    tool.url = format!("{}/example-tool-v2.zip", w.server.base);

    let state = check::read_drive(w.root(), Some(&w.index));
    assert_eq!(state.updates_available, 1);
    let view = state.installed.iter().find(|v| v.record.id == "example-tool").unwrap();
    assert!(view.update_available);
    assert_eq!(view.latest.as_deref(), Some("1.1.0"));

    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();
    assert_eq!(fs::read(w.root().join("packages/example-tool.zip")).unwrap(), v2);
    let m = manifest::load(w.root()).unwrap();
    assert_eq!(m.get("example-tool").unwrap().version, "1.1.0");
    assert_eq!(check::read_drive(w.root(), Some(&w.index)).updates_available, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn untracked_files_need_consent() {
    let w = World::new();
    w.init();
    fs::write(w.root().join("NEMESIS.data"), b"copied by hand").unwrap();
    let err = w
        .install(&["nemesis-core"], "exfat", InstallOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err}");
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), b"copied by hand");

    let opts = InstallOptions {
        replace_untracked: true,
        ..Default::default()
    };
    w.install(&["nemesis-core"], "exfat", opts).await.unwrap();
    assert_eq!(fs::read(w.root().join("NEMESIS.data")).unwrap(), w.core_body);
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

/// Stand-in for pulling the stick mid-copy: the copy stops part-way. The
/// manifest stays valid, the package isn't recorded, and no partial file is
/// left behind.
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
        .install_with(&["example-tool"], "exfat", InstallOptions::default(), cancel, sink)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let m = manifest::load(w.root()).unwrap();
    assert!(m.installed.is_empty());
    assert!(!w.root().join("NEMESIS.data").exists());
    no_partials_anywhere(w.root());
    // Every queued package got a terminal event, so the UI never hangs.
    let events = w.events.lock().unwrap();
    for id in ["nemesis-core", "example-tool"] {
        assert!(
            events.iter().any(|e| e.id == id && e.stage == Stage::Error),
            "{id} has no terminal event"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_change_is_detected_and_migrated() {
    let w = World::new();
    w.init();
    w.install(&["example-tool"], "exfat", InstallOptions::default())
        .await
        .unwrap();

    let mut v2 = profile();
    v2.version = 2;
    v2.required_dirs = vec!["tools".into()];
    let slot = v2.slots.get_mut("packages").unwrap();
    slot.path = "tools".into();
    slot.extract_dir = Some("tools/{id}".into());

    let report = check::check_drive(w.root(), &w.drive("exfat"), &v2, Some(&w.index));
    let issue = report
        .issues
        .iter()
        .find(|i| i.code == IssueCode::ProfileOutdated)
        .unwrap();
    assert_eq!(issue.fix, Some(FixAction::MigrateProfile));

    // Installs are refused until the drive is migrated.
    let mut idx = w.index.clone();
    idx.profiles = vec![v2.clone()];
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
            profile: &v2,
            index: &idx,
            ids: &ids,
            options: &options,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Profile(_)), "{err}");

    check::fix_drive(w.root(), &v2, Some(&idx), &[FixAction::MigrateProfile]).unwrap();
    assert!(w.root().join("tools/example-tool.zip").exists());
    assert!(!w.root().join("packages/example-tool.zip").exists());
    let m = manifest::load(w.root()).unwrap();
    assert_eq!(m.profile.version, 2);
    assert_eq!(m.get("example-tool").unwrap().paths, ["tools/example-tool.zip"]);
    let report = check::check_drive(w.root(), &w.drive("exfat"), &v2, Some(&idx));
    assert!(report.ready && report.issues.is_empty(), "{:?}", report.issues);
}

#[tokio::test(flavor = "multi_thread")]
async fn index_fetch_uses_etag_and_falls_back_to_cache() {
    // tiny_http doesn't do ETags itself, so serve the index through a tiny
    // custom server that does.
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
    assert_eq!(first.index.packages.len(), 2);

    let second = index::fetch(&client, &url, cache.path()).await.unwrap();
    assert_eq!(second.source, index::IndexSource::NotModified);

    // Third request: the server is down → offline, from cache.
    let third = index::fetch(&client, &url, cache.path()).await.unwrap();
    assert_eq!(third.source, index::IndexSource::Cache);
    assert!(third.offline);
    assert!(third.error.is_some());
    assert_eq!(third.index.packages.len(), 2);

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
    assert_eq!(parsed.packages.len(), 1);
    assert_eq!(warnings.len(), 1);

    idx.schema_version = 99;
    let bytes = serde_json::to_vec(&idx).unwrap();
    assert!(index::parse(&bytes).is_err());
}
