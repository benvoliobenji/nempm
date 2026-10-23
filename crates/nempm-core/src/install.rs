//! The install pipeline.
//!
//! A Nemesis stick holds one package at a time: Core is its `.tar` at the
//! drive root; any other package is `packages/<id>.zip` plus the
//! `NEMESIS.data` from its release at the root, or a zip unpacked into the
//! root that brings its own `NEMESIS.data`. An install:
//!
//! 1. **Plans** the work: slot rules, the files to write, any package that has
//!    to make room (only once the user confirms), and any file in the way that
//!    this app didn't write (also only with consent).
//! 2. **Fetches** every file into the cache and verifies it. Nothing on the
//!    drive changes until this has succeeded.
//! 3. **Stages** each file as `<dest>.nempm-part` (a zip's contents in a
//!    staging folder) and flushes it to the device.
//! 4. **Removes** the package being replaced, before staging instead if the
//!    old and new files won't fit side by side.
//! 5. **Commits** everything with renames, then **reads it back** from the
//!    device, bypassing the OS cache, and compares hashes.
//! 6. **Records** the files in the drive manifest, written atomically.
//!
//! The drive manifest only ever describes complete, verified packages, so
//! pulling the stick mid-copy leaves a valid manifest and, at worst,
//! `.nempm-part` files that Check drive offers to remove.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::cache::Downloader;
use crate::drives::DriveInfo;
use crate::error::{io_ctx, io_error};
use crate::model::{DriveManifest, Index, InstalledPackage, Package, Profile};
use crate::profile::{self, max_file_size};
use crate::progress::{CancelToken, ProgressFn, Reporter, Stage};
use crate::{Error, Result, fsutil, manifest, paths, uninstall};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct InstallOptions {
    /// Per-package choice to unpack a zip, where the slot allows it.
    #[ts(type = "Record<string, boolean>")]
    pub extract: BTreeMap<String, bool>,
    /// Replace files at the target paths that weren't installed by this app.
    pub replace_untracked: bool,
    /// Remove the package(s) already on the drive to make room, where the
    /// profile limits how many a drive holds.
    pub replace_installed: bool,
    /// Reinstall packages that are already at the index version.
    pub reinstall: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PlanAction {
    Install,
    Update,
    Reinstall,
    Downgrade,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanStep {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_version: Option<String>,
    pub action: PlanAction,
    pub slot: String,
    pub extract: bool,
    /// Bytes written: the package plus its data file.
    #[ts(type = "number")]
    pub size: u64,
    /// Drive-relative file or folder the package itself is written to.
    pub target: String,
    /// Drive-relative path of its data file (Nemesis: `NEMESIS.data`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data_target: Option<String>,
    /// The data file comes inside the zip, so the zip is always unpacked.
    pub data_in_zip: bool,
    /// One of the targets exists but no installed package owns it.
    pub untracked: bool,
}

/// A package removed to make room for the new one.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlanRemoval {
    pub id: String,
    pub name: String,
    pub version: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstallPlan {
    pub steps: Vec<PlanStep>,
    /// Packages that come off the drive first (needs `replaceInstalled`).
    pub removes: Vec<PlanRemoval>,
    /// Requested packages that are already installed at the index version.
    pub up_to_date: Vec<String>,
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// Targets that would replace files this app didn't install (needs
    /// `replaceUntracked`).
    pub untracked: Vec<String>,
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Works out what an install of `ids` does to the drive.
pub fn plan(
    root: &Path,
    index: &Index,
    profile: &Profile,
    manifest: &DriveManifest,
    ids: &[String],
    options: &InstallOptions,
) -> Result<InstallPlan> {
    if ids.is_empty() {
        return Err(Error::InvalidData("Nothing to install".into()));
    }
    let mut seen = HashSet::new();
    let pkgs = ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| index.package(id).ok_or_else(|| Error::UnknownPackage(id.clone())))
        .collect::<Result<Vec<_>>>()?;
    if let Some(max) = profile.max_packages
        && pkgs.len() > max as usize
    {
        return Err(Error::Profile(format!(
            "A {} drive holds {max} package{} at a time. Pick one.",
            profile.display_name(),
            plural(max)
        )));
    }

    // Make room: everything else comes off when the drive would hold too many.
    let requested: HashSet<&str> = pkgs.iter().map(|p| p.id.as_str()).collect();
    let others: Vec<&InstalledPackage> = manifest
        .installed
        .iter()
        .filter(|r| !requested.contains(r.id.as_str()))
        .collect();
    let removes: Vec<&InstalledPackage> = match profile.max_packages {
        Some(max) if others.len() + pkgs.len() > max as usize => others,
        _ => Vec::new(),
    };
    let removed: HashSet<&str> = removes.iter().map(|r| r.id.as_str()).collect();

    let mut steps = Vec::new();
    let mut up_to_date = Vec::new();
    let mut untracked = Vec::new();
    for pkg in pkgs {
        let slot = profile::slot_for(profile, pkg)?;
        let place = profile::placement(profile, pkg)?;
        let extract = profile::effective_extract(slot, pkg, options.extract.get(&pkg.id).copied());
        let installed = manifest.get(&pkg.id);

        let action = match installed {
            None => PlanAction::Install,
            Some(rec) => {
                let intact = rec.paths.iter().all(|p| paths::join(root, p).exists());
                match compare_versions(&pkg.version, &rec.version) {
                    Ordering::Greater => PlanAction::Update,
                    Ordering::Less => PlanAction::Downgrade,
                    Ordering::Equal if options.reinstall || !intact || rec.extracted != extract => {
                        PlanAction::Reinstall
                    }
                    Ordering::Equal => {
                        up_to_date.push(pkg.id.clone());
                        continue;
                    }
                }
            }
        };

        let target = place.target(extract).to_string();
        let mut step_untracked = false;
        // An unpacked zip merges into its folder (possibly the drive root);
        // its files are checked one by one once the zip is downloaded.
        let file_target = (!extract).then_some(&target);
        for path in file_target.into_iter().chain(place.data_file.as_ref()) {
            match manifest.owner_of(path) {
                Some(owner) if owner.id == pkg.id || removed.contains(owner.id.as_str()) => {}
                Some(owner) => {
                    return Err(Error::Conflict {
                        path: format!("{path} (installed by {})", owner.id),
                    });
                }
                None if paths::join(root, path).exists() => {
                    step_untracked = true;
                    untracked.push(path.clone());
                }
                None => {}
            }
        }

        steps.push(PlanStep {
            id: pkg.id.clone(),
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            from_version: installed.map(|r| r.version.clone()),
            action,
            slot: pkg.slot.clone(),
            extract,
            size: pkg.total_size(),
            target,
            data_target: place.data_file.clone(),
            data_in_zip: profile::data_in_zip(slot, pkg),
            untracked: step_untracked,
        });
    }

    for (name, slot) in &profile.slots {
        let Some(max) = slot.max else { continue };
        let mut in_slot: HashSet<&str> = manifest
            .installed
            .iter()
            .filter(|r| &r.slot == name && !removed.contains(r.id.as_str()))
            .map(|r| r.id.as_str())
            .collect();
        in_slot.extend(steps.iter().filter(|s| &s.slot == name).map(|s| s.id.as_str()));
        if in_slot.len() > max as usize {
            let mut list: Vec<_> = in_slot.into_iter().collect();
            list.sort();
            return Err(Error::Profile(format!(
                "{} allows {max} package{} in \"{name}\", but this would make {}: {}. Uninstall one first.",
                profile.display_name(),
                plural(max),
                list.len(),
                list.join(", ")
            )));
        }
    }

    let removes = removes
        .into_iter()
        .map(|r| PlanRemoval {
            id: r.id.clone(),
            name: index.package(&r.id).map_or_else(|| r.id.clone(), |p| p.name.clone()),
            version: r.version.clone(),
            paths: r.paths.clone(),
        })
        .collect();
    let total_bytes = steps.iter().map(|s| s.size).sum();
    Ok(InstallPlan {
        steps,
        removes,
        up_to_date,
        total_bytes,
        untracked,
    })
}

pub struct InstallRequest<'a> {
    pub root: &'a Path,
    pub drive: &'a DriveInfo,
    pub profile: &'a Profile,
    pub index: &'a Index,
    pub ids: &'a [String],
    pub options: &'a InstallOptions,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstallOutcome {
    pub installed: Vec<InstalledPackage>,
    /// Ids of packages taken off the drive to make room.
    pub removed: Vec<String>,
    pub up_to_date: Vec<String>,
}

/// Checks that apply to the whole drive before anything is written.
fn preflight(req: &InstallRequest<'_>, manifest: &DriveManifest, plan: &InstallPlan) -> Result<()> {
    let profile = req.profile;
    if manifest.profile.id != profile.id {
        return Err(Error::Profile(format!(
            "This drive was set up for \"{}\", not {}.",
            manifest.profile.id,
            profile.display_name()
        )));
    }
    if manifest.profile.version != profile.version {
        return Err(Error::Profile(format!(
            "This drive uses {} layout version {}, but the current one is {}. Run Check drive and migrate it first.",
            profile.display_name(),
            manifest.profile.version,
            profile.version
        )));
    }
    if req.drive.read_only {
        return Err(Error::Profile("This drive is read-only.".into()));
    }
    let fs_name = req.drive.filesystem.as_str();
    if fs_name != "unknown" && !profile.filesystems.iter().any(|f| f == fs_name) {
        return Err(Error::Profile(format!(
            "This drive is formatted as {}, which {} can't read. Format it as exFAT first.",
            req.drive.filesystem_raw,
            profile.display_name()
        )));
    }
    if !plan.removes.is_empty() && !req.options.replace_installed {
        return Err(Error::Occupied {
            current: plan
                .removes
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }
    if !plan.untracked.is_empty() && !req.options.replace_untracked {
        return Err(Error::Conflict {
            path: plan.untracked.join(", "),
        });
    }
    if let Some(limit) = max_file_size(profile, fs_name) {
        for step in &plan.steps {
            let pkg = req.index.package(&step.id).expect("planned from the index");
            let files = [(!step.extract).then_some(pkg.size), pkg.data.as_ref().map(|d| d.size)];
            if let Some(size) = files.into_iter().flatten().find(|&s| s > limit) {
                return Err(Error::FileTooLarge {
                    name: step.name.clone(),
                    size,
                    filesystem: req.drive.filesystem_raw.clone(),
                });
            }
        }
    }
    let free = fsutil::free_space(req.root).unwrap_or(req.drive.free_bytes);
    let needed = with_margin(plan.total_bytes);
    // Space taken by what this install replaces or removes comes back.
    let reclaimed: u64 = plan
        .steps
        .iter()
        .map(|s| s.id.as_str())
        .chain(plan.removes.iter().map(|r| r.id.as_str()))
        .filter_map(|id| manifest.get(id))
        .map(|r| r.size)
        .sum();
    if needed > free.saturating_add(reclaimed) {
        return Err(Error::NotEnoughSpace {
            needed,
            available: free,
        });
    }
    Ok(())
}

/// Size plus a 5% margin for filesystem overhead.
fn with_margin(bytes: u64) -> u64 {
    bytes.saturating_add(bytes / 20)
}

/// Installs `req.ids`, replacing what's on the drive if the profile requires
/// it and the user agreed. Emits progress for each package. Stops at the
/// first failure; packages finished before it stay installed and recorded.
pub async fn install(dl: &Downloader, progress: ProgressFn, req: InstallRequest<'_>) -> Result<InstallOutcome> {
    let mut manifest = manifest::load(req.root)?;
    let plan = plan(req.root, req.index, req.profile, &manifest, req.ids, req.options)?;
    preflight(&req, &manifest, &plan)?;

    let reporters: Vec<Arc<Reporter>> = plan
        .steps
        .iter()
        .map(|s| {
            let r = Reporter::new(progress.clone(), &s.id);
            r.stage(Stage::Queued, 0, s.size);
            r
        })
        .collect();
    let fail = |i: usize, e: Error| {
        reporters[i].error(e.to_string());
        let why = if matches!(e, Error::Cancelled) {
            "Cancelled".to_string()
        } else {
            format!("Not installed because {} failed", plan.steps[i].name)
        };
        for r in &reporters[i + 1..] {
            r.error(why.clone());
        }
        e
    };

    // Everything is downloaded and verified before the drive is touched, so a
    // network problem never leaves the stick half-changed.
    let mut fetched = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        let pkg = req.index.package(&step.id).expect("planned from the index");
        let result = match dl.cancel.check() {
            Ok(()) => fetch(dl, pkg, &reporters[i]).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(f) => fetched.push(f),
            Err(e) => return Err(fail(i, e)),
        }
    }

    let mut removals: Vec<InstalledPackage> = plan
        .removes
        .iter()
        .filter_map(|r| manifest.get(&r.id).cloned())
        .collect();
    let removed: Vec<String> = removals.iter().map(|r| r.id.clone()).collect();
    let mut installed = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        let pkg = req.index.package(&step.id).expect("planned from the index");
        let result = match dl.cancel.check() {
            Ok(()) => {
                write_package(
                    dl,
                    &req,
                    pkg,
                    step,
                    &fetched[i],
                    &mut manifest,
                    &mut removals,
                    &reporters[i],
                )
                .await
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(record) => {
                reporters[i].done(step.size, None);
                installed.push(record);
            }
            Err(e) => return Err(fail(i, e)),
        }
    }
    // Only removals left (every requested package was already up to date).
    remove_packages(req.root, &req.profile.required_dirs, &mut manifest, &mut removals)?;

    Ok(InstallOutcome {
        installed,
        removed,
        up_to_date: plan.up_to_date,
    })
}

/// Verified local copies of one package's files.
struct Fetched {
    main: PathBuf,
    data: Option<PathBuf>,
}

async fn fetch(dl: &Downloader, pkg: &Package, reporter: &Arc<Reporter>) -> Result<Fetched> {
    let total = pkg.total_size();
    let mut offset = 0;
    let data = match pkg.data_asset() {
        Some(asset) => {
            reporter.set_scope(0, total);
            let path = dl.ensure(&asset, reporter).await;
            offset = asset.size;
            Some(path)
        }
        None => None,
    }
    .transpose();
    let data = match data {
        Ok(d) => d,
        Err(e) => {
            reporter.clear_scope();
            return Err(e);
        }
    };
    reporter.set_scope(offset, total);
    let main = dl.ensure(&pkg.asset(), reporter).await;
    reporter.clear_scope();
    Ok(Fetched { main: main?, data })
}

/// Takes packages off the drive, saving the manifest after each. Folders they
/// leave empty go too, except the profile's required ones (`keep`).
fn remove_packages(
    root: &Path,
    keep: &[String],
    manifest: &mut DriveManifest,
    removals: &mut Vec<InstalledPackage>,
) -> Result<()> {
    for rec in removals.drain(..) {
        uninstall::remove_files(root, &rec, keep)?;
        manifest.remove(&rec.id);
        manifest::save(root, manifest)?;
    }
    Ok(())
}

/// A failure, and whether it happened after new files replaced old ones.
struct Failure {
    error: Error,
    committed: bool,
}

/// One file or folder to put on the drive.
enum Job {
    File {
        src: PathBuf,
        rel: String,
        sha256: String,
        size: u64,
    },
    Extract {
        zip: PathBuf,
        rel_dir: String,
    },
}

/// Written to the drive and flushed, but not yet in place.
enum Staged {
    File {
        part: PathBuf,
        dest: PathBuf,
        rel: String,
        sha256: String,
        size: u64,
    },
    /// A zip unpacked into a staging folder, to be merged into `rel_dir`
    /// (empty for the drive root) file by file.
    Dir {
        staging: PathBuf,
        rel_dir: String,
        /// (path inside `rel_dir`, sha256, size)
        files: Vec<(String, String, u64)>,
        /// Folders the zip lists, inside `rel_dir`.
        dirs: Vec<String>,
        /// How many of `files` are in place.
        committed: Cell<usize>,
    },
}

impl Staged {
    fn bytes(&self) -> u64 {
        match self {
            Staged::File { size, .. } => *size,
            Staged::Dir { files, .. } => files.iter().map(|f| f.2).sum(),
        }
    }

    fn any_committed(&self) -> bool {
        match self {
            Staged::File { .. } => false,
            Staged::Dir { committed, .. } => committed.get() > 0,
        }
    }

    fn discard_staging(&self) {
        match self {
            Staged::File { part, .. } => {
                let _ = fs::remove_file(part);
            }
            Staged::Dir { staging, .. } => {
                let _ = fs::remove_dir_all(staging);
            }
        }
    }

    /// Removes exactly the files this item put in place (never a whole
    /// folder: a zip may unpack into the drive root), then any folders that
    /// leaves empty.
    fn discard_committed(&self, ctx: &StageContext) {
        match self {
            Staged::File { dest, rel, .. } => {
                let _ = fs::remove_file(dest);
                uninstall::prune_parents(&ctx.root, rel, &ctx.keep);
            }
            Staged::Dir {
                staging,
                rel_dir,
                files,
                committed,
                ..
            } => {
                for (rel, _, _) in &files[..committed.get()] {
                    let full = paths::rel_join(rel_dir, rel);
                    let _ = fs::remove_file(paths::join(&ctx.root, &full));
                    uninstall::prune_parents(&ctx.root, &full, &ctx.keep);
                }
                let _ = fs::remove_dir_all(staging);
            }
        }
    }
}

/// Runs blocking file work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::InvalidData(format!("Install task failed: {e}")))
}

#[allow(clippy::too_many_arguments)]
async fn write_package(
    dl: &Downloader,
    req: &InstallRequest<'_>,
    pkg: &Package,
    step: &PlanStep,
    fetched: &Fetched,
    manifest: &mut DriveManifest,
    removals: &mut Vec<InstalledPackage>,
    reporter: &Arc<Reporter>,
) -> Result<InstalledPackage> {
    let root = req.root.to_path_buf();
    let keep = req.profile.required_dirs.clone();
    let place = profile::placement(req.profile, pkg)?;
    let mut previous = manifest.get(&pkg.id).cloned();

    // Files the old version or a package being swapped out wrote may be
    // overwritten; anything else needs the user's consent.
    let replaceable: HashSet<String> = previous
        .iter()
        .chain(removals.iter())
        .flat_map(|r| r.paths.iter())
        .map(|p| p.to_ascii_lowercase())
        .collect();
    let ctx = Arc::new(StageContext {
        root: root.clone(),
        id: pkg.id.clone(),
        limit: max_file_size(req.profile, &req.drive.filesystem),
        fs_raw: req.drive.filesystem_raw.clone(),
        reporter: reporter.clone(),
        cancel: dl.cancel.clone(),
        keep: keep.clone(),
        reserved: req.profile.reserved.clone(),
        data_rel: place.data_file.clone(),
        data_in_zip: step.data_in_zip,
        replaceable,
        replace_untracked: req.options.replace_untracked,
    });

    // Check an archive against the drive before anything comes off it.
    let mut needed = pkg.total_size();
    if step.extract {
        let rel_dir = place.extract_dir.clone().expect("extract implies an extract dir");
        let (c, zip) = (ctx.clone(), fetched.main.clone());
        let unpacked = blocking(move || scan_extract(&c, &zip, &rel_dir).map(|s| s.total)).await??;
        needed = pkg.data.as_ref().map_or(0, |d| d.size) + unpacked;
    }

    // If the old and new files can't both be on the drive, the old ones go
    // first. Otherwise they stay until the new ones are safely staged.
    let free = fsutil::free_space(&root).map_err(io_ctx("checking free space on the drive"))?;
    if free < with_margin(needed) {
        remove_packages(&root, &keep, manifest, removals)?;
        if let Some(prev) = previous.take() {
            remove_packages(&root, &keep, manifest, &mut vec![prev])?;
        }
    }

    let mut jobs = Vec::new();
    if let (Some(rel), Some(src), Some(data)) = (&place.data_file, &fetched.data, &pkg.data) {
        jobs.push(Job::File {
            src: src.clone(),
            rel: rel.clone(),
            sha256: data.sha256.clone(),
            size: data.size,
        });
    }
    if step.extract {
        jobs.push(Job::Extract {
            zip: fetched.main.clone(),
            rel_dir: place.extract_dir.clone().expect("extract implies an extract dir"),
        });
    } else {
        jobs.push(Job::File {
            src: fetched.main.clone(),
            rel: place.file.clone(),
            sha256: pkg.sha256.clone(),
            size: pkg.size,
        });
    }

    let c = ctx.clone();
    let staged = blocking(move || stage_all(&c, jobs)).await??;

    if let Err(e) = remove_packages(&root, &keep, manifest, removals) {
        staged.iter().for_each(Staged::discard_staging);
        return Err(e);
    }

    let c = ctx.clone();
    let result = blocking(move || commit_and_verify(&c, &staged)).await?;
    let (new_paths, new_dirs) = match result {
        Ok(v) => v,
        Err(Failure { error, committed }) => {
            if committed && let Some(prev) = &previous {
                // The old version's files were replaced by ones that failed
                // verification: forget it so the drive stays consistent.
                let _ = uninstall::remove_files(&root, prev, &keep);
                manifest.remove(&prev.id);
                manifest::save(&root, manifest)?;
            }
            return Err(error);
        }
    };

    let record = InstalledPackage {
        id: pkg.id.clone(),
        version: pkg.version.clone(),
        sha256: pkg.sha256.clone(),
        slot: pkg.slot.clone(),
        format: pkg.format,
        extracted: step.extract,
        paths: new_paths,
        dirs: new_dirs,
        data: ctx.data_rel.clone(),
        size: pkg.total_size(),
        unpacked_into: step.extract.then(|| place.extract_dir.clone().unwrap_or_default()),
        installed_at: manifest::now(),
    };
    manifest.upsert(record.clone());
    manifest::save(&root, manifest)?;

    // Only now remove what the previous version had that this one doesn't.
    if let Some(prev) = previous {
        let still: HashSet<String> = record.paths.iter().map(|p| p.to_ascii_lowercase()).collect();
        let leftover = InstalledPackage {
            paths: prev
                .paths
                .iter()
                .filter(|p| !still.contains(&p.to_ascii_lowercase()))
                .cloned()
                .collect(),
            dirs: prev.dirs.iter().filter(|d| !record.dirs.contains(d)).cloned().collect(),
            ..prev
        };
        uninstall::remove_files(&root, &leftover, &keep)?;
    }
    Ok(record)
}

/// What the blocking stage/commit steps need.
struct StageContext {
    root: PathBuf,
    id: String,
    limit: Option<u64>,
    fs_raw: String,
    reporter: Arc<Reporter>,
    cancel: CancelToken,
    /// The profile's required folders: never pruned.
    keep: Vec<String>,
    /// The profile's reserved names (plus `.nempm`): a zip can't write there.
    reserved: Vec<String>,
    /// Where this package's data file goes. Its zip may only write there
    /// when it carries the file instead of a separate asset.
    data_rel: Option<String>,
    data_in_zip: bool,
    /// Lowercased paths that may be overwritten without asking.
    replaceable: HashSet<String>,
    replace_untracked: bool,
}

/// Writes every job to the drive without putting anything in place yet.
fn stage_all(ctx: &StageContext, jobs: Vec<Job>) -> Result<Vec<Staged>> {
    let copy_total: u64 = jobs
        .iter()
        .map(|j| match j {
            Job::File { size, .. } => *size,
            Job::Extract { .. } => 0,
        })
        .sum();
    let mut copied = 0;
    let mut staged = Vec::new();
    for job in jobs {
        let result = match job {
            Job::File { src, rel, sha256, size } => {
                ctx.reporter.set_scope(copied, copy_total);
                copied += size;
                stage_file(ctx, &src, &rel, sha256, size)
            }
            Job::Extract { zip, rel_dir } => {
                ctx.reporter.clear_scope();
                stage_extract(ctx, &zip, rel_dir)
            }
        };
        match result {
            Ok(s) => staged.push(s),
            Err(e) => {
                ctx.reporter.clear_scope();
                staged.iter().for_each(Staged::discard_staging);
                return Err(e);
            }
        }
    }
    ctx.reporter.clear_scope();
    Ok(staged)
}

fn stage_file(ctx: &StageContext, src: &Path, rel: &str, sha256: String, size: u64) -> Result<Staged> {
    let dest = paths::join(&ctx.root, rel);
    let parent = dest.parent().unwrap_or(&ctx.root).to_path_buf();
    fs::create_dir_all(&parent).map_err(io_ctx(format!("creating the folder for {rel}")))?;
    let part = fsutil::part_path(&dest);
    let _ = fsutil::remove_file_if_exists(&part);

    ctx.reporter.stage(Stage::Copy, 0, size);
    let copied = fsutil::copy_hashed(src, &part, |n| ctx.reporter.bytes(Stage::Copy, n, size), &ctx.cancel);
    let (written_sha, written) = match copied {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    if written_sha != sha256 || written != size {
        let _ = fs::remove_file(&part);
        return Err(Error::InvalidData(
            "The cached download changed while it was being copied. Try again.".into(),
        ));
    }
    Ok(Staged::File {
        part,
        dest,
        rel: rel.to_string(),
        sha256,
        size,
    })
}

/// A zip entry name made safe for the drive: no absolute paths, no `..`, and
/// nothing FAT32/exFAT can't store. Directory-only and empty names map to "".
pub fn safe_entry_path(name: &str) -> Result<String> {
    let unsafe_entry = || Error::UnsafeArchive {
        entry: name.to_string(),
    };
    if name.starts_with('/') || name.starts_with('\\') || name.as_bytes().get(1) == Some(&b':') {
        return Err(unsafe_entry());
    }
    let mut parts = Vec::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return Err(unsafe_entry()),
            _ => {
                paths::validate_component(part).map_err(|msg| {
                    Error::InvalidData(format!("The archive has a file name drives can't store: {msg}"))
                })?;
                parts.push(part);
            }
        }
    }
    Ok(parts.join("/"))
}

