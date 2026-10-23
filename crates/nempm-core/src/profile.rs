//! Target profiles: validating them, and resolving a package's slot to the
//! concrete place it goes on the drive.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{ExtractPolicy, NameCase, Package, PackageFormat, Profile, Slot};
use crate::paths::{self, Vars};
use crate::{Error, Result};

/// FAT32 can't store a file of 4 GiB or more.
pub const FAT32_MAX_FILE: u64 = 4_294_967_295;

static SHA256_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-f0-9]{64}$").unwrap());

/// Maps what each OS reports (`FAT32`, `vfat`, `msdos`, `exFAT`, ...) onto the
/// names profiles use.
pub fn normalize_filesystem(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "fat32" | "vfat" | "msdos" | "fat" => "fat32".into(),
        "exfat" => "exfat".into(),
        "ntfs" | "ntfs3" => "ntfs".into(),
        "apfs" => "apfs".into(),
        "hfs" | "hfs+" | "hfsplus" => "hfsplus".into(),
        "" => "unknown".into(),
        _ => lower,
    }
}

/// The largest single file this profile allows on the given filesystem.
pub fn max_file_size(profile: &Profile, filesystem: &str) -> Option<u64> {
    profile
        .max_file_bytes
        .get(filesystem)
        .copied()
        .or((filesystem == "fat32").then_some(FAT32_MAX_FILE))
}

/// Where one package goes on the drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub slot: String,
    /// Drive-relative path of the package file when it is copied as-is.
    pub file: String,
    /// Drive-relative folder it is unpacked into, if the slot allows that.
    pub extract_dir: Option<String>,
    /// Drive-relative path of the package's data file, if the slot takes one.
    pub data_file: Option<String>,
}

impl Placement {
    /// The path an install with this extract choice writes to.
    pub fn target(&self, extract: bool) -> &str {
        match (&self.extract_dir, extract) {
            (Some(dir), true) => dir,
            _ => &self.file,
        }
    }
}

/// Resolves `slot` → concrete paths for `pkg` under `profile`.
pub fn placement(profile: &Profile, pkg: &Package) -> Result<Placement> {
    let slot = slot_for(profile, pkg)?;
    let file_name = match &slot.file {
        Some(_) => None,
        None => Some(paths::file_name_from_url(&pkg.url)?),
    };
    placement_for(
        profile,
        &pkg.slot,
        slot,
        &pkg.id,
        &pkg.version,
        pkg.format,
        file_name.as_deref(),
    )
}

/// Like [`placement`], for a package that may no longer be in the index
/// (used when migrating a drive to a new profile version).
pub fn placement_for(
    profile: &Profile,
    slot_name: &str,
    slot: &Slot,
    id: &str,
    version: &str,
    format: PackageFormat,
    asset_file_name: Option<&str>,
) -> Result<Placement> {
    paths::validate_id(id)?;
    semver::Version::parse(version)
        .map_err(|e| Error::InvalidData(format!("{id}: version \"{version}\" is not valid: {e}")))?;
    let v = Vars {
        id,
        version,
        format: format.as_str(),
    };
    let dir = paths::render(&slot.path, &v)?;
    let name = match &slot.file {
        Some(template) => paths::render(template, &v)?,
        None => asset_file_name
            .ok_or_else(|| Error::Profile(format!("Slot \"{slot_name}\" needs a file name")))?
            .to_string(),
    };
    paths::validate_component(&name).map_err(Error::Profile)?;
    let file = paths::rel_join(&dir, &name);
    check_path(profile, &file)?;

    let extract_dir = match slot.extract {
        ExtractPolicy::Never => None,
        _ => {
            let template = slot
                .extract_dir
                .clone()
                .unwrap_or_else(|| paths::rel_join(&slot.path, "{id}"));
            // Empty means the drive root: the zip's own folders land there.
            let dir = paths::render(&template, &v)?;
            check_path(profile, &dir)?;
            Some(dir)
        }
    };

    let data_file = match &slot.data_file {
        Some(template) => {
            let rel = paths::render(template, &v)?;
            check_path(profile, &rel)?;
            if rel.is_empty() || rel == file {
                return Err(Error::Profile(format!(
                    "Slot \"{slot_name}\" has an invalid data file path \"{rel}\""
                )));
            }
            Some(rel)
        }
        None => None,
    };

    Ok(Placement {
        slot: slot_name.to_string(),
        file,
        extract_dir,
        data_file,
    })
}

