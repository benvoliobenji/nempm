//! Tauri commands: the only way the UI reaches the network or a drive.
//!
//! Anything that touches a drive runs on a blocking thread so a slow stick
//! can't freeze the window, and every write claims [`AppState::claim`] so two
//! operations never interleave and an app update never lands mid-write.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pkgmgr_core::cache::{self, Downloader};
use pkgmgr_core::check::{self, CheckReport, DriveState, FixAction};
use pkgmgr_core::drives::{self, DriveInfo};
use pkgmgr_core::index::{self, IndexResult};
use pkgmgr_core::install::{self, InstallOptions, InstallOutcome, InstallPlan, InstallRequest};
use pkgmgr_core::progress::{Progress, ProgressFn};
use pkgmgr_core::{Error, Result, eject as eject_mod, manifest, uninstall as uninstall_mod};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use ts_rs::TS;

use crate::materials::{self, Material};
use crate::settings::{Settings, Theme};
use crate::state::AppState;
use crate::updates::{self, UpdateInfo};

/// Runs blocking drive I/O off the main thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| Error::InvalidData(format!("Background task failed: {e}")))?
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppInfo {
    pub version: String,
    /// `windows`, `macos` or `linux`.
    pub platform: String,
    pub material: Material,
    pub updater_configured: bool,
    pub default_index_url: String,
}

#[tauri::command]
pub fn app_info(app: AppHandle, state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        platform: std::env::consts::OS.to_string(),
        material: *state.material.lock().unwrap(),
        updater_configured: updates::configured(&app),
        default_index_url: crate::settings::DEFAULT_INDEX_URL.to_string(),
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings()
}

pub fn theme_flag(theme: Theme) -> Option<bool> {
    match theme {
        Theme::System => None,
        Theme::Light => Some(false),
        Theme::Dark => Some(true),
    }
}

pub fn apply_theme(app: &AppHandle, theme: Theme, material: Material) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let tauri_theme = match theme {
        Theme::System => None,
        Theme::Light => Some(tauri::Theme::Light),
        Theme::Dark => Some(tauri::Theme::Dark),
    };
    let w = window.clone();
    let _ = window.run_on_main_thread(move || {
        let _ = w.set_theme(tauri_theme);
        materials::retint(&w, material, theme_flag(theme));
    });
}

#[tauri::command]
pub fn set_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> Result<Settings> {
    if settings.index_url.trim().is_empty() {
        return Err(Error::InvalidData("The index URL can't be empty".into()));
    }
    pkgmgr_core::http::check_url(&settings.index_url)?;
    let previous = state.settings();
    settings
        .save(&state.settings_path)
        .map_err(pkgmgr_core::error::io_ctx("saving settings"))?;
    *state.settings.lock().unwrap() = settings.clone();
    if previous.theme != settings.theme {
        apply_theme(&app, settings.theme, *state.material.lock().unwrap());
    }
    if previous.index_url != settings.index_url || previous.cache_dir != settings.cache_dir {
        // Forget the old registry's packages until the new one loads.
        *state.index.lock().unwrap() = index::cached(&state.cache_dir(), &state.index_url());
    }
    Ok(settings)
}

/// The cached index, instantly, so the UI has something to show at launch.
#[tauri::command]
pub fn cached_index(state: State<'_, AppState>) -> Option<IndexResult> {
    let result = index::cached(&state.cache_dir(), &state.index_url())?;
    let mut slot = state.index.lock().unwrap();
    if slot.is_none() {
        *slot = Some(result.clone());
    }
    Some(result)
}

#[tauri::command]
pub async fn fetch_index(state: State<'_, AppState>) -> Result<IndexResult> {
    let url = state.index_url();
    let result = index::fetch(&state.client, &url, &state.cache_dir()).await?;
    *state.index.lock().unwrap() = Some(result.clone());
    Ok(result)
}

#[tauri::command]
pub async fn list_drives(state: State<'_, AppState>) -> Result<Vec<DriveInfo>> {
    let all = state.settings().show_all_drives;
    blocking(move || Ok(drives::list_drives(all))).await
}

#[tauri::command]
pub async fn inspect_folder(path: String) -> Result<DriveInfo> {
    blocking(move || drives::inspect_path(Path::new(&path))).await
}

#[tauri::command]
pub async fn read_drive(state: State<'_, AppState>, path: String) -> Result<DriveState> {
    let index = state.index().ok();
    blocking(move || Ok(check::read_drive(Path::new(&path), index.as_ref()))).await
}

#[tauri::command]
pub async fn check_drive(state: State<'_, AppState>, path: String, profile: String) -> Result<CheckReport> {
    let profile = state.profile(&profile)?;
    let index = state.index().ok();
    remember_profile(&state, &profile.id);
    blocking(move || {
        let root = PathBuf::from(&path);
        let drive = drives::inspect_path(&root)?;
        Ok(check::check_drive(&root, &drive, &profile, index.as_ref()))
    })
    .await
}

