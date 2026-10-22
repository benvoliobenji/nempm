//! App self-update: check on launch and every 6 hours (driven by the
//! frontend), download in the background, install only on the user's
//! confirmation and never while a drive is being written.

use pkgmgr_core::{Error, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};
use ts_rs::TS;

use crate::settings::UpdateChannel;
use crate::state::AppState;

/// Beta builds: `latest.json` for the newest tag of any kind, published to the
/// app repo's GitHub Pages by the release workflow.
const BETA_ENDPOINT: &str = "https://OWNER.github.io/pkgmgr-app/updater/beta/latest.json";

pub struct PendingUpdate {
    update: Update,
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    pub downloaded: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateProgress {
    #[ts(type = "number")]
    pub bytes_done: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub bytes_total: Option<u64>,
}

fn info(p: &PendingUpdate) -> UpdateInfo {
    UpdateInfo {
        version: p.update.version.clone(),
        current_version: p.update.current_version.clone(),
        notes: p.update.body.clone(),
        date: p.update.date.map(|d| d.to_string()),
        downloaded: p.bytes.is_some(),
    }
}

fn update_error(e: tauri_plugin_updater::Error) -> Error {
    Error::Network(format!("Update failed: {e}"))
}

/// Whether `plugins.updater.pubkey` is set in tauri.conf.json. Until the
/// signing key is generated, update checks are skipped.
pub fn configured(app: &AppHandle) -> bool {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty())
}

pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>> {
    let state = app.state::<AppState>();
    let channel = state.settings().update_channel;
    if !configured(app) || channel == UpdateChannel::Off {
        return Ok(None);
    }
    let mut builder = app.updater_builder();
    if channel == UpdateChannel::Beta {
        let url = Url::parse(BETA_ENDPOINT).map_err(|e| Error::InvalidData(e.to_string()))?;
        builder = builder.endpoints(vec![url]).map_err(update_error)?;
    }
    let updater = builder.build().map_err(update_error)?;
    let Some(update) = updater.check().await.map_err(update_error)? else {
        *state.pending_update.lock().unwrap() = None;
        return Ok(None);
    };
    let mut pending = state.pending_update.lock().unwrap();
    // Keep an already-downloaded copy of the same version.
    if pending.as_ref().is_none_or(|p| p.update.version != update.version) {
        *pending = Some(PendingUpdate { update, bytes: None });
    }
    Ok(pending.as_ref().map(info))
}

/// Downloads the pending update in the background, emitting `update-progress`.
pub async fn download(app: &AppHandle) -> Result<UpdateInfo> {
    let state = app.state::<AppState>();
    let update = {
        let pending = state.pending_update.lock().unwrap();
        let p = pending
            .as_ref()
            .ok_or_else(|| Error::InvalidData("No update to download".into()))?;
        if p.bytes.is_some() {
            return Ok(info(p));
        }
        p.update.clone()
    };
    let emitter = app.clone();
    let mut done = 0u64;
    let bytes = update
        .download(
            move |chunk, total| {
                done += chunk as u64;
                let _ = emitter.emit(
                    "update-progress",
                    UpdateProgress {
                        bytes_done: done,
                        bytes_total: total,
                    },
                );
            },
            || {},
        )
        .await
        .map_err(update_error)?;
    let mut pending = state.pending_update.lock().unwrap();
    let p = pending
        .as_mut()
        .filter(|p| p.update.version == update.version)
        .ok_or_else(|| Error::InvalidData("The update changed while downloading; check again".into()))?;
    p.bytes = Some(bytes);
    Ok(info(p))
}

/// Installs the downloaded update and restarts. Refused while a drive write
/// is in progress.
pub fn apply(app: &AppHandle) -> Result<()> {
    let state = app.state::<AppState>();
    let _guard = state.claim()?;
    let pending = state.pending_update.lock().unwrap().take();
    let Some(PendingUpdate {
        update,
        bytes: Some(bytes),
    }) = pending
    else {
        return Err(Error::InvalidData("The update hasn't finished downloading".into()));
    };
    update.install(bytes).map_err(update_error)?;
    app.restart();
}