fn check_path(profile: &Profile, rel: &str) -> Result<()> {
    paths::validate_rel(rel).map_err(|e| Error::Profile(e.to_string()))?;
    paths::check_not_reserved(rel, &profile.reserved)
}

/// The slot `pkg` names, after checking the package belongs on this profile.
pub fn slot_for<'p>(profile: &'p Profile, pkg: &Package) -> Result<&'p Slot> {
    if !pkg.profiles.iter().any(|p| p == &profile.id) {
        return Err(Error::Profile(format!(
            "{} isn't made for {}",
            pkg.name,
            profile.display_name()
        )));
    }
    let slot = profile.slots.get(&pkg.slot).ok_or_else(|| {
        Error::Profile(format!(
            "{} uses slot \"{}\", which {} doesn't have",
            pkg.name,
            pkg.slot,
            profile.display_name()
        ))
    })?;
    if !slot.allow.contains(&pkg.format) {
        return Err(Error::Profile(format!(
            "Slot \"{}\" in {} doesn't accept {} files",
            pkg.slot,
            profile.display_name(),
            pkg.format.as_str()
        )));
    }
    match (&slot.data_file, &pkg.data) {
        // A zip without a data asset carries the file itself and is unpacked.
        (Some(file), None) if pkg.format != PackageFormat::Zip || slot.extract == ExtractPolicy::Never => {
            return Err(Error::Profile(format!(
                "{} is missing the {file} that {} needs next to it. Ask its publisher to add it to the release.",
                pkg.name,
                profile.display_name()
            )));
        }
        (None, Some(_)) => {
            return Err(Error::Profile(format!(
                "{} comes with a data file, but slot \"{}\" in {} doesn't take one",
                pkg.name,
                pkg.slot,
                profile.display_name()
            )));
        }
        _ => {}
    }
    if let Some(rule) = &profile.naming {
        let re =
            Regex::new(&rule.pattern).map_err(|e| Error::Profile(format!("Profile naming pattern is invalid: {e}")))?;
        let case_ok = match rule.case {
            NameCase::Lower => pkg.id == pkg.id.to_lowercase(),
            NameCase::Upper => pkg.id == pkg.id.to_uppercase(),
            NameCase::Any => true,
        };
        if !re.is_match(&pkg.id) || !case_ok {
            return Err(Error::Profile(format!(
                "Package id \"{}\" doesn't follow {}'s naming rule",
                pkg.id,
                profile.display_name()
            )));
        }
    }
    Ok(slot)
}

/// Whether this install unpacks the zip. The slot has the final say: `never`
/// overrides the package default and the user's choice; a tar is never
/// unpacked.
pub fn effective_extract(slot: &Slot, pkg: &Package, requested: Option<bool>) -> bool {
    if pkg.format == PackageFormat::Tar {
        return false;
    }
    match slot.extract {
        ExtractPolicy::Never => false,
        ExtractPolicy::Always => true,
        ExtractPolicy::Optional => data_in_zip(slot, pkg) || requested.unwrap_or(pkg.extract),
    }
}

/// Whether the package's data file is inside its zip rather than an asset of
/// its own. Such a zip is always unpacked, which puts the file in place.
pub fn data_in_zip(slot: &Slot, pkg: &Package) -> bool {
    slot.data_file.is_some() && pkg.data.is_none() && pkg.format == PackageFormat::Zip
}

