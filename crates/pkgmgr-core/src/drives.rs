//! Finding removable drives.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sysinfo::Disks;
use ts_rs::TS;

use crate::profile::normalize_filesystem;
use crate::{Error, Result, fsutil};

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DriveInfo {
    /// Where the volume is mounted (`E:\`, `/media/ben/STICK`, `/Volumes/STICK`),
    /// or the folder picked with Choose folder.
    pub mount_point: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// Normalised: `fat32`, `exfat`, `ntfs`, ... or `unknown`.
    pub filesystem: String,
    pub filesystem_raw: String,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub free_bytes: u64,
    pub removable: bool,
    pub read_only: bool,
    /// Block device on Linux (`/dev/sdb1`), used to eject.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub device: Option<String>,
}

impl DriveInfo {
    pub fn root(&self) -> PathBuf {
        PathBuf::from(&self.mount_point)
    }
}

/// Removes Windows' `\\?\` prefix from canonicalized paths so they compare
/// equal to mount points like `E:\`.
pub fn simplify(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

fn is_system_mount(mount: &Path) -> bool {
    if mount == Path::new("/") {
        return true;
    }
    #[cfg(windows)]
    if let Ok(sys) = std::env::var("SystemDrive") {
        let m = mount.to_string_lossy().trim_end_matches('\\').to_ascii_uppercase();
        if m == sys.trim_end_matches('\\').to_ascii_uppercase() {
            return true;
        }
    }
    #[cfg(target_os = "macos")]
    if mount.starts_with("/System/Volumes") {
        return true;
    }
    #[cfg(target_os = "linux")]
    for prefix in [
        "/boot",
        "/snap",
        "/var",
        "/usr",
        "/home",
        "/run/user",
        "/sys",
        "/proc",
        "/dev",
    ] {
        if mount.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Mount points where desktop Linux and macOS put USB drives.
fn is_user_media_mount(mount: &Path) -> bool {
    mount.starts_with("/media") || mount.starts_with("/run/media") || mount.starts_with("/Volumes")
}

fn label_for(disk: &sysinfo::Disk) -> Option<String> {
    let name = disk.name().to_string_lossy().into_owned();
    if cfg!(target_os = "linux") {
        // sysinfo reports the device path on Linux; the mount folder is
        // named after the label by udisks.
        return disk.mount_point().file_name().map(|n| n.to_string_lossy().into_owned());
    }
    (!name.is_empty()).then_some(name)
}

fn info_for(disk: &sysinfo::Disk) -> DriveInfo {
    let raw_fs = disk.file_system().to_string_lossy().into_owned();
    let mount = disk.mount_point();
    DriveInfo {
        mount_point: mount.to_string_lossy().into_owned(),
        label: label_for(disk),
        filesystem: normalize_filesystem(&raw_fs),
        filesystem_raw: raw_fs,
        total_bytes: disk.total_space(),
        free_bytes: fsutil::free_space(mount).unwrap_or_else(|_| disk.available_space()),
        removable: disk.is_removable(),
        read_only: disk.is_read_only(),
        device: cfg!(target_os = "linux").then(|| disk.name().to_string_lossy().into_owned()),
    }
}

/// Removable volumes, plus every non-system volume when `include_fixed` is set
/// (USB hard drives often report themselves as fixed).
pub fn list_drives(include_fixed: bool) -> Vec<DriveInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut out: Vec<DriveInfo> = disks
        .list()
        .iter()
        .filter(|d| {
            let mount = d.mount_point();
            !is_system_mount(mount) && (d.is_removable() || is_user_media_mount(mount) || include_fixed)
        })
        .map(info_for)
        .collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out.dedup_by(|a, b| a.mount_point == b.mount_point);
    out
}

/// Describes any folder, for the Choose folder fallback: the filesystem and
/// space come from the volume that contains it.
pub fn inspect_path(path: &Path) -> Result<DriveInfo> {
    let canonical = path
        .canonicalize()
        .map_err(|e| Error::InvalidData(format!("Can't open {}: {e}", path.display())))?;
    let canonical = simplify(&canonical);
    if !canonical.is_dir() {
        return Err(Error::InvalidData(format!("{} is not a folder", path.display())));
    }
    let disks = Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|d| canonical.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());
    let mut info = match disk {
        Some(d) => info_for(d),
        None => DriveInfo {
            mount_point: String::new(),
            label: None,
            filesystem: "unknown".into(),
            filesystem_raw: String::new(),
            total_bytes: 0,
            free_bytes: 0,
            removable: false,
            read_only: false,
            device: None,
        },
    };
    let is_volume_root = disk.is_some_and(|d| d.mount_point() == canonical);
    if !is_volume_root {
        info.label = canonical.file_name().map(|n| n.to_string_lossy().into_owned());
        info.device = None;
    }
    info.mount_point = canonical.to_string_lossy().into_owned();
    info.free_bytes = fsutil::free_space(&canonical).unwrap_or(info.free_bytes);
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplify_strips_verbatim_prefix() {
        assert_eq!(simplify(Path::new(r"\\?\E:\folder")), PathBuf::from(r"E:\folder"));
        assert_eq!(
            simplify(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\?\UNC\server\share")
        );
        assert_eq!(simplify(Path::new("/media/x")), PathBuf::from("/media/x"));
    }

    #[test]
    fn inspect_a_temp_folder() {
        let dir = tempfile::tempdir().unwrap();
        let info = inspect_path(dir.path()).unwrap();
        assert!(info.free_bytes > 0);
        assert!(!info.mount_point.is_empty());
    }

    #[test]
    fn listing_does_not_panic() {
        let _ = list_drives(false);
        let all = list_drives(true);
        assert!(all.iter().all(|d| d.mount_point != "/"));
    }
}