struct ZipEntry {
    index: usize,
    rel: String,
    size: u64,
    dir: bool,
}

/// Lists the entries of a zip after validating every name and size, without
/// decompressing anything.
fn scan_zip(
    archive: &mut zip::ZipArchive<BufReader<File>>,
    limit: Option<u64>,
    fs_raw: &str,
) -> Result<(Vec<ZipEntry>, u64)> {
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        let f = archive
            .by_index_raw(index)
            .map_err(|e| Error::InvalidData(format!("The archive is damaged: {e}")))?;
        let name = f
            .name()
            .map_err(|e| Error::InvalidData(format!("The archive is damaged: {e}")))?
            .into_owned();
        if f.is_symlink() {
            return Err(Error::UnsafeArchive { entry: name });
        }
        let rel = safe_entry_path(&name)?;
        if rel.is_empty() {
            continue;
        }
        let dir = f.is_dir();
        let size = f.size();
        if !dir {
            if let Some(limit) = limit
                && size > limit
            {
                return Err(Error::FileTooLarge {
                    name: rel,
                    size,
                    filesystem: fs_raw.to_string(),
                });
            }
            // FAT32 and exFAT ignore case: two names that differ only in case
            // would overwrite each other.
            if !seen.insert(rel.to_lowercase()) {
                return Err(Error::UnsafeArchive { entry: name });
            }
            total = total.saturating_add(size);
        }
        entries.push(ZipEntry { index, rel, size, dir });
    }
    Ok((entries, total))
}

