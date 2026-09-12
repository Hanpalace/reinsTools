// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

mod db;
mod history;
mod import;
mod runlog;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![])))
        .plugin(tauri_plugin_os::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            db::test_connection,
            db::save_connection,
            db::list_connections,
            db::delete_connection,
            import::stat_import_files,
            import::preview_import,
            import::execute_import,
            import::export_import_script,
            history::list_import_history,
            history::get_history_run,
            history::add_history_run,
            history::update_history_run,
            history::delete_history_run,
            runlog::read_import_log,
            runlog::clear_import_log
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
