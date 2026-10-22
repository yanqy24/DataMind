//! watchdog.rs — 界面渲染守护（Windows）
//!
//! 场景：第三方输入法（如某些版本的搜狗输入法）注入 WebView2 进程后，
//! 浏览器进程会在初始化阶段崩溃，宿主窗口仍在 → 用户看到一片黑屏且无提示。
//!
//! 处理：
//!   1. 后台线程周期性探测 WebView2 子进程是否存活，或是否出现新的崩溃转储
//!      （崩溃转储可覆盖"启动即崩、从未存活"的情况）
//!   2. 确认崩溃 → 自动重建窗口（最多 2 次）
//!   3. 自动重建仍失败 → 弹出原生消息框，给出可操作的修复建议（含输入法名称），
//!      因为此时 WebView 内的页面 JS 已无法运行，前端提示不可达
//! 非 Windows 平台为空实现。

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(windows)]
use std::time::{Duration, SystemTime};

#[cfg(windows)]
use serde_json::json;
#[cfg(windows)]
use tauri::{AppHandle, Emitter, Manager};

/// 重建窗口过程中需要阻止"最后一个窗口关闭即退出"
#[cfg(windows)]
static RESTARTING: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);

/// 供 RunEvent::ExitRequested 询问：当前是否在重建窗口
#[cfg(windows)]
pub fn is_restarting() -> bool {
    RESTARTING.load(Ordering::SeqCst)
}

#[cfg(windows)]
const MAX_AUTO_RESTART: u32 = 2;

/// 把守护事件写入数据目录 watchdog.log（页面无法渲染时，这是唯一可查的线索）
#[cfg(windows)]
fn log(app: &AppHandle, line: &str) {
    use std::io::Write;
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("watchdog.log"))
        {
            let _ = writeln!(
                f,
                "[{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                line
            );
        }
    }
}

#[cfg(windows)]
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let self_pid = std::process::id();
        let started = SystemTime::now();
        log(&app, &format!(
            "watchdog started pid={} ime={}",
            self_pid,
            crate::diag::ime_conflict().unwrap_or_else(|| "none".to_string())
        ));
        // 给 WebView2 留出初始化时间，避免启动瞬间误判
        std::thread::sleep(Duration::from_secs(6));
        let mut miss = 0u32;
        let mut ever_alive = false;
        loop {
            if app.get_webview_window("main").is_none() {
                break; // 窗口已关闭，应用正在退出
            }
            let alive = crate::diag::webview_child_alive(self_pid);
            if alive {
                ever_alive = true;
            }
            // 崩溃信号：子进程消失（曾存活过），或出现本次启动后的新崩溃转储
            let dumped = has_new_dump(&app, started);
            if !alive && (ever_alive || dumped) {
                miss += 1;
                if miss >= 2 {
                    // 连续两次确认，避免探测抖动
                    on_crash(&app, true);
                    miss = 0;
                    std::thread::sleep(Duration::from_secs(5)); // 给重建留出时间
                }
            } else {
                miss = 0;
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// 本次启动之后是否产生过新的 WebView2 崩溃转储
#[cfg(windows)]
fn has_new_dump(app: &AppHandle, started: SystemTime) -> bool {
    let dir = match app.path().app_local_data_dir() {
        Ok(d) => d.join("EBWebView").join("Crashpad").join("reports"),
        Err(_) => return false,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    entries.flatten().any(|e| {
        if e.path().extension().and_then(|x| x.to_str()) != Some("dmp") {
            return false;
        }
        e.metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t > started)
            .unwrap_or(false)
    })
}

/// 崩溃处理：自动重建（有限次）；超限后弹原生提示并交由用户决定
#[cfg(windows)]
fn on_crash(app: &AppHandle, auto: bool) {
    let n = ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    let ime = crate::diag::ime_conflict();
    log(
        app,
        &format!(
            "webview crashed (attempt {}), ime={}, auto restart={}",
            n + 1,
            ime.clone().unwrap_or_else(|| "none".to_string()),
            auto && n < MAX_AUTO_RESTART
        ),
    );
    let _ = app.emit(
        "webview:crashed",
        json!({ "autoRestart": auto && n < MAX_AUTO_RESTART, "attempt": n + 1, "ime": ime }),
    );
    if auto && n < MAX_AUTO_RESTART {
        match rebuild(app) {
            Ok(()) => {
                log(app, "window rebuilt");
                return;
            }
            Err(e) => log(app, &format!("rebuild failed: {e}")),
        }
    }
    // 自动重建已用尽：页面 JS 无法触达前端，改用原生消息框
    loop {
        let retry = native_alert(&ime);
        if !retry {
            break;
        }
        reset_attempts();
        if rebuild(app).is_ok() {
            break;
        }
    }
}

/// 原生消息框：说明崩溃原因并给出修复建议；返回 true 表示用户选择重试
#[cfg(windows)]
fn native_alert(ime: &Option<String>) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_RETRYCANCEL, IDRETRY,
    };
    let mut text = String::from(
        "DataMind 的界面渲染进程（WebView2）已崩溃，无法恢复。\n\n\
         最常见原因是第三方输入法与 WebView2 冲突。",
    );
    if let Some(name) = ime {
        text.push_str(&format!("\n\n当前检测到第三方输入法：{name}"));
    }
    text.push_str(
        "\n\n建议：\n\
         1. 临时切换为微软拼音（Win + 空格 或 语言栏）；\n\
         2. 点击【重试(R)】重建界面；\n\
         3. 若反复失败，请更新或卸载该输入法后重启 DataMind。",
    );
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let cap_w: Vec<u16> = "DataMind 界面渲染异常"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { MessageBoxW(std::ptr::null_mut(), text_w.as_ptr(), cap_w.as_ptr(), MB_ICONERROR | MB_RETRYCANCEL) == IDRETRY }
}

/// 销毁并重建主窗口（重建期间阻止应用退出）
///
/// 说明：曾尝试"重建前把本进程键盘布局切为英文/微软拼音，使浏览器进程继承"，
/// 实测无效——浏览器进程的默认输入语言取自系统配置（HKCU\Keyboard Layout\Preload），
/// 且第三方输入法以 TSF 方式加载，与键盘布局无关。故此处不再改动用户输入法状态，
/// 只做纯重建，避免给用户带来副作用。
#[cfg(windows)]
pub fn rebuild(app: &AppHandle) -> Result<(), String> {
    RESTARTING.store(true, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.close();
    }
    std::thread::sleep(Duration::from_millis(400));
    let built =
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
            .title("DataMind")
            .inner_size(1280.0, 820.0)
            .min_inner_size(940.0, 620.0)
            .build()
            .map_err(|e| e.to_string())?;
    let _ = built.show();
    RESTARTING.store(false, Ordering::SeqCst);
    Ok(())
}

/// 手动重建界面时重置自动重试计数，让用户始终能再试一次
#[cfg(windows)]
pub fn reset_attempts() {
    ATTEMPTS.store(0, Ordering::SeqCst);
}

#[cfg(not(windows))]
pub fn spawn(_app: tauri::AppHandle) {}
#[cfg(not(windows))]
pub fn is_restarting() -> bool {
    false
}