/// Defensive checks on a profile from the index. The registry CI enforces the
/// same rules; the app re-checks so a bad index can't make it write somewhere
/// unexpected.
pub fn validate_profile(profile: &Profile) -> Result<()> {
    paths::validate_id(&profile.id)?;
    if profile.filesystems.is_empty() {
        return Err(Error::InvalidData(format!(
            "Profile {} lists no filesystems",
            profile.id
        )));
    }
    if profile.slots.is_empty() {
        return Err(Error::InvalidData(format!("Profile {} has no slots", profile.id)));
    }
    if profile.max_packages == Some(0) {
        return Err(Error::InvalidData(format!(
            "Profile {}: maxPackages must be at least 1",
            profile.id
        )));
    }
    for dir in &profile.required_dirs {
        if dir.is_empty() || dir.contains('{') {
            return Err(Error::InvalidData(format!(
                "Profile {}: required folder \"{dir}\" is invalid",
                profile.id
            )));
        }
        check_path(profile, dir)?;
    }
    if let Some(rule) = &profile.naming {
        Regex::new(&rule.pattern)
            .map_err(|e| Error::InvalidData(format!("Profile {}: bad naming pattern: {e}", profile.id)))?;
    }
    if let Some(rule) = &profile.volume_label {
        Regex::new(&rule.pattern)
            .map_err(|e| Error::InvalidData(format!("Profile {}: bad label pattern: {e}", profile.id)))?;
    }
    for (name, slot) in &profile.slots {
        // Render with sample values to catch bad templates early.
        let sample = Vars {
            id: "sample",
            version: "1.0.0",
            format: "zip",
        };
        let dir = paths::render(&slot.path, &sample)?;
        check_path(profile, &dir)?;
        if let Some(file) = &slot.file {
            let file = paths::render(file, &sample)?;
            paths::validate_component(&file).map_err(Error::InvalidData)?;
        }
        if let Some(data) = &slot.data_file {
            let data = paths::render(data, &sample)?;
            if data.is_empty() {
                return Err(Error::InvalidData(format!(
                    "Profile {}: slot \"{name}\" has an empty data file path",
                    profile.id
                )));
            }
            check_path(profile, &data)?;
        }
        if slot.extract != ExtractPolicy::Never {
            let template = slot
                .extract_dir
                .clone()
                .unwrap_or_else(|| paths::rel_join(&slot.path, "{id}"));
            // Packages would unpack over each other, unless the drive (or
            // slot) only ever holds one.
            let holds_one = profile.max_packages == Some(1) || slot.max == Some(1);
            if !template.contains("{id}") && !holds_one {
                return Err(Error::InvalidData(format!(
                    "Profile {}: slot \"{name}\" must unpack into a folder containing {{id}}, or hold one package",
                    profile.id
                )));
            }
            let dir = paths::render(&template, &sample)?;
            check_path(profile, &dir)?;
        }
    }
    Ok(())
}

