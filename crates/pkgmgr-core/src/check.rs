//! Check drive, Initialize, the safe fixes, and reading what a drive holds.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::drives::DriveInfo;
use crate::error::io_ctx;
use crate::fsutil::{self, PART_SUFFIX};
use crate::manifest::{self, ManifestState};
use crate::model::{DriveManifest, Index, InstalledPackage, Profile, ProfileRef};
use crate::profile::{self, max_file_size};
use crate::{Error, Result, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum IssueCode {
    ReadOnly,
    FilesystemNotAllowed,
    FilesystemUnknown,
    NotInitialized,
    ManifestCorrupt,
    ProfileMismatch,
    ProfileOutdated,
    ProfileNewer,
    MissingDir,
    PathBlocked,
    PartialFiles,
    MissingFiles,
    UntrackedFile,
    TooLargeForFilesystem,
    LowSpace,
    VolumeLabel,
    UnknownPackage,
}

/// A fix the app can apply safely. Formatting is never one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FixAction {
    /// Create the required folders and an empty drive manifest.
    Initialize,
    /// Delete leftover `.pkgmgr-part` files and the staging folder.
    RemovePartials,
    /// Forget packages whose files are missing (removing what's left of them)
    /// so they can be installed again.
    ForgetMissing,
    /// Move installed files to where the current profile version wants them.
    MigrateProfile,
    /// Set the damaged manifest aside and start a new one.
    ResetManifest,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Issue {
    pub code: IssueCode,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fix: Option<FixAction>,
}

impl Issue {
    fn new(code: IssueCode, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            path: None,
            fix: None,
        }
    }

    fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn fix(mut self, fix: FixAction) -> Self {
        self.fix = Some(fix);
        self
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CheckReport {
    pub profile: ProfileRef,
    pub drive: DriveInfo,
    pub issues: Vec<Issue>,
    /// No errors: installs can go ahead.
    pub ready: bool,
}

/// Runs every profile check against the drive at `root`.
pub fn check_drive(root: &Path, drive: &DriveInfo, profile: &Profile, index: Option<&Index>) -> CheckReport {
    let mut issues = Vec::new();
    let name = profile.display_name();

    if drive.read_only {
        issues.push(Issue::new(
            IssueCode::ReadOnly,
            Severity::Error,
            "This drive is read-only. Check the lock switch on the stick, or try another drive.",
        ));
    }

    let fs_name = drive.filesystem.as_str();
    if fs_name == "unknown" {
        issues.push(Issue::new(
            IssueCode::FilesystemUnknown,
            Severity::Warning,
            format!(
                "Couldn't tell how this drive is formatted. {name} needs {}.",
                list(&profile.filesystems)
            ),
        ));
    } else if !profile.filesystems.iter().any(|f| f == fs_name) {
        issues.push(Issue::new(
            IssueCode::FilesystemNotAllowed,
            Severity::Error,
            format!(
                "This drive is formatted as {}, but {name} needs {}. Back up anything on it, \
                 format it as exFAT with your system's disk utility, then check again.",
                drive.filesystem_raw,
                list(&profile.filesystems)
            ),
        ));
    }

    let manifest = match manifest::read(root) {
        ManifestState::Missing => {
            issues.push(
                Issue::new(
                    IssueCode::NotInitialized,
                    Severity::Warning,
                    format!("This drive isn't set up for {name} yet."),
                )
                .fix(FixAction::Initialize),
            );
            None
        }
        ManifestState::Corrupt(msg) => {
            issues.push(
                Issue::new(IssueCode::ManifestCorrupt, Severity::Error, msg)
                    .path(format!("{}/{}", manifest::APP_DIR, manifest::MANIFEST_FILE))
                    .fix(FixAction::ResetManifest),
            );
            None
        }
        ManifestState::Present(m) => Some(m),
    };

    if let Some(m) = &manifest {
        if m.profile.id != profile.id {
            issues.push(Issue::new(
                IssueCode::ProfileMismatch,
                Severity::Error,
                format!(
                    "This drive was set up for \"{}\", not {name}. Pick that profile instead.",
                    m.profile.id
                ),
            ));
        } else if m.profile.version < profile.version {
            issues.push(
                Issue::new(
                    IssueCode::ProfileOutdated,
                    Severity::Warning,
                    format!(
                        "The {name} layout changed (version {} → {}). Migrate to move installed files.",
                        m.profile.version, profile.version
                    ),
                )
                .fix(FixAction::MigrateProfile),
            );
        } else if m.profile.version > profile.version {
            issues.push(Issue::new(
                IssueCode::ProfileNewer,
                Severity::Warning,
                format!(
                    "This drive uses a newer {name} layout (version {}) than this app knows (version {}). Refresh the package list.",
                    m.profile.version, profile.version
                ),
            ));
        }
    }

    for dir in &profile.required_dirs {
        let path = paths::join(root, dir);
        if path.is_file() {
            issues.push(
                Issue::new(
                    IssueCode::PathBlocked,
                    Severity::Error,
                    format!("A file named \"{dir}\" is where {name} needs a folder. Rename or move it."),
                )
                .path(dir.clone()),
            );
        } else if !path.is_dir() {
            issues.push(
                Issue::new(
                    IssueCode::MissingDir,
                    Severity::Error,
                    format!("The \"{dir}\" folder is missing."),
                )
                .path(dir.clone())
                .fix(FixAction::Initialize),
            );
        }
    }

    let partials = find_partials(root, profile);
    if !partials.is_empty() {
        issues.push(
            Issue::new(
                IssueCode::PartialFiles,
                Severity::Warning,
                format!(
                    "{} unfinished file(s) from an interrupted install are taking up space.",
                    partials.len()
                ),
            )
            .fix(FixAction::RemovePartials),
        );
    }

    if let Some(m) = &manifest {
        for rec in &m.installed {
            let missing = rec.paths.iter().filter(|p| !paths::join(root, p).exists()).count();
            if missing > 0 {
                issues.push(
                    Issue::new(
                        IssueCode::MissingFiles,
                        Severity::Error,
                        format!(
                            "{} is recorded as installed but {missing} of its {} file(s) are missing.",
                            rec.id,
                            rec.paths.len()
                        ),
                    )
                    .path(rec.paths[0].clone())
                    .fix(FixAction::ForgetMissing),
                );
            }
            if index.is_some_and(|i| i.package(&rec.id).is_none()) {
                issues.push(Issue::new(
                    IssueCode::UnknownPackage,
                    Severity::Info,
                    format!("{} is installed but no longer in the package list.", rec.id),
                ));
            }
        }
    }

    if let Some(index) = index {
        let limit = max_file_size(profile, fs_name);
        for pkg in index.packages.iter().filter(|p| p.profiles.contains(&profile.id)) {
            let Ok(place) = profile::placement(profile, pkg) else {
                continue;
            };
            for target in [Some(&place.file), place.extract_dir.as_ref()].into_iter().flatten() {
                let owned = manifest.as_ref().is_some_and(|m| m.owner_of(target).is_some());
                if !owned && paths::join(root, target).exists() {
                    issues.push(
                        Issue::new(
                            IssueCode::UntrackedFile,
                            Severity::Warning,
                            format!(
                                "{target} is already on the drive but wasn't installed by this app. \
                                 Installing {} will ask before replacing it.",
                                pkg.name
                            ),
                        )
                        .path(target.clone()),
                    );
                }
            }
            if let Some(limit) = limit
                && pkg.size > limit
            {
                issues.push(Issue::new(
                    IssueCode::TooLargeForFilesystem,
                    Severity::Warning,
                    format!(
                        "{} ({}) is too large for {}. Format the drive as exFAT to install it.",
                        pkg.name,
                        human_bytes(pkg.size),
                        drive.filesystem_raw
                    ),
                ));
            }
        }
        let core_needed: u64 = index
            .packages
            .iter()
            .filter(|p| p.core && p.profiles.contains(&profile.id))
            .filter(|p| manifest.as_ref().is_none_or(|m| m.get(&p.id).is_none()))
            .map(|p| p.size)
            .sum();
        if core_needed > 0 && core_needed > drive.free_bytes {
            issues.push(Issue::new(
                IssueCode::LowSpace,
                Severity::Warning,
                format!(
                    "Only {} free; the core package needs {}.",
                    human_bytes(drive.free_bytes),
                    human_bytes(core_needed)
                ),
            ));
        }
    }

    if let Some(rule) = &profile.volume_label
        && rule.required
    {
        let ok = regex::Regex::new(&rule.pattern)
            .map(|re| drive.label.as_deref().is_some_and(|l| re.is_match(l)))
            .unwrap_or(true);
        if !ok {
            issues.push(Issue::new(
                IssueCode::VolumeLabel,
                Severity::Warning,
                format!(
                    "{name} expects the drive's name to match {}. Rename the drive.",
                    rule.pattern
                ),
            ));
        }
    }

    let ready = !issues.iter().any(|i| i.severity == Severity::Error);
    CheckReport {
        profile: profile.as_ref(),
        drive: drive.clone(),
        issues,
        ready,
    }
}

fn list(items: &[String]) -> String {
    let names: Vec<&str> = items
        .iter()
        .map(|s| match s.as_str() {
            "fat32" => "FAT32",
            "exfat" => "exFAT",
            "ntfs" => "NTFS",
            "apfs" => "APFS",
            "hfsplus" => "Mac OS Extended",
            other => other,
        })
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Leftovers from interrupted installs: `*.pkgmgr-part` files in the folders
/// the profile writes to, plus anything in `.pkgmgr/staging`.
pub fn find_partials(root: &Path, profile: &Profile) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<String> = vec![String::new()];
    dirs.extend(profile.required_dirs.iter().cloned());
    for slot in profile.slots.values() {
        // The static part of the slot path, before any placeholder.
        let fixed: Vec<&str> = slot.path.split('/').take_while(|s| !s.contains('{')).collect();
        dirs.push(fixed.join("/"));
    }
    dirs.sort();
    dirs.dedup();

    let mut found = Vec::new();
    for dir in dirs {
        collect_parts(&paths::join(root, &dir), 2, &mut found);
    }
    if let Ok(entries) = fs::read_dir(manifest::staging_dir(root)) {
        found.extend(entries.filter_map(|e| e.ok()).map(|e| e.path()));
    }
    let tmp = manifest::manifest_path(root).with_extension("json.tmp");
    if tmp.exists() {
        found.push(tmp);
    }
    found.sort();
    found.dedup();
    found
}

fn collect_parts(dir: &Path, depth: u32, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() && path.to_string_lossy().ends_with(PART_SUFFIX) {
            out.push(path);
        } else if kind.is_dir() && depth > 0 && entry.file_name() != manifest::APP_DIR {
            collect_parts(&path, depth - 1, out);
        }
    }
}

/// Creates the required folders and, if there is none, an empty manifest.
pub fn init_drive(root: &Path, profile: &Profile) -> Result<DriveManifest> {
    match manifest::read(root) {
        ManifestState::Present(m) if m.profile.id != profile.id => {
            return Err(Error::Profile(format!(
                "This drive is already set up for \"{}\". It won't be changed to {}.",
                m.profile.id,
                profile.display_name()
            )));
        }
        ManifestState::Corrupt(msg) => return Err(Error::InvalidData(msg)),
        _ => {}
    }
    for dir in &profile.required_dirs {
        paths::validate_rel(dir)?;
        let path = paths::join(root, dir);
        if path.is_file() {
            return Err(Error::Conflict { path: dir.clone() });
        }
        fs::create_dir_all(&path).map_err(io_ctx(format!("creating the {dir} folder")))?;
    }
    match manifest::read(root) {
        ManifestState::Present(m) => Ok(m),
        _ => {
            let mut m = manifest::fresh(profile);
            manifest::save(root, &mut m)?;
            Ok(m)
        }
    }
}

/// Applies the chosen fixes in a safe order.
pub fn fix_drive(root: &Path, profile: &Profile, index: Option<&Index>, fixes: &[FixAction]) -> Result<()> {
    let order = [
        FixAction::ResetManifest,
        FixAction::Initialize,
        FixAction::RemovePartials,
        FixAction::ForgetMissing,
        FixAction::MigrateProfile,
    ];
    for fix in order.iter().filter(|f| fixes.contains(f)) {
        match fix {
            FixAction::ResetManifest => reset_manifest(root, profile)?,
            FixAction::Initialize => {
                init_drive(root, profile)?;
            }
            FixAction::RemovePartials => remove_partials(root, profile)?,
            FixAction::ForgetMissing => forget_missing(root)?,
            FixAction::MigrateProfile => migrate_profile(root, profile, index)?,
        }
    }
    Ok(())
}

fn reset_manifest(root: &Path, profile: &Profile) -> Result<()> {
    let path = manifest::manifest_path(root);
    if path.exists() {
        let aside = manifest::app_dir(root).join(format!(
            "manifest.damaged-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        fs::rename(&path, aside).map_err(io_ctx("setting the damaged manifest aside"))?;
    }
    let mut m = manifest::fresh(profile);
    manifest::save(root, &mut m)
}

fn remove_partials(root: &Path, profile: &Profile) -> Result<()> {
    for path in find_partials(root, profile) {
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(io_ctx(format!("removing {}", path.display())))?;
        } else {
            fsutil::remove_file_if_exists(&path).map_err(io_ctx(format!("removing {}", path.display())))?;
        }
    }
    Ok(())
}

fn forget_missing(root: &Path) -> Result<()> {
    let mut m = manifest::load(root)?;
    let broken: Vec<InstalledPackage> = m
        .installed
        .iter()
        .filter(|r| r.paths.iter().any(|p| !paths::join(root, p).exists()))
        .cloned()
        .collect();
    for rec in &broken {
        crate::uninstall::remove_files(root, rec)?;
        m.remove(&rec.id);
    }
    manifest::save(root, &mut m)
}

/// Moves each installed package to where the new profile version puts it,
/// then records the new version.
pub fn migrate_profile(root: &Path, profile: &Profile, index: Option<&Index>) -> Result<()> {
    let mut m = manifest::load(root)?;
    if m.profile.id != profile.id {
        return Err(Error::Profile(format!(
            "This drive was set up for \"{}\"",
            m.profile.id
        )));
    }
    for dir in &profile.required_dirs {
        fs::create_dir_all(paths::join(root, dir)).map_err(io_ctx(format!("creating the {dir} folder")))?;
    }
    let mut failures = Vec::new();
    let records = m.installed.clone();
    for rec in records {
        match migrate_one(root, profile, index, &rec) {
            Ok(Some(updated)) => {
                m.upsert(updated);
                manifest::save(root, &mut m)?;
            }
            Ok(None) => {}
            Err(e) => failures.push(format!("{}: {e}", rec.id)),
        }
    }
    if !failures.is_empty() {
        return Err(Error::Profile(format!(
            "Some packages couldn't be moved; reinstall them: {}",
            failures.join("; ")
        )));
    }
    m.profile = profile.as_ref();
    manifest::save(root, &mut m)
}

fn migrate_one(
    root: &Path,
    profile: &Profile,
    index: Option<&Index>,
    rec: &InstalledPackage,
) -> Result<Option<InstalledPackage>> {
    let slot = profile
        .slots
        .get(&rec.slot)
        .ok_or_else(|| Error::Profile(format!("slot \"{}\" no longer exists", rec.slot)))?;
    let asset_name = index
        .and_then(|i| i.package(&rec.id))
        .and_then(|p| paths::file_name_from_url(&p.url).ok())
        .or_else(|| rec.paths.first().and_then(|p| p.rsplit('/').next()).map(str::to_string));
    let place = profile::placement_for(
        profile,
        &rec.slot,
        slot,
        &rec.id,
        &rec.version,
        rec.format,
        asset_name.as_deref(),
    )?;

    let mut updated = rec.clone();
    if rec.extracted {
        let new_dir = place
            .extract_dir
            .ok_or_else(|| Error::Profile("the slot no longer allows unpacked packages".into()))?;
        let old_dir = rec.dirs.first().cloned().unwrap_or_default();
        if old_dir.is_empty() || old_dir == new_dir {
            return Ok(None);
        }
        move_path(root, &old_dir, &new_dir)?;
        updated.paths = rec
            .paths
            .iter()
            .map(|p| match p.strip_prefix(&format!("{old_dir}/")) {
                Some(rest) => paths::rel_join(&new_dir, rest),
                None => p.clone(),
            })
            .collect();
        updated.dirs = vec![new_dir];
    } else {
        let Some(old) = rec.paths.first() else { return Ok(None) };
        if *old == place.file {
            return Ok(None);
        }
        move_path(root, old, &place.file)?;
        updated.paths = vec![place.file];
    }
    Ok(Some(updated))
}

fn move_path(root: &Path, from: &str, to: &str) -> Result<()> {
    let src = paths::join(root, from);
    let dst = paths::join(root, to);
    if dst.exists() {
        return Err(Error::Conflict { path: to.to_string() });
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(io_ctx(format!("creating the folder for {to}")))?;
    }
    fs::rename(&src, &dst).map_err(io_ctx(format!("moving {from} to {to}")))?;
    if let Some(parent) = src.parent() {
        fsutil::prune_empty_dirs(parent, root);
    }
    Ok(())
}

/// What the drive holds, for the drive panel.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstalledView {
    pub record: InstalledPackage,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub latest: Option<String>,
    pub update_available: bool,
    pub in_index: bool,
    pub files_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DriveStatus {
    Uninitialized,
    Ready,
    Corrupt,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DriveState {
    pub path: String,
    pub status: DriveStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile: Option<ProfileRef>,
    pub installed: Vec<InstalledView>,
    pub updates_available: u32,
    pub profile_outdated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Parses the drive manifest and compares it with the index.
pub fn read_drive(root: &Path, index: Option<&Index>) -> DriveState {
    let path = root.to_string_lossy().into_owned();
    let m = match manifest::read(root) {
        ManifestState::Missing => {
            return DriveState {
                path,
                status: DriveStatus::Uninitialized,
                profile: None,
                installed: vec![],
                updates_available: 0,
                profile_outdated: false,
                error: None,
            };
        }
        ManifestState::Corrupt(msg) => {
            return DriveState {
                path,
                status: DriveStatus::Corrupt,
                profile: None,
                installed: vec![],
                updates_available: 0,
                profile_outdated: false,
                error: Some(msg),
            };
        }
        ManifestState::Present(m) => m,
    };
    let installed: Vec<InstalledView> =
        m.installed
            .iter()
            .map(|rec| {
                let pkg = index.and_then(|i| i.package(&rec.id));
                let newer = pkg.is_some_and(|p| {
                    match (semver::Version::parse(&p.version), semver::Version::parse(&rec.version)) {
                        (Ok(latest), Ok(current)) => latest > current,
                        _ => p.version != rec.version,
                    }
                });
                InstalledView {
                    record: rec.clone(),
                    name: pkg.map(|p| p.name.clone()).unwrap_or_else(|| rec.id.clone()),
                    latest: pkg.map(|p| p.version.clone()),
                    update_available: newer,
                    in_index: pkg.is_some(),
                    files_present: rec.paths.iter().all(|p| paths::join(root, p).exists()),
                }
            })
            .collect();
    let profile_outdated = index
        .and_then(|i| i.profile(&m.profile.id))
        .is_some_and(|p| p.version > m.profile.version);
    DriveState {
        path,
        status: DriveStatus::Ready,
        profile: Some(m.profile.clone()),
        updates_available: installed.iter().filter(|v| v.update_available).count() as u32,
        installed,
        profile_outdated,
        error: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::profile::tests::nemesis;

    pub fn drive(root: &Path, fs: &str) -> DriveInfo {
        DriveInfo {
            mount_point: root.to_string_lossy().into_owned(),
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

    fn codes(report: &CheckReport) -> Vec<IssueCode> {
        report.issues.iter().map(|i| i.code).collect()
    }

    #[test]
    fn blank_drive_needs_initializing_then_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let profile = nemesis();
        let report = check_drive(dir.path(), &drive(dir.path(), "exfat"), &profile, None);
        assert!(!report.ready);
        assert!(codes(&report).contains(&IssueCode::NotInitialized));
        assert!(codes(&report).contains(&IssueCode::MissingDir));

        fix_drive(dir.path(), &profile, None, &[FixAction::Initialize]).unwrap();
        let report = check_drive(dir.path(), &drive(dir.path(), "exfat"), &profile, None);
        assert!(report.ready, "{:?}", report.issues);
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert!(dir.path().join("packages").is_dir());
        assert!(dir.path().join(".pkgmgr/manifest.json").is_file());
    }

    #[test]
    fn wrong_filesystem_is_an_error_without_a_fix() {
        let dir = tempfile::tempdir().unwrap();
        let profile = nemesis();
        init_drive(dir.path(), &profile).unwrap();
        let report = check_drive(dir.path(), &drive(dir.path(), "ntfs"), &profile, None);
        assert!(!report.ready);
        let issue = report
            .issues
            .iter()
            .find(|i| i.code == IssueCode::FilesystemNotAllowed)
            .unwrap();
        assert!(issue.fix.is_none());
        assert!(issue.message.contains("exFAT"));
    }

    #[test]
    fn leftover_part_files_are_found_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let profile = nemesis();
        init_drive(dir.path(), &profile).unwrap();
        fs::write(dir.path().join("packages/tool.zip.pkgmgr-part"), b"half").unwrap();
        fs::write(dir.path().join("NEMESIS.data.pkgmgr-part"), b"half").unwrap();
        fs::create_dir_all(dir.path().join(".pkgmgr/staging/tool-1")).unwrap();
        fs::write(dir.path().join("packages/keep.txt"), b"user file").unwrap();

        assert_eq!(find_partials(dir.path(), &profile).len(), 3);
        let report = check_drive(dir.path(), &drive(dir.path(), "fat32"), &profile, None);
        assert!(codes(&report).contains(&IssueCode::PartialFiles));

        fix_drive(dir.path(), &profile, None, &[FixAction::RemovePartials]).unwrap();
        assert!(find_partials(dir.path(), &profile).is_empty());
        assert!(dir.path().join("packages/keep.txt").exists());
    }

    #[test]
    fn corrupt_manifest_can_be_reset() {
        let dir = tempfile::tempdir().unwrap();
        let profile = nemesis();
        init_drive(dir.path(), &profile).unwrap();
        fs::write(manifest::manifest_path(dir.path()), b"garbage").unwrap();
        let report = check_drive(dir.path(), &drive(dir.path(), "exfat"), &profile, None);
        assert!(codes(&report).contains(&IssueCode::ManifestCorrupt));
        fix_drive(dir.path(), &profile, None, &[FixAction::ResetManifest]).unwrap();
        assert!(matches!(manifest::read(dir.path()), ManifestState::Present(_)));
    }

    #[test]
    fn other_profiles_drive_is_not_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let profile = nemesis();
        init_drive(dir.path(), &profile).unwrap();
        let mut other = profile.clone();
        other.id = "other".into();
        assert!(init_drive(dir.path(), &other).is_err());
        let report = check_drive(dir.path(), &drive(dir.path(), "exfat"), &other, None);
        assert!(codes(&report).contains(&IssueCode::ProfileMismatch));
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(4_294_967_296), "4.3 GB");
        assert_eq!(list(&["exfat".into(), "fat32".into()]), "exFAT or FAT32");
    }
}
