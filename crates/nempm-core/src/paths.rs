//! Drive-relative paths: template rendering and the safety rules every path
//! must pass before the app writes or deletes anything.
//!
//! Relative paths use `/` separators everywhere (in profiles and in the drive
//! manifest) and are only turned into OS paths by [`join`]. On Windows, std
//! adds the `\\?\` prefix itself when a path exceeds `MAX_PATH`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::{Error, Result};

static ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:[.-][a-z0-9]+)*$").unwrap());

const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Package and profile ids end up in file names, so they are held to a strict
/// charset regardless of what a profile allows.
pub fn validate_id(id: &str) -> Result<()> {
    if id.len() <= 64 && ID_RE.is_match(id) {
        Ok(())
    } else {
        Err(Error::InvalidData(format!(
            "\"{id}\" is not a valid id (lowercase letters and digits, separated by single . or -)"
        )))
    }
}

/// Checks one path component against what FAT32, exFAT and Windows accept.
pub fn validate_component(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("empty path segment".into());
    }
    if name == "." || name == ".." {
        return Err(format!("\"{name}\" is not allowed in a path"));
    }
    if name.len() > 255 {
        return Err(format!("\"{name}\" is longer than 255 characters"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control())
    {
        return Err(format!("\"{name}\" contains a character drives can't store ({c:?})"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(format!("\"{name}\" ends with a dot or space"));
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if WINDOWS_DEVICE_NAMES.iter().any(|d| d.eq_ignore_ascii_case(stem)) {
        return Err(format!("\"{name}\" is a reserved device name"));
    }
    Ok(())
}

/// A drive-relative path. Empty means the drive root.
pub fn validate_rel(rel: &str) -> Result<()> {
    if rel.is_empty() {
        return Ok(());
    }
    if rel.starts_with('/') || rel.contains('\\') {
        return Err(Error::InvalidData(format!(
            "\"{rel}\" must be relative and use / separators"
        )));
    }
    for part in rel.split('/') {
        validate_component(part).map_err(|msg| Error::InvalidData(format!("Path \"{rel}\": {msg}")))?;
    }
    Ok(())
}

/// Rejects paths whose first segment is one of the profile's reserved names
/// (always including `.nempm`, the app's own folder).
pub fn check_not_reserved(rel: &str, reserved: &[String]) -> Result<()> {
    let first = rel.split('/').next().unwrap_or("");
    if crate::manifest::is_app_dir(first) || reserved.iter().any(|r| r.eq_ignore_ascii_case(first)) {
        return Err(Error::Profile(format!(
            "\"{rel}\" is inside the reserved folder \"{first}\""
        )));
    }
    Ok(())
}

/// Values substituted into `{id}`, `{version}` and `{format}`.
pub struct Vars<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub format: &'a str,
}

/// Renders a profile path template. Unknown placeholders and stray braces are
/// errors, so a typo in a profile can't silently write to the wrong place.
pub fn render(template: &str, vars: &Vars<'_>) -> Result<String> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(start) = rest.find(['{', '}']) {
        if rest.as_bytes()[start] == b'}' {
            return Err(Error::Profile(format!("Unmatched }} in \"{template}\"")));
        }
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| Error::Profile(format!("Unclosed {{ in \"{template}\"")))?;
        out.push_str(match &after[..end] {
            "id" => vars.id,
            "version" => vars.version,
            "format" => vars.format,
            other => {
                return Err(Error::Profile(format!(
                    "Unknown placeholder {{{other}}} in \"{template}\""
                )));
            }
        });
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Joins a validated relative path onto the drive root.
pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        path.push(part);
    }
    path
}

/// Joins two relative paths, treating an empty left side as the root.
pub fn rel_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The parent of a relative path, or `None` at the root.
pub fn rel_parent(rel: &str) -> Option<&str> {
    rel.rsplit_once('/').map(|(parent, _)| parent)
}

/// The last segment of a URL's path, percent-decoded: the asset's file name.
pub fn file_name_from_url(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw).map_err(|e| Error::InvalidData(format!("Bad URL {raw}: {e}")))?;
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::InvalidData(format!("URL {raw} has no file name")))?;
    let decoded = percent_decode(last);
    validate_component(&decoded).map_err(Error::InvalidData)?;
    Ok(decoded)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turns a path inside the drive back into a `/`-separated relative path.
pub fn to_rel(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars<'static> {
        Vars {
            id: "tool",
            version: "1.2.0",
            format: "zip",
        }
    }

    #[test]
    fn renders_placeholders() {
        assert_eq!(render("packages/{id}.zip", &vars()).unwrap(), "packages/tool.zip");
        assert_eq!(render("{id}-{version}.{format}", &vars()).unwrap(), "tool-1.2.0.zip");
        assert_eq!(render("NEMESIS.data", &vars()).unwrap(), "NEMESIS.data");
        assert_eq!(render("", &vars()).unwrap(), "");
    }

    #[test]
    fn rejects_bad_templates() {
        assert!(render("{name}.zip", &vars()).is_err());
        assert!(render("{id.zip", &vars()).is_err());
        assert!(render("id}.zip", &vars()).is_err());
    }

    #[test]
    fn rejects_traversal_and_unsafe_names() {
        for bad in [
            "../x",
            "a/../b",
            "/abs",
            "a\\b",
            "a//b",
            "a/./b",
            "CON",
            "nul.txt",
            "x:y",
            "trailing.",
        ] {
            assert!(validate_rel(bad).is_err(), "{bad} should be rejected");
        }
        for good in ["", "packages", "packages/tool.zip", "NEMESIS.data", "a b/c-d_e.f"] {
            assert!(validate_rel(good).is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn ids_are_strict() {
        assert!(validate_id("example-tool").is_ok());
        assert!(validate_id("my.tool").is_ok());
        for bad in ["Tool", "my..tool", ".tool", "tool.", "-tool", "con.", "a.-b"] {
            assert!(validate_id(bad).is_err(), "{bad} should be rejected");
        }
        assert!(validate_id(&"a".repeat(65)).is_err());
        assert!(validate_id("../x").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("").is_err());
    }

    #[test]
    fn reserved_folders_are_blocked() {
        assert!(check_not_reserved(".nempm/x", &[]).is_err());
        assert!(check_not_reserved(".PKGMGR", &[]).is_err());
        assert!(check_not_reserved("system/x", &["System".into()]).is_err());
        assert!(check_not_reserved("packages/x", &[]).is_ok());
    }

    #[test]
    fn file_name_from_release_url() {
        let url = "https://github.com/o/r/releases/download/v1.0.0/my%20tool.zip";
        assert_eq!(file_name_from_url(url).unwrap(), "my tool.zip");
        assert!(file_name_from_url("https://github.com/").is_err());
        assert!(file_name_from_url("https://x/a/..%2F..%2Fevil").is_err());
    }

    #[test]
    fn join_and_back() {
        let root = Path::new("drive");
        let joined = join(root, "packages/tool.zip");
        assert_eq!(to_rel(root, &joined).unwrap(), "packages/tool.zip");
        assert_eq!(rel_join("", "NEMESIS.data"), "NEMESIS.data");
        assert_eq!(rel_parent("packages/tool.zip"), Some("packages"));
        assert_eq!(rel_parent("NEMESIS.data"), None);
    }
}
