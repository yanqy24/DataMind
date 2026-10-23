//! lib.rs — 应用装配：状态 / 插件 / 命令注册

mod agent;
mod commands;
mod diag;
mod error;
mod guard;
mod llm;
mod models;
mod mysql;
mod store;
mod tunnel;
mod watchdog;

use tauri::Manager;

/// 供诊断工具（examples/）复用真实的解码逻辑
#[doc(hidden)]
pub use mysql::decode_diag;

/// 应用数据目录（setup 时记录，供 panic 钩子等无 AppHandle 场景使用）
pub static DATA_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // 记录数据目录并安装 panic 钩子：release 采用 panic=abort，进程将直接终止，
            // 系统侧只有一条无堆栈的 WER 记录；钩子在终止前把 panic 现场写入 panic.log。
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = DATA_DIR.set(dir);
            }
            install_panic_hook();
            let state = commands::AppState::build(app)?;
            app.manage(state);
            // 界面渲染守护：WebView2 崩溃时自动重建并通知前端
            watchdog::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::conn_list,
            commands::conn_save,
            commands::conn_delete,
            commands::conn_test,
            commands::conn_sync,
            commands::memory_get,
            commands::table_detail,
            commands::sql_execute,
            commands::sql_cancel,
            commands::sql_generate,
            commands::model_list,
            commands::model_save,
            commands::model_delete,
            commands::model_activate,
            commands::log_list,
            commands::log_clear,
            commands::settings_get,
            commands::settings_set,
            commands::app_version,
            commands::diag_log,
            commands::env_diag,
            commands::webview_restart,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    // 重建窗口期间（旧窗口已关、新窗口未起）阻止应用退出
    app.run(|_handle, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if watchdog::is_restarting() {
                api.prevent_exit();
            }
        }
    });
}

/// panic 钩子：把 panic 信息（含位置与消息）追加写入 {data_dir}/panic.log
/// （panic=abort 下进程随后终止，这是唯一的现场记录）
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!(
            "[{}] PANIC {} | {:?}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            info,
            std::thread::current()
        );
        // 尽力写文件；失败时至少输出到 stderr
        if let Some(dir) = DATA_DIR.get() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("panic.log"))
            {
                let _ = f.write_all(msg.as_bytes());
            }
        }
        eprint!("{msg}");
    }));
}
