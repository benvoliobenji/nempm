//! Removing packages: exactly the files the manifest recorded, nothing else.

use std::fs;
use std::path::Path;

use crate::error::io_ctx;
use crate::model::{DriveManifest, InstalledPackage};
use crate::{Error, Result, fsutil, manifest, paths};

/// Deletes a record's files, then every folder that leaves empty, up to (not
/// including) the drive root. Folders in `keep` (a profile's required folders)
/// and anything still holding other files stay. A path that fails validation
/// (a tampered manifest pointing outside the drive) stops the whole operation
/// before anything is deleted.
pub fn remove_files(root: &Path, rec: &InstalledPackage, keep: &[String]) -> Result<()> {
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
    for rel in rec.paths.iter().chain(&rec.dirs) {
        prune_parents(root, rel, keep);
    }
    Ok(())
}

/// Removes the empty folders above `rel` (a file or folder that has just been
/// deleted), walking up until a folder isn't empty, is in `keep`, or is the
/// drive root.
pub(crate) fn prune_parents(root: &Path, rel: &str, keep: &[String]) {
    let mut dir = paths::rel_parent(rel);
    while let Some(d) = dir {
        if d.is_empty()
            || keep.iter().any(|k| k.eq_ignore_ascii_case(d))
            || d.split('/').next().is_some_and(manifest::is_app_dir)
            || fs::remove_dir(paths::join(root, d)).is_err()
        {
            break;
        }
        dir = paths::rel_parent(d);
    }
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

/// Uninstalls `id`: its files, including its data file, the folders that
/// leaves empty (except those in `keep`), and its record.
pub fn uninstall(root: &Path, id: &str, keep: &[String]) -> Result<DriveManifest> {
    let mut m = manifest::load(root)?;
    let rec = m
        .get(id)
        .cloned()
        .ok_or_else(|| Error::UnknownPackage(id.to_string()))?;
    remove_files(root, &rec, keep)?;
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

    fn record(id: &str, paths: &[&str], dirs: &[&str], data: Option<&str>) -> InstalledPackage {
        InstalledPackage {
            id: id.into(),
            version: "1.0.0".into(),
            sha256: "a".repeat(64),
            slot: "packages".into(),
            format: PackageFormat::Zip,
            extracted: !dirs.is_empty(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            dirs: dirs.iter().map(|s| s.to_string()).collect(),
            data: data.map(str::to_string),
            size: 1,
            unpacked_into: None,
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
        fs::write(root.join("NEMESIS.data"), b"tool's data").unwrap();
        m.upsert(record(
            "tool",
            &["NEMESIS.data", "packages/tool/a.txt", "packages/tool/sub/b.txt"],
            &["packages/tool"],
            Some("NEMESIS.data"),
        ));
        manifest::save(root, &mut m).unwrap();

        let m = uninstall(root, "tool", &[]).unwrap();
        assert!(m.installed.is_empty());
        assert!(!root.join("packages/tool").exists());
        assert!(
            !root.join("NEMESIS.data").exists(),
            "the data file goes with its package"
        );
        assert!(root.join("packages").is_dir(), "still holds the user's file");
        assert!(root.join("packages/user.txt").exists());
    }

    #[test]
    fn folders_left_empty_are_removed_up_to_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut m = init_drive(root, &nemesis()).unwrap();
        fs::create_dir_all(root.join("packages")).unwrap();
        fs::create_dir_all(root.join("launchpad/lib")).unwrap();
        fs::create_dir_all(root.join("kept")).unwrap();
        for f in [
            "NEMESIS.data",
            "packages/inner.zip",
            "launchpad/install.sh",
            "launchpad/lib/a.sh",
            "kept/x",
        ] {
            fs::write(root.join(f), b"x").unwrap();
        }
        m.upsert(record(
            "tool",
            &[
                "NEMESIS.data",
                "packages/inner.zip",
                "launchpad/install.sh",
                "launchpad/lib/a.sh",
                "kept/x",
            ],
            &[],
            Some("NEMESIS.data"),
        ));
        manifest::save(root, &mut m).unwrap();
        uninstall(root, "tool", &["kept".into()]).unwrap();
        assert!(!root.join("packages").exists());
        assert!(!root.join("launchpad").exists());
        assert!(root.join("kept").is_dir(), "required folders stay");
        assert!(root.join(".nempm/manifest.json").exists());
    }

    #[test]
    fn user_files_inside_an_extract_folder_survive() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut m = init_drive(root, &nemesis()).unwrap();
        fs::create_dir_all(root.join("packages/tool")).unwrap();
        fs::write(root.join("packages/tool/a.txt"), b"a").unwrap();
        fs::write(root.join("packages/tool/notes.txt"), b"mine").unwrap();
        m.upsert(record("tool", &["packages/tool/a.txt"], &["packages/tool"], None));
        manifest::save(root, &mut m).unwrap();
        uninstall(root, "tool", &[]).unwrap();
        assert!(root.join("packages/tool/notes.txt").exists());
    }

    #[test]
    fn tampered_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = dir.path().parent().unwrap().join("nempm-outside.txt");
        fs::write(&outside, b"keep").unwrap();
        let mut m = init_drive(root, &nemesis()).unwrap();
        m.upsert(record("evil", &["../nempm-outside.txt"], &[], None));
        manifest::save(root, &mut m).unwrap();
        assert!(uninstall(root, "evil", &[]).is_err());
        assert!(outside.exists());
        let _ = fs::remove_file(outside);

        let mut m = manifest::load(root).unwrap();
        m.remove("evil");
        m.upsert(record("evil2", &[".nempm/manifest.json"], &[], None));
        manifest::save(root, &mut m).unwrap();
        assert!(uninstall(root, "evil2", &[]).is_err());
        assert!(manifest::manifest_path(root).exists());
    }
}