/// A validated zip, ready to unpack.
struct ExtractScan {
    archive: zip::ZipArchive<BufReader<File>>,
    entries: Vec<ZipEntry>,
    total: u64,
}

/// Opens and checks a zip against the drive: every name is safe, nothing
/// lands in a reserved folder, the data file comes from exactly one place
/// (the zip or its own asset), and no file this app didn't write is
/// overwritten without consent. Reads only the zip's directory and the
/// drive's file names; writes nothing.
fn scan_extract(ctx: &StageContext, zip_path: &Path, rel_dir: &str) -> Result<ExtractScan> {
    let id = &ctx.id;
    let file = File::open(zip_path).map_err(io_ctx("opening the cached zip"))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| Error::InvalidData(format!("{id} is not a readable zip: {e}")))?;
    let (entries, total) = scan_zip(&mut archive, ctx.limit, &ctx.fs_raw)?;

    let mut untracked = Vec::new();
    let mut has_data = false;
    for entry in &entries {
        let full = paths::rel_join(rel_dir, &entry.rel);
        paths::check_not_reserved(&full, &ctx.reserved).map_err(|_| Error::UnsafeArchive {
            entry: entry.rel.clone(),
        })?;
        let dest = paths::join(&ctx.root, &full);
        if entry.dir {
            if dest.is_file() {
                return Err(Error::Conflict {
                    path: format!("{full} (a file is where the package needs a folder)"),
                });
            }
            continue;
        }
        if ctx.data_rel.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&full)) {
            if !ctx.data_in_zip {
                return Err(Error::InvalidData(format!(
                    "{id}'s zip contains {full}, which its own data file also writes. Ask its publisher to fix the release."
                )));
            }
            has_data = true;
        }
        if dest.is_dir() {
            return Err(Error::Conflict {
                path: format!("{full} (a folder is where the package needs a file)"),
            });
        }
        if dest.exists() && !ctx.replace_untracked && !ctx.replaceable.contains(&full.to_ascii_lowercase()) {
            untracked.push(full);
        }
    }
    if ctx.data_in_zip && !has_data {
        let data = ctx.data_rel.as_deref().unwrap_or_default();
        return Err(Error::InvalidData(format!(
            "{id}'s zip has no {data}, and the release doesn't publish one either. Ask its publisher to fix the release."
        )));
    }
    if !untracked.is_empty() {
        let more = untracked.len().saturating_sub(5);
        untracked.truncate(5);
        let mut path = untracked.join(", ");
        if more > 0 {
            path.push_str(&format!(" and {more} more"));
        }
        return Err(Error::Conflict { path });
    }
    Ok(ExtractScan {
        archive,
        entries,
        total,
    })
}

