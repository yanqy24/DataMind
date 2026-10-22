//! diag.rs — 运行环境诊断
//!
//! 提供两类检测，用于定位"界面渲染不出来"这类宿主环境故障：
//!   1. `webview_child_alive`：本进程是否还挂着 WebView2 浏览器进程
//!      （第三方输入法注入等原因可导致 WebView2 进程崩溃，宿主窗口仍在 → 表现为黑屏）
//!   2. `ime_conflict`：当前用户是否启用第三方输入法（已知与 WebView2 存在冲突）
//!
//! 非 Windows 平台一律返回"正常"，保持行为一致。

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    /// 注册表键句柄（windows-sys 0.61 中 HANDLE / HKEY 均为 *mut c_void）
    type Hkey = *mut std::ffi::c_void;

    const WEBVIEW_EXE: &str = "msedgewebview2.exe";

    /// 进程快照：[(pid, ppid, 进程名)]
    fn snapshot() -> Option<Vec<(u32, u32, String)>> {
        unsafe {
            let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut out = Vec::with_capacity(128);
            let mut more = Process32FirstW(handle, &mut entry) != 0;
            while more {
                let name = from_u16_nul(&entry.szExeFile);
                out.push((entry.th32ProcessID, entry.th32ParentProcessID, name));
                more = Process32NextW(handle, &mut entry) != 0;
            }
            CloseHandle(handle);
            Some(out)
        }
    }

    fn from_u16_nul(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 本进程（或其后代进程）是否仍在运行 WebView2 浏览器进程
    pub fn webview_child_alive(self_pid: u32) -> bool {
        let procs = match snapshot() {
            Some(p) => p,
            None => return false, // 探测失败时不误判为崩溃
        };
        let parent: HashMap<u32, u32> =
            procs.iter().map(|(pid, ppid, _)| (*pid, *ppid)).collect();

        for (pid, _, name) in &procs {
            if !name.eq_ignore_ascii_case(WEBVIEW_EXE) {
                continue;
            }
            // 向上追溯祖先链，命中本进程即视为"我们的" WebView2
            let mut cur = *pid;
            for _ in 0..8 {
                if cur == self_pid {
                    return true;
                }
                match parent.get(&cur) {
                    Some(&pp) if pp != cur => cur = pp,
                    _ => break,
                }
            }
        }
        false
    }

    /// 读取注册表字符串值
    fn reg_str(root: Hkey, path: &str, value: &str) -> Option<String> {
        unsafe {
            let path_w = to_wide(path);
            let value_w = to_wide(value);
            let mut buf = [0u16; 256];
            let mut size = (buf.len() * 2) as u32;
            let rc = RegGetValueW(
                root,
                path_w.as_ptr(),
                value_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                &mut size,
            );
            if rc != 0 {
                return None;
            }
            let len = (size / 2).saturating_sub(1).min(buf.len() as u32) as usize;
            Some(String::from_utf16_lossy(&buf[..len]))
        }
    }

    /// 判断某个键盘布局是否属于第三方输入法；是则返回其显示名
    fn layout_conflict(layout_id: &str) -> Option<String> {
        let base = format!(
            "SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts\\{}",
            layout_id
        );
        let file = reg_str(HKEY_LOCAL_MACHINE, &base, "Layout File").unwrap_or_default();
        if file.is_empty() {
            return None;
        }
        let lower = file.to_lowercase();
        // 微软自带：mscandui.dll / MSSCIPY.IME / kbdus.dll 等，均以 ms 或 kbd 开头
        if lower.starts_with("ms") || lower.starts_with("kbd") {
            return None;
        }
        let text = reg_str(HKEY_LOCAL_MACHINE, &base, "Layout Text").unwrap_or_default();
        Some(if text.is_empty() { file } else { text })
    }

    /// Windows 自带的 IME 目录（其余均视为第三方）
    const BUILTIN_IME_DIRS: [&str; 6] = ["SHARED", "IMEJP", "IMEJP10", "IMEKR", "IMETC", "EN-US"];

    /// 把第三方 IME 安装目录名转成友好名称
    fn ime_display_name(dir: &str) -> String {
        match dir.to_lowercase().as_str() {
            "sogoupy" | "sogou" => "搜狗拼音输入法".to_string(),
            "baidupinyin" | "baidu" => "百度输入法".to_string(),
            "qqpinyin" | "qqintl" => "QQ 输入法".to_string(),
            "microsoftpinyin" => "微软拼音".to_string(),
            other => format!("第三方输入法（{other}）"),
        }
    }

    /// 扫描系统 IME 目录，发现第三方输入法（现代输入法以 TSF 方式注册，
    /// 不会出现在传统键盘布局里，因此按安装目录判断）
    fn third_party_ime() -> Option<String> {
        let roots = [
            "C:\\Windows\\System32\\IME",
            "C:\\Windows\\SysWOW64\\IME",
        ];
        for root in roots {
            let entries = match std::fs::read_dir(root) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if BUILTIN_IME_DIRS
                    .iter()
                    .any(|b| name.eq_ignore_ascii_case(b))
                {
                    continue;
                }
                if !entry.path().is_dir() {
                    continue;
                }
                // 目录非空即视为已安装的输入法（.ime 常位于安装目录而非此处，
                // 例如搜狗在 SysWOW64\IME\SogouPY 下只放 broker 程序）
                let not_empty = std::fs::read_dir(entry.path())
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);
                if not_empty {
                    return Some(ime_display_name(&name));
                }
            }
        }
        None
    }

    /// 当前用户启用的输入法中是否存在第三方输入法（已知会引发 WebView2 崩溃）
    pub fn ime_conflict() -> Option<String> {
        // 1) 传统键盘布局（IME 型输入法会替换 Layout File）
        for slot in ["1", "2", "3", "4", "5"] {
            if let Some(layout) = reg_str(HKEY_CURRENT_USER, "Keyboard Layout\\Preload", slot) {
                if let Some(name) = layout_conflict(layout.trim()) {
                    return Some(name);
                }
            }
        }
        // 2) TSF 型输入法：按系统 IME 目录判断
        third_party_ime()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn webview_child_alive(_self_pid: u32) -> bool {
        true
    }
    pub fn ime_conflict() -> Option<String> {
        None
    }
}

/// 本进程是否仍挂着 WebView2 浏览器进程（false ≈ 界面已崩溃）
pub fn webview_child_alive(self_pid: u32) -> bool {
    imp::webview_child_alive(self_pid)
}

/// 是否存在已知冲突的第三方输入法；有则返回其名称
pub fn ime_conflict() -> Option<String> {
    imp::ime_conflict()
}

