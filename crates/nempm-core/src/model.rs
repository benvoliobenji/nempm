//! Wire types shared with the registry (`index.json`), the drive
//! (`.nempm/manifest.json`) and the frontend (generated TypeScript bindings).
//!
//! Unknown fields are ignored everywhere so an older app keeps working with a
//! newer index.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum PackageFormat {
    Zip,
    Tar,
}

impl PackageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageFormat::Zip => "zip",
            PackageFormat::Tar => "tar",
        }
    }
}

/// A slot's rule for unpacking zips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ExtractPolicy {
    #[default]
    Never,
    Optional,
    Always,
}

/// One package manifest from the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Package {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    pub format: PackageFormat,
    #[serde(default)]
    pub core: bool,
    pub url: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size: u64,
    /// The data file published in the same release (Nemesis: `NEMESIS.data`),
    /// written to the drive next to the package. Without one, a zip in a slot
    /// with a data file carries it inside and is always unpacked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<DataAsset>,
    /// Default for unpacking a zip; the user can override it per install.
    #[serde(default)]
    pub extract: bool,
    pub slot: String,
    pub profiles: Vec<String>,
    /// Packages that must already be on the target device (not the drive).
    /// Shown to the user; never installed automatically.
    #[serde(default, alias = "deps")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DataAsset {
    pub url: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size: u64,
}

/// One downloadable file: a package's main asset or its data file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// For messages: "Example Tool" or "Example Tool's data file".
    pub label: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Package {
    pub fn asset(&self) -> Asset {
        Asset {
            label: self.name.clone(),
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            size: self.size,
        }
    }

    pub fn data_asset(&self) -> Option<Asset> {
        self.data.as_ref().map(|d| Asset {
            label: format!("{}'s data file", self.name),
            url: d.url.clone(),
            sha256: d.sha256.clone(),
            size: d.size,
        })
    }

    /// Bytes written to the drive (package plus data file).
    pub fn total_size(&self) -> u64 {
        self.size + self.data.as_ref().map_or(0, |d| d.size)
    }
}

/// Where a kind of package goes in a profile's layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Slot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Folder the package file is written to; empty means the drive root.
    #[serde(default)]
    pub path: String,
    /// File name template; defaults to the asset's own file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file: Option<String>,
    pub allow: Vec<PackageFormat>,
    #[serde(default)]
    pub extract: ExtractPolicy,
    /// Folder a zip is unpacked into; defaults to `<path>/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub extract_dir: Option<String>,
    /// Where a package's data file goes (Nemesis: `NEMESIS.data`). Packages
    /// in a slot with one publish it as an asset or carry it in their zip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VolumeLabelRule {
    pub pattern: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum NameCase {
    Lower,
    Upper,
    #[default]
    Any,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NamingRule {
    pub pattern: String,
    #[serde(default)]
    pub case: NameCase,
}

/// The USB layout a target device expects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Profile {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    pub version: u32,
    /// Normalised names: `fat32`, `exfat`, `ntfs`, `apfs`, `hfsplus`, `ext4`.
    pub filesystems: Vec<String>,
    #[serde(default)]
    #[ts(type = "Record<string, number>")]
    pub max_file_bytes: BTreeMap<String, u64>,
    /// How many packages a drive holds at once (Nemesis: 1). Installing
    /// another replaces what's there, once the user confirms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_packages: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub volume_label: Option<VolumeLabelRule>,
    #[serde(default)]
    pub required_dirs: Vec<String>,
    pub slots: BTreeMap<String, Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub naming: Option<NamingRule>,
    #[serde(default)]
    pub reserved: Vec<String>,
}

impl Profile {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn as_ref(&self) -> ProfileRef {
        ProfileRef {
            id: self.id.clone(),
            version: self.version,
        }
    }
}

/// `index.json`, built by the registry's CI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Index {
    pub schema_version: u32,
    pub generated_at: String,
    pub packages: Vec<Package>,
    pub profiles: Vec<Profile>,
}

