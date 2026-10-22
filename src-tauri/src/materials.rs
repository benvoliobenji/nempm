//! The OS window material behind the app: Liquid Glass on macOS 26+,
//! vibrancy on older macOS, Mica on Windows 11, and a solid background
//! elsewhere. The frontend reads the result and sets `data-material` on
//! `<html>` so CSS can swap background and contrast tokens.

use serde::Serialize;
use tauri::WebviewWindow;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Material {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Glass,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Mica,
    Solid,
}

/// Applies the best material this OS supports. `dark` pins Mica's tint;
/// `None` follows the system.
#[allow(unused_variables)]
pub fn apply(window: &WebviewWindow, dark: Option<bool>) -> Material {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{
            LiquidGlassOptions, NSGlassEffectViewStyle, NSVisualEffectMaterial, apply_liquid_glass, apply_vibrancy,
        };
        // NSGlassEffectView is a private API; if a macOS update breaks it,
        // fall back to classic vibrancy.
        if apply_liquid_glass(window, LiquidGlassOptions::new(NSGlassEffectViewStyle::Regular)).is_ok() {
            return Material::Glass;
        }
        if apply_vibrancy(window, NSVisualEffectMaterial::Sidebar, None, None).is_ok() {
            return Material::Glass;
        }
        Material::Solid
    }
    #[cfg(target_os = "windows")]
    {
        // Mica needs Windows 11. Acrylic is skipped on Windows 10: the
        // window-vibrancy docs note it lags badly while resizing.
        if window_vibrancy::apply_mica(window, dark).is_ok() {
            return Material::Mica;
        }
        Material::Solid
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux: the compositor controls blur; use a solid background.
        Material::Solid
    }
}

/// Re-tints Mica after a theme change. Glass follows the window theme on its own.
#[allow(unused_variables)]
pub fn retint(window: &WebviewWindow, material: Material, dark: Option<bool>) {
    #[cfg(target_os = "windows")]
    if material == Material::Mica {
        let _ = window_vibrancy::apply_mica(window, dark);
    }
}
