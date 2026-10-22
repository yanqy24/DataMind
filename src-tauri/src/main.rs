//! DataMind — 与数据对话的桌面客户端
//! 防止 Windows 发行版弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    datamind_lib::run()
}
