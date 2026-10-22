//! The drive manifest, `.pkgmgr/manifest.json`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::io_ctx;
use crate::fsutil;
use crate::model::{DriveManifest, Profile};
use crate::{Error, Result, WRITTEN_BY};

/// The app's own folder on the drive. Always reserved.
pub const APP_DIR: &str = ".pkgmgr";
pub const MANIFEST_FILE: &str = "manifest.json";
/// Where zips are unpacked before being renamed into place.
pub const STAGING_DIR: &str = "staging";

pub const SCHEMA_VERSION: u32 = 1;

pub fn app_dir(root: &Path) -> PathBuf {
    root.join(APP_DIR)
}

pub fn manifest_path(root: &Path) -> PathBuf {
    app_dir(root).join(MANIFEST_FILE)
}

pub fn staging_dir(root: &Path) -> PathBuf {
    app_dir(root).join(STAGING_DIR)
}

#[derive(Debug)]
pub enum ManifestState {
    Missing,
    Present(DriveManifest),
    Corrupt(String),
}

pub fn read(root: &Path) -> ManifestState {
    let path = manifest_path(root);
    let text = match fs::read(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ManifestState::Missing,
        Err(e) => return ManifestState::Corrupt(format!("Couldn't read the drive manifest: {e}")),
    };
    match serde_json::from_slice::<DriveManifest>(&text) {
        Ok(m) if m.schema_version > SCHEMA_VERSION => ManifestState::Corrupt(format!(
            "This drive was written by a newer version of the app ({}). Update the app.",
            m.written_by
        )),
        Ok(m) => ManifestState::Present(m),
        Err(e) => ManifestState::Corrupt(format!("The drive manifest is damaged: {e}")),
    }
}

/// The manifest, or an error saying why there isn't a usable one.
pub fn load(root: &Path) -> Result<DriveManifest> {
    match read(root) {
        ManifestState::Present(m) => Ok(m),
        ManifestState::Missing => Err(Error::NotInitialized),
        ManifestState::Corrupt(msg) => Err(Error::InvalidData(msg)),
    }
}

/// Writes the manifest atomically (`.tmp`, fsync, rename).
pub fn save(root: &Path, manifest: &mut DriveManifest) -> Result<()> {
    manifest.written_by = WRITTEN_BY.to_string();
    manifest.updated_at = Some(now());
    let dir = app_dir(root);
    fs::create_dir_all(&dir).map_err(io_ctx("creating the .pkgmgr folder on the drive"))?;
    let text = serde_json::to_vec_pretty(manifest)?;
    fsutil::write_atomic(&manifest_path(root), &text).map_err(io_ctx("saving the drive manifest"))
}

pub fn fresh(profile: &Profile) -> DriveManifest {
    DriveManifest {
        schema_version: SCHEMA_VERSION,
        profile: profile.as_ref(),
        installed: Vec::new(),
        written_by: WRITTEN_BY.to_string(),
        updated_at: None,
    }
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::tests::nemesis;

    #[test]
    fn missing_present_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read(dir.path()), ManifestState::Missing));
        assert!(matches!(load(dir.path()), Err(Error::NotInitialized)));

        let mut m = fresh(&nemesis());
        save(dir.path(), &mut m).unwrap();
        let ManifestState::Present(back) = read(dir.path()) else {
            panic!("expected a manifest")
        };
        assert_eq!(back.profile.id, "nemesis");
        assert!(back.updated_at.is_some());

        fs::write(manifest_path(dir.path()), b"{ not json").unwrap();
        assert!(matches!(read(dir.path()), ManifestState::Corrupt(_)));
    }
}
