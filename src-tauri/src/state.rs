//! State shared by all commands.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use pkgmgr_core::http::HttpClient;
use pkgmgr_core::index::IndexResult;
use pkgmgr_core::model::{Index, Profile};
use pkgmgr_core::progress::CancelToken;
use pkgmgr_core::{Error, Result};
use tauri::{AppHandle, Manager};

use crate::materials::Material;
use crate::settings::Settings;
use crate::updates::PendingUpdate;

pub struct AppState {
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    /// `PKGMGR_INDEX_URL`, for the local development registry. Used instead of
    /// the saved index URL but never written to settings.
    index_url_override: Option<String>,
    pub default_cache_dir: PathBuf,
    pub client: HttpClient,
    pub index: Mutex<Option<IndexResult>>,
    /// Set while anything writes to a drive. Updates never install then.
    busy: AtomicBool,
    pub cancel: CancelToken,
    pub material: Mutex<Material>,
    pub pending_update: Mutex<Option<PendingUpdate>>,
}

impl AppState {
    pub fn load(app: &AppHandle) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let config_dir = app.path().app_config_dir()?;
        let settings_path = config_dir.join("settings.json");
        let default_cache_dir = app.path().app_cache_dir()?;
        Ok(Self {
            settings: Mutex::new(Settings::load(&settings_path)),
            settings_path,
            index_url_override: std::env::var("PKGMGR_INDEX_URL").ok().filter(|u| !u.trim().is_empty()),
            default_cache_dir,
            client: pkgmgr_core::http::client()?,
            index: Mutex::new(None),
            busy: AtomicBool::new(false),
            cancel: CancelToken::new(),
            material: Mutex::new(Material::Solid),
            pending_update: Mutex::new(None),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn index_url(&self) -> String {
        self.index_url_override
            .clone()
            .unwrap_or_else(|| self.settings.lock().unwrap().index_url.clone())
    }

    pub fn cache_dir(&self) -> PathBuf {
        match &self.settings.lock().unwrap().cache_dir {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => self.default_cache_dir.clone(),
        }
    }

    pub fn index(&self) -> Result<Index> {
        self.index
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.index.clone())
            .ok_or_else(|| Error::InvalidData("The package list hasn't loaded yet. Refresh and try again.".into()))
    }

    pub fn profile(&self, id: &str) -> Result<Profile> {
        self.index()?
            .profile(id)
            .cloned()
            .ok_or_else(|| Error::UnknownProfile(id.to_string()))
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Claims the drive for one operation; released when the guard drops.
    pub fn claim(&self) -> Result<BusyGuard<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        self.cancel.reset();
        Ok(BusyGuard(&self.busy))
    }
}

pub struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