fn remember_profile(state: &AppState, id: &str) {
    let mut settings = state.settings.lock().unwrap();
    if settings.last_profile.as_deref() != Some(id) {
        settings.last_profile = Some(id.to_string());
        let _ = settings.save(&state.settings_path);
    }
}

#[tauri::command]
pub async fn init_drive(state: State<'_, AppState>, path: String, profile: String) -> Result<CheckReport> {
    fix_drive(state, path, profile, vec![FixAction::Initialize]).await
}

/// Applies safe fixes, then re-runs the check.
#[tauri::command]
pub async fn fix_drive(
    state: State<'_, AppState>,
    path: String,
    profile: String,
    fixes: Vec<FixAction>,
) -> Result<CheckReport> {
    let _guard = state.claim()?;
    let profile = state.profile(&profile)?;
    let index = state.index().ok();
    blocking(move || {
        let root = PathBuf::from(&path);
        check::fix_drive(&root, &profile, index.as_ref(), &fixes)?;
        let drive = drives::inspect_path(&root)?;
        Ok(check::check_drive(&root, &drive, &profile, index.as_ref()))
    })
    .await
}

#[tauri::command]
pub async fn plan_install(
    state: State<'_, AppState>,
    path: String,
    profile: String,
    ids: Vec<String>,
    options: InstallOptions,
) -> Result<InstallPlan> {
    let profile = state.profile(&profile)?;
    let index = state.index()?;
    blocking(move || {
        let root = PathBuf::from(&path);
        let m = manifest::load(&root)?;
        install::plan(&root, &index, &profile, &m, &ids, &options)
    })
    .await
}

/// Installs packages (dependencies first), emitting `progress` events.
#[tauri::command]
pub async fn install(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    profile: String,
    ids: Vec<String>,
    options: InstallOptions,
) -> Result<InstallOutcome> {
    let _guard = state.claim()?;
    let profile = state.profile(&profile)?;
    let index = state.index()?;
    let root = PathBuf::from(&path);
    let drive = {
        let r = root.clone();
        blocking(move || drives::inspect_path(&r)).await?
    };
    let emitter = app.clone();
    let sink: ProgressFn = Arc::new(move |p: Progress| {
        let _ = emitter.emit("progress", p);
    });
    let downloader = Downloader::new(state.client.clone(), state.cache_dir(), state.cancel.clone());
    install::install(
        &downloader,
        sink,
        InstallRequest {
            root: &root,
            drive: &drive,
            profile: &profile,
            index: &index,
            ids: &ids,
            options: &options,
        },
    )
    .await
}

/// Stops the running install after the current chunk. Partial downloads are
/// kept so the next attempt resumes.
#[tauri::command]
pub fn cancel_install(state: State<'_, AppState>) {
    state.cancel.cancel();
}

#[tauri::command]
pub async fn uninstall(state: State<'_, AppState>, path: String, id: String, force: bool) -> Result<DriveState> {
    let _guard = state.claim()?;
    let index = state.index().ok();
    blocking(move || {
        let root = PathBuf::from(&path);
        uninstall_mod::uninstall(&root, &id, force)?;
        Ok(check::read_drive(&root, index.as_ref()))
    })
    .await
}

#[tauri::command]
pub async fn eject(state: State<'_, AppState>, path: String) -> Result<()> {
    let _guard = state.claim()?;
    blocking(move || {
        let root = PathBuf::from(&path);
        let drive = drives::list_drives(true)
            .into_iter()
            .find(|d| Path::new(&d.mount_point) == root)
            .map(Ok)
            .unwrap_or_else(|| drives::inspect_path(&root))?;
        eject_mod::eject(&drive)
    })
    .await
}

#[tauri::command]
pub fn is_busy(state: State<'_, AppState>) -> bool {
    state.is_busy()
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CacheInfo {
    pub path: String,
    #[ts(type = "number")]
    pub bytes: u64,
}

#[tauri::command]
pub async fn cache_info(state: State<'_, AppState>) -> Result<CacheInfo> {
    let dir = state.cache_dir();
    blocking(move || {
        Ok(CacheInfo {
            bytes: cache::size(&dir),
            path: dir.to_string_lossy().into_owned(),
        })
    })
    .await
}

#[tauri::command]
pub async fn clear_cache(state: State<'_, AppState>) -> Result<u64> {
    let _guard = state.claim()?;
    let dir = state.cache_dir();
    blocking(move || cache::clear(&dir)).await
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>> {
    updates::check(&app).await
}

#[tauri::command]
pub async fn download_update(app: AppHandle) -> Result<UpdateInfo> {
    updates::download(&app).await
}

#[tauri::command]
pub fn apply_update(app: AppHandle) -> Result<()> {
    updates::apply(&app)
}
