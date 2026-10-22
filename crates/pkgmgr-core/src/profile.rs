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
            let dir = paths::render(&template, &v)?;
            check_path(profile, &dir)?;
            if dir.is_empty() {
                return Err(Error::Profile(format!(
                    "Slot \"{slot_name}\" would unpack into the drive root"
                )));
            }
            Some(dir)
        }
    };

    Ok(Placement {
        slot: slot_name.to_string(),
        file,
        extract_dir,
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
        ExtractPolicy::Optional => requested.unwrap_or(pkg.extract),
    }
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
        if slot.extract != ExtractPolicy::Never {
            let template = slot
                .extract_dir
                .clone()
                .unwrap_or_else(|| paths::rel_join(&slot.path, "{id}"));
            if !template.contains("{id}") {
                return Err(Error::InvalidData(format!(
                    "Profile {}: slot \"{name}\" must unpack into a folder containing {{id}}",
                    profile.id
                )));
            }
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
    for dep in &pkg.deps {
        paths::validate_id(dep)?;
    }
    crate::http::check_url(&pkg.url)?;
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
            slot: slot.into(),
            profiles: vec!["nemesis".into()],
            deps: vec![],
            homepage: None,
        }
    }

    #[test]
    fn nemesis_layout() {
        let profile = nemesis();
        validate_profile(&profile).unwrap();

        let core = placement(&profile, &pkg("nemesis-core", PackageFormat::Tar, "core")).unwrap();
        assert_eq!(core.file, "NEMESIS.data");
        assert_eq!(core.extract_dir, None);

        let tool = placement(&profile, &pkg("tool", PackageFormat::Zip, "packages")).unwrap();
        assert_eq!(tool.file, "packages/tool.zip");
        assert_eq!(tool.extract_dir.as_deref(), Some("packages/tool"));
        assert_eq!(tool.target(false), "packages/tool.zip");
        assert_eq!(tool.target(true), "packages/tool");
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
    fn hostile_profiles_are_rejected() {
        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().path = "../outside".into();
        assert!(validate_profile(&profile).is_err());

        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().path = ".pkgmgr".into();
        assert!(validate_profile(&profile).is_err());

        let mut profile = nemesis();
        profile.slots.get_mut("packages").unwrap().extract_dir = Some("packages/all".into());
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
    }
}