/// Unpacks a zip into a staging folder on the drive.
fn stage_extract(ctx: &StageContext, zip_path: &Path, rel_dir: String) -> Result<Staged> {
    let id = &ctx.id;
    let ExtractScan {
        mut archive,
        entries,
        total,
    } = scan_extract(ctx, zip_path, &rel_dir)?;

    let free = fsutil::free_space(&ctx.root).map_err(io_ctx("checking free space"))?;
    if with_margin(total) > free {
        return Err(Error::NotEnoughSpace {
            needed: with_margin(total),
            available: free,
        });
    }

    let staging = manifest::staging_dir(&ctx.root).join(format!("{id}-{}", fsutil::nonce()));
    let fail = |e: Error| {
        let _ = fs::remove_dir_all(&staging);
        e
    };
    fs::create_dir_all(&staging).map_err(|e| fail(io_error("creating a staging folder on the drive", e)))?;

    ctx.reporter.stage(Stage::Extract, 0, total);
    let mut done = 0u64;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in &entries {
        let dest = paths::join(&staging, &entry.rel);
        if entry.dir {
            fs::create_dir_all(&dest).map_err(|e| fail(io_error(format!("creating {}", entry.rel), e)))?;
            dirs.push(entry.rel.clone());
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| fail(io_error(format!("creating the folder for {}", entry.rel), e)))?;
        }
        let mut zf = archive
            .by_index(entry.index)
            .map_err(|e| fail(Error::InvalidData(format!("The archive is damaged: {e}"))))?;
        let (sha, n) = fsutil::write_hashed(
            &mut zf,
            &dest,
            |delta| {
                done += delta;
                ctx.reporter.bytes(Stage::Extract, done, total);
            },
            &ctx.cancel,
        )
        .map_err(fail)?;
        if n != entry.size {
            return Err(fail(Error::InvalidData(format!(
                "The archive is damaged: {} unpacked to {n} bytes, expected {}",
                entry.rel, entry.size
            ))));
        }
        files.push((entry.rel.clone(), sha, n));
    }
    Ok(Staged::Dir {
        staging,
        rel_dir,
        files,
        dirs,
        committed: Cell::new(0),
    })
}