impl Index {
    pub fn package(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id == id)
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileRef {
    pub id: String,
    pub version: u32,
}

/// One package as recorded on the drive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstalledPackage {
    pub id: String,
    pub version: String,
    pub sha256: String,
    #[serde(default)]
    pub slot: String,
    #[serde(default = "default_format")]
    pub format: PackageFormat,
    #[serde(default)]
    pub extracted: bool,
    /// Every file written, relative to the drive root with `/` separators.
    /// Uninstall removes exactly these and nothing else.
    pub paths: Vec<String>,
    /// Folders owned by this package (an extraction folder); removed on
    /// uninstall once empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    /// Which of `paths` is the package's data file, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<String>,
    /// For an unpacked package, the folder its zip went into ("" is the
    /// drive root). Older records leave it out; their first `dirs` entry is it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unpacked_into: Option<String>,
    #[serde(default)]
    #[ts(type = "number")]
    pub size: u64,
    pub installed_at: String,
}

fn default_format() -> PackageFormat {
    PackageFormat::Zip
}

fn one() -> u32 {
    1
}

/// `.nempm/manifest.json` on the drive: makes the drive self-describing on
/// any computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DriveManifest {
    #[serde(default = "one")]
    pub schema_version: u32,
    pub profile: ProfileRef,
    #[serde(default)]
    pub installed: Vec<InstalledPackage>,
    pub written_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub updated_at: Option<String>,
}

impl DriveManifest {
    pub fn get(&self, id: &str) -> Option<&InstalledPackage> {
        self.installed.iter().find(|p| p.id == id)
    }

    /// Inserts or replaces the record for `record.id`, keeping list order.
    pub fn upsert(&mut self, record: InstalledPackage) {
        match self.installed.iter_mut().find(|p| p.id == record.id) {
            Some(existing) => *existing = record,
            None => self.installed.push(record),
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<InstalledPackage> {
        let pos = self.installed.iter().position(|p| p.id == id)?;
        Some(self.installed.remove(pos))
    }

    /// The installed package that recorded `rel` among its files, if any.
    pub fn owner_of(&self, rel: &str) -> Option<&InstalledPackage> {
        let rel = rel.to_ascii_lowercase();
        self.installed.iter().find(|p| {
            p.paths.iter().any(|x| x.eq_ignore_ascii_case(&rel)) || p.dirs.iter().any(|d| d.eq_ignore_ascii_case(&rel))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_parses_with_defaults_and_ignores_unknown_fields() {
        let pkg: Package = serde_json::from_str(
            r#"{"id":"a","name":"A","version":"1.0.0","format":"zip","url":"https://x/a.zip",
                "sha256":"00","size":1,"slot":"packages","profiles":["nemesis"],"future":true}"#,
        )
        .unwrap();
        assert!(!pkg.core);
        assert!(!pkg.extract);
        assert!(pkg.requires.is_empty());
        assert!(pkg.data.is_none());
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = DriveManifest {
            schema_version: 1,
            profile: ProfileRef {
                id: "nemesis".into(),
                version: 1,
            },
            installed: vec![InstalledPackage {
                id: "a".into(),
                version: "1.0.0".into(),
                sha256: "00".into(),
                slot: "packages".into(),
                format: PackageFormat::Zip,
                extracted: false,
                paths: vec!["packages/a.zip".into()],
                dirs: vec![],
                data: None,
                size: 1,
                unpacked_into: None,
                installed_at: "2026-10-21T19:00:00Z".into(),
            }],
            written_by: "nempm 0.1.0".into(),
            updated_at: None,
        };
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(text.contains("\"writtenBy\""));
        assert!(text.contains("\"installedAt\""));
        let back: DriveManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.owner_of("PACKAGES/A.ZIP").unwrap().id, "a");
    }
}
