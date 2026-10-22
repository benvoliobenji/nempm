//! Removing packages: exactly the files the manifest recorded, nothing else.

use std::fs;
use std::path::Path;

use crate::error::io_ctx;
use crate::model::{DriveManifest, InstalledPackage};
use crate::{Error, Result, fsutil, manifest, paths};

/// Deletes a record's files, then any of its folders left empty. A path that
/// fails validation (a tampered manifest pointing outside the drive) stops the
/// whole operation before anything is deleted.
pub fn remove_files(root: &Path, rec: &InstalledPackage) -> Result<()> {
    for rel in rec.paths.iter().chain(&rec.dirs) {
        paths::validate_rel(rel)?;
        if rel.is_empty() {
            return Err(Error::InvalidData(format!(
                "{} records the drive root as one of its paths",
                rec.id
            )));
        }
        paths::check_not_reserved(rel, &[])?;
    }
    for rel in &rec.paths {
        let path = paths::join(root, rel);
        fsutil::remove_file_if_exists(&path).map_err(io_ctx(format!("removing {rel}")))?;
    }
    for rel in &rec.dirs {
        remove_empty_tree(&paths::join(root, rel));
    }
    Ok(())
}

/// Removes empty folders bottom-up. Anything the user added stays.
fn remove_empty_tree(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_tree(&entry.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Uninstalls `id`. Refuses when another installed package depends on it,
/// unless `force` is set.
pub fn uninstall(root: &Path, id: &str, force: bool) -> Result<DriveManifest> {
    let mut m = manifest::load(root)?;
    let rec = m
        .get(id)
        .cloned()
        .ok_or_else(|| Error::UnknownPackage(id.to_string()))?;
    if !force {
        let dependents: Vec<&str> = m
            .installed
            .iter()
            .filter(|r| r.deps.iter().any(|d| d == id))
            .map(|r| r.id.as_str())
            .collect();
        if !dependents.is_empty() {
            return Err(Error::Dependency(format!(
                "{id} is needed by {}. Uninstall {} first.",
                dependents.join(", "),
                if dependents.len() == 1 { "it" } else { "them" }
            )));
        }
    }
    remove_files(root, &rec)?;
    m.remove(id);
    manifest::save(root, &mut m)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::init_drive;
    use crate::model::PackageFormat;
    use crate::profile::tests::nemesis;

    fn record(id: &str, paths: &[&str], dirs: &[&str], deps: &[&str]) -> InstalledPackage {
        InstalledPackage {
            id: id.into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            slot: "packages".into(),
            format: PackageFormat::Zip,
            extracted: !dirs.is_empty(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            dirs: dirs.iter().map(|s| s.to_string()).collect(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            size: 1,
            installed_at: String::new(),
        }
    }

    #[test]
    fn removes_only_recorded_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut m = init_drive(root, &nemesis()).unwrap();
        fs::create_dir_all(root.join("packages/tool/sub")).unwrap();
        fs::write(root.join("packages/tool/a.txt"), b"a").unwrap();
        fs::write(root.join("packages/tool/sub/b.txt"), b"b").unwrap();
        fs::write(root.join("packages/user.txt"), b"mine").unwrap();
        m.upsert(record(
            "tool",
            &["packages/tool/a.txt", "packages/tool/sub/b.txt"],
            &["packages/tool"],
            &[],
        ));
        manifest::save(root, &mut m).unwrap();

        let m = uninstall(root, "tool", false).unwrap();
        assert!(m.installed.is_empty());
        assert!(!root.join("packages/tool").exists());
        assert!(root.join("packages").is_dir());
        assert!(root.join("packages/user.txt").exists());
    }

    #[test]
    fn user_files_inside_an_extract_folder_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut m = init_drive(root, &nemesis()).unwrap();
        fs::create_dir_all(root.join("packages/tool")).unwrap();
        fs::write(root.join("packages/tool/a.txt"), b"a").unwrap();
        fs::write(root.join("packages/tool/notes.txt"), b"mine").unwrap();
        m.upsert(record("tool", &["packages/tool/a.txt"], &["packages/tool"], &[]));
        manifest::save(root, &mut m).unwrap();
        uninstall(root, "tool", false).unwrap();
        assert!(root.join("packages/tool/notes.txt").exists());
    }

    #[test]
    fn dependents_block_uninstall_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut m = init_drive(root, &nemesis()).unwrap();
        m.upsert(record("core", &["NEMESIS.data"], &[], &[]));
        m.upsert(record("tool", &["packages/tool.zip"], &[], &["core"]));
        manifest::save(root, &mut m).unwrap();
        assert!(matches!(uninstall(root, "core", false), Err(Error::Dependency(_))));
        uninstall(root, "core", true).unwrap();
    }

    #[test]
    fn tampered_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = dir.path().parent().unwrap().join("pkgmgr-outside.txt");
        fs::write(&outside, b"keep").unwrap();
        let mut m = init_drive(root, &nemesis()).unwrap();
        m.upsert(record("evil", &["../pkgmgr-outside.txt"], &[], &[]));
        manifest::save(root, &mut m).unwrap();
        assert!(uninstall(root, "evil", false).is_err());
        assert!(outside.exists());
        let _ = fs::remove_file(outside);

        let mut m = manifest::load(root).unwrap();
        m.remove("evil");
        m.upsert(record("evil2", &[".pkgmgr/manifest.json"], &[], &[]));
        manifest::save(root, &mut m).unwrap();
        assert!(uninstall(root, "evil2", false).is_err());
        assert!(manifest::manifest_path(root).exists());
    }
}