/// Moves one staged item into place. An unpacked zip is merged file by file,
/// so it can share a folder (even the drive root) with the package's data
/// file.
fn commit_one(ctx: &StageContext, staged: &Staged) -> Result<()> {
    match staged {
        Staged::File { part, dest, rel, .. } => {
            fs::rename(part, dest).map_err(io_ctx(format!("putting {rel} in place")))?;
            if let Some(parent) = dest.parent() {
                fsutil::sync_dir(parent);
            }
            Ok(())
        }
        Staged::Dir {
            staging,
            rel_dir,
            files,
            dirs,
            committed,
        } => {
            let mut touched = HashSet::new();
            for (rel, _, _) in files {
                let full = paths::rel_join(rel_dir, rel);
                let dest = paths::join(&ctx.root, &full);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(io_ctx(format!("creating the folder for {full}")))?;
                    touched.insert(parent.to_path_buf());
                }
                fs::rename(paths::join(staging, rel), &dest).map_err(io_ctx(format!("putting {full} in place")))?;
                committed.set(committed.get() + 1);
            }
            for dir in dirs {
                let full = paths::join(&ctx.root, &paths::rel_join(rel_dir, dir));
                fs::create_dir_all(&full).map_err(io_ctx(format!("creating {dir}")))?;
            }
            for dir in touched {
                fsutil::sync_dir(&dir);
            }
            let _ = fs::remove_dir_all(staging);
            Ok(())
        }
    }
}