/// Defensive checks on a package from the index.
pub fn validate_package(pkg: &Package) -> Result<()> {
    paths::validate_id(&pkg.id)?;
    semver::Version::parse(&pkg.version)
        .map_err(|e| Error::InvalidData(format!("{}: invalid version \"{}\": {e}", pkg.id, pkg.version)))?;
    if !SHA256_RE.is_match(&pkg.sha256) {
        return Err(Error::InvalidData(format!(
            "{}: sha256 must be 64 lowercase hex characters",
            pkg.id
        )));
    }
    if pkg.size == 0 {
        return Err(Error::InvalidData(format!("{}: size must be positive", pkg.id)));
    }
    if pkg.format == PackageFormat::Tar && pkg.extract {
        return Err(Error::InvalidData(format!(
            "{}: a tar package can't be extracted",
            pkg.id
        )));
    }
    if pkg.core && pkg.format != PackageFormat::Tar {
        return Err(Error::InvalidData(format!(
            "{}: the core package must be a tar",
            pkg.id
        )));
    }
    for req in &pkg.requires {
        paths::validate_id(req)?;
    }
    crate::http::check_url(&pkg.url)?;
    if let Some(data) = &pkg.data {
        if pkg.core {
            return Err(Error::InvalidData(format!(
                "{}: the core package has no data file",
                pkg.id
            )));
        }
        if !SHA256_RE.is_match(&data.sha256) || data.size == 0 {
            return Err(Error::InvalidData(format!(
                "{}: the data file needs a 64-character lowercase sha256 and a positive size",
                pkg.id
            )));
        }
        crate::http::check_url(&data.url)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub fn nemesis() -> Profile {
        serde_json::from_str(include_str!("../tests/fixtures/nemesis.json")).unwrap()
    }

    pub fn pkg(id: &str, format: PackageFormat, slot: &str) -> Package {
        Package {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            version: "1.0.0".into(),
            format,
            core: format == PackageFormat::Tar,
            url: format!(
                "https://github.com/o/{id}/releases/download/v1.0.0/{id}.{}",
                format.as_str()
            ),
            sha256: "a".repeat(64),
            size: 10,
            extract: false,
            data: (slot == "packages").then(|| crate::model::DataAsset {
                url: format!("https://github.com/o/{id}/releases/download/v1.0.0/NEMESIS.data"),
                sha256: "b".repeat(64),
                size: 5,
            }),
            slot: slot.into(),
            profiles: vec!["nemesis".into()],
            requires: vec![],
            homepage: None,
        }
    }

    #[test]
    fn nemesis_layout() {
        let profile = nemesis();
        validate_profile(&profile).unwrap();

        assert_eq!(profile.max_packages, Some(1));

        // Core: the .tar at the drive root under its release file name.
        let core = placement(&profile, &pkg("nemesis-core", PackageFormat::Tar, "core")).unwrap();
        assert_eq!(core.file, "nemesis-core.tar");
        assert_eq!(core.extract_dir, None);
        assert_eq!(core.data_file, None);

        // A tool: NEMESIS.data at the root plus packages/<id>.zip.
        let tool = placement(&profile, &pkg("my.tool", PackageFormat::Zip, "packages")).unwrap();
        assert_eq!(tool.file, "packages/my.tool.zip");
        assert_eq!(tool.data_file.as_deref(), Some("NEMESIS.data"));
        // Unpacked, the zip's contents go straight into the drive root.
        assert_eq!(tool.extract_dir.as_deref(), Some(""));
        assert_eq!(tool.target(false), "packages/my.tool.zip");
        assert_eq!(tool.target(true), "");
    }

    #[test]
    fn data_file_must_match_the_slot() {
        let profile = nemesis();
        // A zip without a data asset is fine: the file comes inside it...
        let mut tool = pkg("tool", PackageFormat::Zip, "packages");
        tool.data = None;
        placement(&profile, &tool).unwrap();
        // ...unless the slot can't unpack it.
        let mut never = profile.clone();
        let slot = never.slots.get_mut("packages").unwrap();
        slot.extract = ExtractPolicy::Never;
        slot.extract_dir = None;
        assert!(matches!(placement(&never, &tool), Err(Error::Profile(m)) if m.contains("NEMESIS.data")));
        let mut core = pkg("nemesis-core", PackageFormat::Tar, "core");
        core.data = pkg("tool", PackageFormat::Zip, "packages").data;
        assert!(placement(&profile, &core).is_err());
        assert!(validate_package(&core).is_err());
    }

    #[test]
    fn default_file_name_comes_from_the_url() {
        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().file = None;
        let tool = placement(&profile, &pkg("tool", PackageFormat::Zip, "packages")).unwrap();
        assert_eq!(tool.file, "packages/tool.zip");
    }

    #[test]
    fn slot_rules_are_enforced() {
        let profile = nemesis();
        assert!(placement(&profile, &pkg("tool", PackageFormat::Zip, "core")).is_err());
        assert!(placement(&profile, &pkg("tool", PackageFormat::Zip, "missing")).is_err());
        let mut other = pkg("tool", PackageFormat::Zip, "packages");
        other.profiles = vec!["other".into()];
        assert!(placement(&profile, &other).is_err());
    }

    #[test]
    fn extract_never_overrides_everything() {
        let profile = nemesis();
        let core_slot = &profile.slots["core"];
        let pkgs_slot = &profile.slots["packages"];
        let mut tool = pkg("tool", PackageFormat::Zip, "packages");
        assert!(!effective_extract(pkgs_slot, &tool, None));
        assert!(effective_extract(pkgs_slot, &tool, Some(true)));
        tool.extract = true;
        assert!(effective_extract(pkgs_slot, &tool, None));
        assert!(!effective_extract(pkgs_slot, &tool, Some(false)));
        assert!(!effective_extract(core_slot, &tool, Some(true)));
        let core = pkg("nemesis-core", PackageFormat::Tar, "core");
        assert!(!effective_extract(pkgs_slot, &core, Some(true)));
    }

    #[test]
    fn a_zip_carrying_its_data_file_is_always_unpacked() {
        let profile = nemesis();
        let slot = &profile.slots["packages"];
        let mut tool = pkg("tool", PackageFormat::Zip, "packages");
        assert!(!data_in_zip(slot, &tool));
        tool.data = None;
        assert!(data_in_zip(slot, &tool));
        assert!(effective_extract(slot, &tool, None));
        assert!(
            effective_extract(slot, &tool, Some(false)),
            "the user can't turn it off"
        );
        assert!(!data_in_zip(
            &profile.slots["core"],
            &pkg("nemesis-core", PackageFormat::Tar, "core")
        ));
    }

    #[test]
    fn hostile_profiles_are_rejected() {
        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().path = "../outside".into();
        assert!(validate_profile(&profile).is_err());

        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().path = ".nempm".into();
        assert!(validate_profile(&profile).is_err());

        // A shared unpack folder is only fine when the drive holds one package.
        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().extract_dir = Some("packages/all".into());
        validate_profile(&profile).unwrap();
        profile.max_packages = None;
        assert!(validate_profile(&profile).is_err());

        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().extract_dir = Some(".nempm/{id}".into());
        assert!(validate_profile(&profile).is_err());
    }

    #[test]
    fn fat32_limit() {
        let mut profile = nemesis();
        assert_eq!(max_file_size(&profile, "fat32"), Some(FAT32_MAX_FILE));
        assert_eq!(max_file_size(&profile, "exfat"), None);
        profile.max_file_bytes = BTreeMap::from([("exfat".into(), 100)]);
        assert_eq!(max_file_size(&profile, "exfat"), Some(100));
    }

    #[test]
    fn filesystem_names_are_normalised() {
        assert_eq!(normalize_filesystem("FAT32"), "fat32");
        assert_eq!(normalize_filesystem("vfat"), "fat32");
        assert_eq!(normalize_filesystem("msdos"), "fat32");
        assert_eq!(normalize_filesystem("exFAT"), "exfat");
        assert_eq!(normalize_filesystem("NTFS"), "ntfs");
        assert_eq!(normalize_filesystem(""), "unknown");
    }

    #[test]
    fn package_validation() {
        let good = pkg("tool", PackageFormat::Zip, "packages");
        validate_package(&good).unwrap();
        let mut bad = good.clone();
        bad.sha256 = "XYZ".into();
        assert!(validate_package(&bad).is_err());
        let mut bad = good.clone();
        bad.url = "http://example.com/tool.zip".into();
        assert!(validate_package(&bad).is_err());
        let mut bad = good.clone();
        bad.core = true;
        assert!(validate_package(&bad).is_err());
        let mut bad = good.clone();
        bad.data.as_mut().unwrap().sha256 = "nope".into();
        assert!(validate_package(&bad).is_err());
    }
}
