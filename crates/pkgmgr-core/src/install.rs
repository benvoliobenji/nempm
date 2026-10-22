//! The install pipeline.
//!
//! Per package: pre-flight checks → verified copy in the cache (download if
//! needed) → copy to `<dest>.pkgmgr-part` on the drive, fsync, rename →
//! read-back from the device and compare SHA-256 → (zip, optional) unpack in a
//! staging folder on the drive and rename into place → record the files in
//! the drive manifest, written atomically.
//!
//! The drive manifest is only updated after a package's files are complete and
//! verified, so pulling the stick mid-copy leaves a valid manifest and, at
//! worst, a `.pkgmgr-part` file that Check drive offers to remove.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::{Error, Result, fsutil, manifest, paths};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct InstallOptions {
    /// Per-package choice to unpack a zip, where the slot allows it.
    #[ts(type = "Record<string, boolean>")]
    pub extract: BTreeMap<String, bool>,
    /// Replace files at the target path that weren't installed by this app.
    pub replace_untracked: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PlanReason {
    Requested,
    Dependency,
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
    pub reason: PlanReason,
    pub slot: String,
    pub extract: bool,
    #[ts(type = "number")]
    pub size: u64,
    /// Drive-relative file or folder this step writes.
    pub target: String,
    /// The target exists but no installed package owns it.
    pub untracked: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstallPlan {
    /// In install order: dependencies first, the core package before the rest.
    pub steps: Vec<PlanStep>,
    /// Requested packages that are already installed at the index version.
    pub up_to_date: Vec<String>,
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// Targets that would replace files this app didn't install.
    pub untracked: Vec<String>,
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        _ => a.cmp(b),
    }
}

/// Resolves dependencies, orders the work (deps first, core first), and checks
/// slot rules and ownership of every target path.
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
    let requested: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut roots = ids
        .iter()
        .map(|id| index.package(id).ok_or_else(|| Error::UnknownPackage(id.clone())))
        .collect::<Result<Vec<_>>>()?;
    roots.sort_by_key(|p| !p.core);

    let mut state: HashMap<&str, bool> = HashMap::new();
    let mut order: Vec<&Package> = Vec::new();
    for pkg in roots {
        visit(pkg, index, &mut state, &mut order, &mut Vec::new())?;
    }

    let mut steps = Vec::new();
    let mut up_to_date = Vec::new();
    let mut untracked = Vec::new();
    for pkg in order {
        let slot = profile::slot_for(profile, pkg)?;
        let place = profile::placement(profile, pkg)?;
        let extract = profile::effective_extract(slot, pkg, options.extract.get(&pkg.id).copied());
        let is_requested = requested.contains(pkg.id.as_str());
        let installed = manifest.get(&pkg.id);

        let action = match installed {
            None => PlanAction::Install,
            // A dependency that's already there (any version) is left alone.
            Some(_) if !is_requested => continue,
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
        let is_untracked = match manifest.owner_of(&target) {
            Some(owner) if owner.id == pkg.id => false,
            Some(owner) => {
                return Err(Error::Conflict {
                    path: format!("{target} (installed by {})", owner.id),
                });
            }
            None => paths::join(root, &target).exists(),
        };
        if is_untracked {
            untracked.push(target.clone());
        }

        steps.push(PlanStep {
            id: pkg.id.clone(),
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            from_version: installed.map(|r| r.version.clone()),
            action,
            reason: if is_requested {
                PlanReason::Requested
            } else {
                PlanReason::Dependency
            },
            slot: pkg.slot.clone(),
            extract,
            size: pkg.size,
            target,
            untracked: is_untracked,
        });
    }

    for (name, slot) in &profile.slots {
        let Some(max) = slot.max else { continue };
        let mut ids: HashSet<&str> = manifest
            .installed
            .iter()
            .filter(|r| &r.slot == name)
            .map(|r| r.id.as_str())
            .collect();
        ids.extend(steps.iter().filter(|s| &s.slot == name).map(|s| s.id.as_str()));
        if ids.len() > max as usize {
            let mut list: Vec<_> = ids.into_iter().collect();
            list.sort();
            return Err(Error::Profile(format!(
                "{} allows {max} package(s) in \"{name}\", but this would make {}: {}. Uninstall one first.",
                profile.display_name(),
                list.len(),
                list.join(", ")
            )));
        }
    }

    let total_bytes = steps.iter().map(|s| s.size).sum();
    Ok(InstallPlan {
        steps,
        up_to_date,
        total_bytes,
        untracked,
    })
}