/// Puts everything in place, then reads it all back from the device. Returns
/// the drive-relative files and folders written.
fn commit_and_verify(
    ctx: &StageContext,
    staged: &[Staged],
) -> std::result::Result<(Vec<String>, Vec<String>), Failure> {
    for (i, item) in staged.iter().enumerate() {
        if let Err(error) = commit_one(ctx, item) {
            let committed = i > 0 || item.any_committed();
            staged[..=i].iter().for_each(|s| s.discard_committed(ctx));
            staged[i + 1..].iter().for_each(Staged::discard_staging);
            return Err(Failure { error, committed });
        }
    }
    let _ = fs::remove_dir(manifest::staging_dir(&ctx.root));

    // Read-back is mandatory: cheap sticks fail silently.
    let total: u64 = staged.iter().map(Staged::bytes).sum();
    ctx.reporter.clear_scope();
    ctx.reporter.stage(Stage::Readback, 0, total);
    let mut checked = 0u64;
    let mut verify = |rel: &str, sha: &str, size: u64| -> Result<()> {
        ctx.reporter.set_scope(checked, total);
        let (got, n) = fsutil::hash_file(
            &paths::join(&ctx.root, rel),
            true,
            |b| ctx.reporter.bytes(Stage::Readback, b, size),
            &ctx.cancel,
        )?;
        checked += size;
        if got == sha && n == size {
            Ok(())
        } else {
            Err(Error::DriveFaulty { path: rel.to_string() })
        }
    };
    let mut written = Vec::new();
    let mut dirs = Vec::new();
    let mut result = Ok(());
    'outer: for item in staged {
        match item {
            Staged::File { rel, sha256, size, .. } => {
                result = verify(rel, sha256, *size);
                if result.is_err() {
                    break 'outer;
                }
                written.push(rel.clone());
            }
            Staged::Dir {
                rel_dir,
                files,
                dirs: zip_dirs,
                ..
            } => {
                for (rel, sha, size) in files {
                    let full = paths::rel_join(rel_dir, rel);
                    result = verify(&full, sha, *size);
                    if result.is_err() {
                        break 'outer;
                    }
                    written.push(full);
                }
                if !rel_dir.is_empty() {
                    dirs.push(rel_dir.clone());
                }
                dirs.extend(zip_dirs.iter().map(|d| paths::rel_join(rel_dir, d)));
            }
        }
    }
    ctx.reporter.clear_scope();
    if let Err(error) = result {
        staged.iter().for_each(|s| s.discard_committed(ctx));
        return Err(Failure { error, committed: true });
    }
    dirs.dedup();
    Ok((written, dirs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PackageFormat, ProfileRef};
    use crate::profile::tests::{nemesis, pkg};

    fn index(packages: Vec<Package>) -> Index {
        Index {
            schema_version: 1,
            generated_at: String::new(),
            packages,
            profiles: vec![nemesis()],
        }
    }

    fn empty_manifest() -> DriveManifest {
        DriveManifest {
            schema_version: 1,
            profile: ProfileRef {
                id: "nemesis".into(),
                version: 2,
            },
            installed: vec![],
            written_by: String::new(),
            updated_at: None,
        }
    }

    fn sample_index() -> Index {
        let core = pkg("nemesis-core", PackageFormat::Tar, "core");
        let mut a = pkg("tool-a", PackageFormat::Zip, "packages");
        a.requires = vec!["nemesis-core".into()];
        let b = pkg("tool.b", PackageFormat::Zip, "packages");
        index(vec![b, a, core])
    }

    fn record(id: &str, paths: &[&str], data: Option<&str>) -> InstalledPackage {
        InstalledPackage {
            id: id.into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            slot: "packages".into(),
            format: PackageFormat::Zip,
            extracted: false,
            paths: paths.iter().map(|s| s.to_string()).collect(),
            dirs: vec![],
            data: data.map(str::to_string),
            size: 15,
            unpacked_into: None,
            installed_at: String::new(),
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_tool_writes_its_zip_and_data_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &ids(&["tool-a"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.steps.len(), 1, "requires is informational; nothing else is added");
        assert_eq!(p.steps[0].target, "packages/tool-a.zip");
        assert_eq!(p.steps[0].data_target.as_deref(), Some("NEMESIS.data"));
        assert_eq!(p.steps[0].size, 15);
        assert!(p.removes.is_empty());

        let core = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &ids(&["nemesis-core"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(core.steps[0].target, "nemesis-core.tar");
        assert_eq!(core.steps[0].data_target, None);
    }

    #[test]
    fn a_zip_with_its_own_data_file_is_unpacked_whatever_the_user_picked() {
        let dir = tempfile::tempdir().unwrap();
        let mut tool = pkg("tool-a", PackageFormat::Zip, "packages");
        tool.data = None;
        let options = InstallOptions {
            extract: BTreeMap::from([("tool-a".into(), false)]),
            ..Default::default()
        };
        let p = plan(
            dir.path(),
            &index(vec![tool]),
            &nemesis(),
            &empty_manifest(),
            &ids(&["tool-a"]),
            &options,
        )
        .unwrap();
        let step = &p.steps[0];
        assert!(step.extract && step.data_in_zip);
        assert_eq!(step.target, "");
        assert_eq!(step.data_target.as_deref(), Some("NEMESIS.data"));
        assert_eq!(step.size, 10, "no separate data asset to count");
    }

    #[test]
    fn only_one_package_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let err = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &ids(&["tool-a", "tool.b"]),
            &InstallOptions::default(),
        );
        assert!(matches!(err, Err(Error::Profile(m)) if m.contains("1 package at a time")));
    }

    #[test]
    fn a_different_package_is_swapped_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = empty_manifest();
        manifest.installed.push(record(
            "tool-a",
            &["NEMESIS.data", "packages/tool-a.zip"],
            Some("NEMESIS.data"),
        ));
        fs::create_dir_all(dir.path().join("packages")).unwrap();
        fs::write(dir.path().join("NEMESIS.data"), b"a's data").unwrap();
        let p = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &manifest,
            &ids(&["tool.b"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.removes.len(), 1);
        assert_eq!(p.removes[0].id, "tool-a");
        assert_eq!(p.steps[0].target, "packages/tool.b.zip");
        assert!(
            p.untracked.is_empty(),
            "the old package's NEMESIS.data is being replaced, not untracked"
        );
    }

    #[test]
    fn without_a_package_limit_shared_files_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let mut profile = nemesis();
        profile.max_packages = None;
        let mut idx = sample_index();
        idx.profiles = vec![profile.clone()];
        let mut manifest = empty_manifest();
        manifest.installed.push(record(
            "tool-a",
            &["NEMESIS.data", "packages/tool-a.zip"],
            Some("NEMESIS.data"),
        ));
        let err = plan(
            dir.path(),
            &idx,
            &profile,
            &manifest,
            &ids(&["tool.b"]),
            &InstallOptions::default(),
        );
        assert!(matches!(err, Err(Error::Conflict { path }) if path.contains("tool-a")));
    }

    #[test]
    fn up_to_date_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("packages")).unwrap();
        fs::write(dir.path().join("packages/tool-a.zip"), b"x").unwrap();
        fs::write(dir.path().join("NEMESIS.data"), b"x").unwrap();
        let mut manifest = empty_manifest();
        manifest.installed.push(record(
            "tool-a",
            &["NEMESIS.data", "packages/tool-a.zip"],
            Some("NEMESIS.data"),
        ));
        let idx = sample_index();
        let p = plan(
            dir.path(),
            &idx,
            &nemesis(),
            &manifest,
            &ids(&["tool-a"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert!(p.steps.is_empty());
        assert!(p.removes.is_empty());
        assert_eq!(p.up_to_date, ["tool-a"]);

        let mut newer = idx.clone();
        newer.packages.iter_mut().find(|p| p.id == "tool-a").unwrap().version = "1.1.0".into();
        let p = plan(
            dir.path(),
            &newer,
            &nemesis(),
            &manifest,
            &ids(&["tool-a"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].action, PlanAction::Update);
        assert_eq!(p.steps[0].from_version.as_deref(), Some("1.0.0"));
        assert!(p.untracked.is_empty());
    }

    #[test]
    fn untracked_files_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("NEMESIS.data"), b"copied by hand").unwrap();
        let p = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &ids(&["tool-a"]),
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.untracked, ["NEMESIS.data"]);
        assert!(p.steps[0].untracked);
    }

    #[test]
    fn zip_slip_is_rejected() {
        for bad in [
            "../evil.txt",
            "a/../../evil",
            "/etc/passwd",
            "C:\\evil",
            "..\\evil",
            "a\\..\\..\\b",
        ] {
            assert!(
                matches!(safe_entry_path(bad), Err(Error::UnsafeArchive { .. })),
                "{bad} should be unsafe"
            );
        }
        assert_eq!(safe_entry_path("dir/").unwrap(), "dir");
        assert_eq!(safe_entry_path("./a/b.txt").unwrap(), "a/b.txt");
        assert_eq!(safe_entry_path("a\\b.txt").unwrap(), "a/b.txt");
        assert!(matches!(safe_entry_path("a/con.txt"), Err(Error::InvalidData(_))));
    }
}
