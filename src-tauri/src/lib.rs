mod commands;
mod materials;
mod settings;
mod state;
mod updates;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second copy writing to the same drive would corrupt it; focus the
        // running one instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let state = state::AppState::load(app.handle())?;
            let theme = state.settings().theme;
            let window = app
                .get_webview_window("main")
                .ok_or("the main window is missing from tauri.conf.json")?;
            let material = materials::apply(&window, commands::theme_flag(theme));
            *state.material.lock().unwrap() = material;
            app.manage(state);
            commands::apply_theme(app.handle(), theme, material);
            window.show()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_settings,
            commands::set_settings,
            commands::cached_index,
            commands::fetch_index,
            commands::list_drives,
            commands::inspect_folder,
            commands::read_drive,
            commands::check_drive,
            commands::init_drive,
            commands::fix_drive,
            commands::plan_install,
            commands::install,
            commands::cancel_install,
            commands::uninstall,
            commands::eject,
            commands::is_busy,
            commands::cache_info,
            commands::clear_cache,
            commands::check_update,
            commands::download_update,
            commands::apply_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