fn visit<'a>(
    pkg: &'a Package,
    index: &'a Index,
    state: &mut HashMap<&'a str, bool>,
    order: &mut Vec<&'a Package>,
    trail: &mut Vec<&'a str>,
) -> Result<()> {
    match state.get(pkg.id.as_str()) {
        Some(true) => return Ok(()),
        Some(false) => {
            trail.push(&pkg.id);
            return Err(Error::Dependency(format!("Dependency cycle: {}", trail.join(" → "))));
        }
        None => {}
    }
    state.insert(&pkg.id, false);
    trail.push(&pkg.id);
    let mut deps = pkg
        .deps
        .iter()
        .map(|d| {
            index.package(d).ok_or_else(|| {
                Error::Dependency(format!("{} needs \"{d}\", which isn't in the package list", pkg.name))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    deps.sort_by_key(|p| !p.core);
    for dep in deps {
        visit(dep, index, state, order, trail)?;
    }
    trail.pop();
    state.insert(&pkg.id, true);
    order.push(pkg);
    Ok(())
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
    if !plan.untracked.is_empty() && !req.options.replace_untracked {
        return Err(Error::Conflict {
            path: plan.untracked.join(", "),
        });
    }
    if let Some(limit) = max_file_size(profile, fs_name)
        && let Some(step) = plan.steps.iter().find(|s| !s.extract && s.size > limit)
    {
        return Err(Error::FileTooLarge {
            name: step.name.clone(),
            size: step.size,
            filesystem: req.drive.filesystem_raw.clone(),
        });
    }
    let free = fsutil::free_space(req.root).unwrap_or(req.drive.free_bytes);
    let needed = with_margin(plan.total_bytes);
    // Files being replaced free their space once the new copy is in place.
    let replaced: u64 = plan
        .steps
        .iter()
        .filter_map(|s| manifest.get(&s.id))
        .map(|r| r.size)
        .sum();
    if needed > free.saturating_add(replaced) {
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

/// Installs `req.ids` and their dependencies, emitting progress for each.
/// Stops at the first failure; packages finished before it stay installed and
/// recorded.
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

    let mut installed = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        let pkg = req.index.package(&step.id).expect("planned from the index");
        let result = match dl.cancel.check() {
            Ok(()) => install_one(dl, &req, pkg, step, &mut manifest, &reporters[i]).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(record) => {
                reporters[i].done(step.size, None);
                installed.push(record);
            }
            Err(e) => {
                reporters[i].error(e.to_string());
                let why = if matches!(e, Error::Cancelled) {
                    "Cancelled".to_string()
                } else {
                    format!("Not installed because {} failed", step.name)
                };
                for r in &reporters[i + 1..] {
                    r.error(why.clone());
                }
                return Err(e);
            }
        }
    }
    Ok(InstallOutcome {
        installed,
        up_to_date: plan.up_to_date,
    })
}

/// A failure, and whether it happened after the new files replaced the old.
struct Failure {
    error: Error,
    committed: bool,
}

impl Failure {
    fn before(error: Error) -> Self {
        Self {
            error,
            committed: false,
        }
    }

    fn after(error: Error) -> Self {
        Self { error, committed: true }
    }
}

async fn install_one(
    dl: &Downloader,
    req: &InstallRequest<'_>,
    pkg: &Package,
    step: &PlanStep,
    manifest: &mut DriveManifest,
    reporter: &Arc<Reporter>,
) -> Result<InstalledPackage> {
    let root = req.root.to_path_buf();
    let place = profile::placement(req.profile, pkg)?;
    let previous = manifest.get(&pkg.id).cloned();

    if !step.extract {
        let free = fsutil::free_space(&root).map_err(io_ctx("checking free space on the drive"))?;
        let needed = with_margin(pkg.size);
        if needed > free {
            return Err(Error::NotEnoughSpace {
                needed,
                available: free,
            });
        }
    }

    let cached = dl.ensure(pkg, reporter).await?;

    let reporter2 = reporter.clone();
    let cancel = dl.cancel.clone();
    let limit = max_file_size(req.profile, &req.drive.filesystem);
    let fs_raw = req.drive.filesystem_raw.clone();
    let (sha, size, id) = (pkg.sha256.clone(), pkg.size, pkg.id.clone());
    let root2 = root.clone();
    let result = if step.extract {
        let dir = place.extract_dir.clone().expect("extract implies an extract dir");
        let dir2 = dir.clone();
        tokio::task::spawn_blocking(move || {
            extract_zip(&cached, &root2, &dir2, &id, limit, &fs_raw, &reporter2, &cancel)
                .map(|files| (files, vec![dir2]))
        })
        .await
    } else {
        let file = place.file.clone();
        tokio::task::spawn_blocking(move || {
            copy_to_drive(&cached, &root2, &file, &sha, size, &reporter2, &cancel).map(|()| (vec![file], vec![]))
        })
        .await
    }
    .map_err(|e| Error::InvalidData(format!("Install task failed: {e}")))?;

    let (new_paths, new_dirs) = match result {
        Ok(v) => v,
        Err(Failure { error, committed }) => {
            if committed && let Some(prev) = &previous {
                // The old version's files were replaced by ones that failed
                // verification: forget it so the drive stays consistent.
                let _ = crate::uninstall::remove_files(&root, prev);
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
        deps: pkg.deps.clone(),
        size: pkg.size,
        installed_at: manifest::now(),
    };
    manifest.upsert(record.clone());
    manifest::save(&root, manifest)?;

    // Only now remove what the previous version had that this one doesn't.
    if let Some(prev) = previous {
        let keep: HashSet<String> = record.paths.iter().map(|p| p.to_ascii_lowercase()).collect();
        let leftover = InstalledPackage {
            paths: prev
                .paths
                .iter()
                .filter(|p| !keep.contains(&p.to_ascii_lowercase()))
                .cloned()
                .collect(),
            dirs: prev.dirs.iter().filter(|d| !record.dirs.contains(d)).cloned().collect(),
            ..prev
        };
        crate::uninstall::remove_files(&root, &leftover)?;
    }
    Ok(record)
}

/// Copies one verified cached file onto the drive and reads it back.
fn copy_to_drive(
    cached: &Path,
    root: &Path,
    rel: &str,
    sha256: &str,
    size: u64,
    reporter: &Reporter,
    cancel: &CancelToken,
) -> std::result::Result<(), Failure> {
    let dest = paths::join(root, rel);
    let parent = dest.parent().unwrap_or(root).to_path_buf();
    fs::create_dir_all(&parent).map_err(|e| Failure::before(io_error(format!("creating the folder for {rel}"), e)))?;
    let part = fsutil::part_path(&dest);
    let _ = fsutil::remove_file_if_exists(&part);

    reporter.stage(Stage::Copy, 0, size);
    let copied = fsutil::copy_hashed(cached, &part, |n| reporter.bytes(Stage::Copy, n, size), cancel);
    let (written_sha, written) = match copied {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(Failure::before(e));
        }
    };
    if written_sha != sha256 || written != size {
        let _ = fs::remove_file(&part);
        return Err(Failure::before(Error::InvalidData(
            "The cached download changed while it was being copied. Try again.".into(),
        )));
    }
    if let Err(e) = fs::rename(&part, &dest) {
        let _ = fs::remove_file(&part);
        return Err(Failure::before(io_error(format!("putting {rel} in place"), e)));
    }
    fsutil::sync_dir(&parent);

    // Read-back is mandatory: cheap sticks fail silently.
    reporter.stage(Stage::Readback, 0, size);
    match fsutil::hash_file(&dest, true, |n| reporter.bytes(Stage::Readback, n, size), cancel) {
        Ok((sha, n)) if sha == sha256 && n == size => Ok(()),
        Ok(_) => {
            let _ = fs::remove_file(&dest);
            Err(Failure::after(Error::DriveFaulty { path: rel.to_string() }))
        }
        Err(e) => {
            let _ = fs::remove_file(&dest);
            Err(Failure::after(e))
        }
    }
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

/// Unpacks a zip into a staging folder on the drive, renames it into place,
/// then reads every file back. Returns the drive-relative files written.
#[allow(clippy::too_many_arguments)]
fn extract_zip(
    cached: &Path,
    root: &Path,
    rel_dir: &str,
    id: &str,
    limit: Option<u64>,
    fs_raw: &str,
    reporter: &Reporter,
    cancel: &CancelToken,
) -> std::result::Result<Vec<String>, Failure> {
    let file = File::open(cached).map_err(|e| Failure::before(io_error("opening the cached zip", e)))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| Failure::before(Error::InvalidData(format!("{id} is not a readable zip: {e}"))))?;
    let (entries, total) = scan_zip(&mut archive, limit, fs_raw).map_err(Failure::before)?;

    let free = fsutil::free_space(root).map_err(|e| Failure::before(io_error("checking free space", e)))?;
    if with_margin(total) > free {
        return Err(Failure::before(Error::NotEnoughSpace {
            needed: with_margin(total),
            available: free,
        }));
    }

    let staging_root = manifest::staging_dir(root);
    let staging = staging_root.join(format!("{id}-{}", fsutil::nonce()));
    let fail = |e: Error| {
        let _ = fs::remove_dir_all(&staging);
        Failure::before(e)
    };
    fs::create_dir_all(&staging).map_err(|e| fail(io_error("creating a staging folder on the drive", e)))?;

    reporter.stage(Stage::Extract, 0, total);
    let mut done = 0u64;
    let mut written: Vec<(String, String, u64)> = Vec::new();
    for entry in &entries {
        let dest = paths::join(&staging, &entry.rel);
        if entry.dir {
            fs::create_dir_all(&dest).map_err(|e| fail(io_error(format!("creating {}", entry.rel), e)))?;
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
                reporter.bytes(Stage::Extract, done, total);
            },
            cancel,
        )
        .map_err(fail)?;
        if n != entry.size {
            return Err(fail(Error::InvalidData(format!(
                "The archive is damaged: {} unpacked to {n} bytes, expected {}",
                entry.rel, entry.size
            ))));
        }
        written.push((entry.rel.clone(), sha, n));
    }

    // Swap the staging folder into place, keeping the old one until the
    // rename has succeeded.
    let target = paths::join(root, rel_dir);
    let mut aside: Option<PathBuf> = None;
    if target.exists() {
        let old = staging_root.join(format!("{id}-old-{}", fsutil::nonce()));
        fs::rename(&target, &old).map_err(|e| fail(io_error(format!("moving the old {rel_dir} aside"), e)))?;
        aside = Some(old);
    }
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::rename(&staging, &target) {
        if let Some(old) = &aside {
            let _ = fs::rename(old, &target);
        }
        return Err(fail(io_error(format!("putting {rel_dir} in place"), e)));
    }
    if let Some(parent) = target.parent() {
        fsutil::sync_dir(parent);
    }
    if let Some(old) = aside {
        let _ = fs::remove_dir_all(old);
    }

    reporter.stage(Stage::Readback, 0, total);
    let mut checked = 0u64;
    for (rel, sha, n) in &written {
        let path = paths::join(&target, rel);
        let base = checked;
        let readback = fsutil::hash_file(
            &path,
            true,
            |b| reporter.bytes(Stage::Readback, base + b, total),
            cancel,
        );
        let full_rel = paths::rel_join(rel_dir, rel);
        match readback {
            Ok((rb, rn)) if rb == *sha && rn == *n => checked += n,
            Ok(_) => {
                let _ = fs::remove_dir_all(&target);
                return Err(Failure::after(Error::DriveFaulty { path: full_rel }));
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&target);
                return Err(Failure::after(e));
            }
        }
    }
    let _ = fs::remove_dir(&staging_root);

    Ok(written
        .into_iter()
        .map(|(rel, _, _)| paths::rel_join(rel_dir, &rel))
        .collect())
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
                version: 1,
            },
            installed: vec![],
            written_by: String::new(),
            updated_at: None,
        }
    }

    fn sample_index() -> Index {
        let core = pkg("nemesis-core", PackageFormat::Tar, "core");
        let mut a = pkg("tool-a", PackageFormat::Zip, "packages");
        a.deps = vec!["nemesis-core".into()];
        let mut b = pkg("tool-b", PackageFormat::Zip, "packages");
        b.deps = vec!["tool-a".into()];
        index(vec![b, a, core])
    }

    #[test]
    fn dependencies_come_first_and_core_leads() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &["tool-b".into()],
            &InstallOptions::default(),
        )
        .unwrap();
        let ids: Vec<_> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["nemesis-core", "tool-a", "tool-b"]);
        assert_eq!(plan.steps[0].target, "NEMESIS.data");
        assert_eq!(plan.steps[0].reason, PlanReason::Dependency);
        assert_eq!(plan.steps[2].reason, PlanReason::Requested);
        assert_eq!(plan.total_bytes, 30);
    }

    #[test]
    fn cycles_and_missing_deps_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = sample_index();
        idx.packages.iter_mut().find(|p| p.id == "nemesis-core").unwrap().deps = vec!["tool-b".into()];
        let err = plan(
            dir.path(),
            &idx,
            &nemesis(),
            &empty_manifest(),
            &["tool-b".into()],
            &InstallOptions::default(),
        );
        assert!(matches!(err, Err(Error::Dependency(msg)) if msg.contains("cycle")));

        let mut idx = sample_index();
        idx.packages.iter_mut().find(|p| p.id == "tool-a").unwrap().deps = vec!["ghost".into()];
        let err = plan(
            dir.path(),
            &idx,
            &nemesis(),
            &empty_manifest(),
            &["tool-a".into()],
            &InstallOptions::default(),
        );
        assert!(matches!(err, Err(Error::Dependency(msg)) if msg.contains("ghost")));
    }

    #[test]
    fn up_to_date_and_updates() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("packages")).unwrap();
        fs::write(dir.path().join("packages/tool-a.zip"), b"x").unwrap();
        let mut manifest = empty_manifest();
        manifest.installed.push(InstalledPackage {
            id: "tool-a".into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            slot: "packages".into(),
            format: PackageFormat::Zip,
            extracted: false,
            paths: vec!["packages/tool-a.zip".into()],
            dirs: vec![],
            deps: vec![],
            size: 10,
            installed_at: String::new(),
        });
        manifest.installed.push(InstalledPackage {
            id: "nemesis-core".into(),
            paths: vec!["NEMESIS.data".into()],
            slot: "core".into(),
            format: PackageFormat::Tar,
            ..manifest.installed[0].clone()
        });
        let idx = sample_index();
        let p = plan(
            dir.path(),
            &idx,
            &nemesis(),
            &manifest,
            &["tool-a".into()],
            &InstallOptions::default(),
        )
        .unwrap();
        assert!(p.steps.is_empty());
        assert_eq!(p.up_to_date, ["tool-a"]);

        let mut newer = idx.clone();
        newer.packages.iter_mut().find(|p| p.id == "tool-a").unwrap().version = "1.1.0".into();
        let p = plan(
            dir.path(),
            &newer,
            &nemesis(),
            &manifest,
            &["tool-a".into()],
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].action, PlanAction::Update);
        assert_eq!(p.steps[0].from_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn untracked_files_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("NEMESIS.data"), b"hand copied").unwrap();
        let p = plan(
            dir.path(),
            &sample_index(),
            &nemesis(),
            &empty_manifest(),
            &["nemesis-core".into()],
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(p.untracked, ["NEMESIS.data"]);
        assert!(p.steps[0].untracked);
    }

    #[test]
    fn slot_max_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = sample_index();
        let mut second = pkg("other-core", PackageFormat::Tar, "core");
        second.id = "other-core".into();
        idx.packages.push(second);
        let err = plan(
            dir.path(),
            &idx,
            &nemesis(),
            &empty_manifest(),
            &["nemesis-core".into(), "other-core".into()],
            &InstallOptions::default(),
        );
        assert!(matches!(err, Err(Error::Profile(msg)) if msg.contains("allows 1")));
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
