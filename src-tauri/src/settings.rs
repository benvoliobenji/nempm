//! User settings, stored as JSON in the OS config folder.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where the registry's CI (benvoliobenji/nempm_reg) publishes the index.
pub const DEFAULT_INDEX_URL: &str = "https://benvoliobenji.github.io/nempm_reg/index.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
    /// No automatic checks.
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct Settings {
    pub theme: Theme,
    pub index_url: String,
    /// Overrides the OS cache folder for downloaded packages.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cache_dir: Option<String>,
    pub update_channel: UpdateChannel,
    /// List fixed drives too (USB hard drives often report as fixed).
    pub show_all_drives: bool,
    /// macOS asks for permission on the first write to a removable volume;
    /// the app explains that once before the first install.
    pub write_permission_explained: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_profile: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            index_url: DEFAULT_INDEX_URL.to_string(),
            cache_dir: None,
            update_channel: UpdateChannel::Stable,
            show_all_drives: false,
            write_permission_explained: false,
            last_profile: None,
        }
    }
}

impl Settings {
    /// Reads settings, falling back to defaults if the file is missing or
    /// damaged.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        nempm_core::fsutil::write_atomic(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_partial_files_use_defaults() {
        let dir = std::env::temp_dir().join(format!("nempm-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = fs::remove_file(&path);
        assert_eq!(Settings::load(&path).theme, Theme::System);

        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, br#"{"theme":"dark"}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.theme, Theme::Dark);
        assert_eq!(s.update_channel, UpdateChannel::Stable);

        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).theme, Theme::Dark);
        let _ = fs::remove_dir_all(dir);
    }
}
