//! error.rs — 统一错误类型（序列化为前端可读结构）

use serde::Serialize;

/// 前端拿到的错误结构：{ code, message, hint }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    /// 连接级故障（服务端 wait_timeout / 防火墙 / SSH 隧道中断等）：可安全重建连接后重试。
    /// 仅后端使用，不序列化给前端。
    #[serde(skip)]
    pub conn_broken: bool,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            hint: None,
            conn_broken: false,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 标记为连接级故障
    pub fn as_conn_broken(mut self) -> Self {
        self.conn_broken = true;
        self
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<mysql_async::Error> for AppError {
    fn from(e: mysql_async::Error) -> Self {
        if let mysql_async::Error::Server(se) = &e {
            return Self::new(&se.code.to_string(), se.message.clone())
                .with_hint(hint_for_mysql_error(se.code, &se.message));
        }
        let err = Self::new("MYSQL", e.to_string());
        if is_conn_broken(&e) {
            // 连接已被单方面断开（闲置超时 / 网络中断 / 隧道断开）：上层可重建连接重试
            err.as_conn_broken().with_hint(
                "连接已断开（多为长时间闲置后服务端或网络侧关闭），已自动重建连接重试；若反复失败请检查网络 / SSH 隧道",
            )
        } else {
            err
        }
    }
}

/// 判断 mysql 错误是否属于"连接已断开"（可安全重连重试）
fn is_conn_broken(e: &mysql_async::Error) -> bool {
    match e {
        // "Input/output error: connection closed" 等 IO 层错误
        mysql_async::Error::Io(_) => true,
        mysql_async::Error::Driver(d) => {
            matches!(d, mysql_async::DriverError::ConnectionClosed)
        }
        _ => false,
    }
}

impl From<tokio::time::error::Elapsed> for AppError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        Self::new("TIMEOUT", "操作超时")
    }
}

impl From<tauri::Error> for AppError {
    fn from(e: tauri::Error) -> Self {
        Self::new("TAURI", e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::new("IO", e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        Self::new("LLM_HTTP", e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::new("JSON", e.to_string())
    }
}

/// 针对 MySQL 常见错误给出人话提示（供「快速修复」参考）
fn hint_for_mysql_error(code: u16, msg: &str) -> String {
    match code {
        1045 => "用户名或密码错误".into(),
        1049 => "数据库不存在，请确认库名或刷新结构记忆".into(),
        1054 => "字段不存在：建议结合左侧表结构核对字段名，或点击「快速修复 SQL」".into(),
        1064 => "SQL 语法错误：建议在编辑窗口中检查后再运行".into(),
        1146 => "表不存在：建议刷新结构记忆后核对表名".into(),
        2003 => "无法连接到 MySQL 服务器：请检查主机 / 端口 / 网络或 SSH 隧道配置".into(),
        2013 => "连接中断：网络或 SSH 隧道可能不稳定".into(),
        _ => {
            if msg.contains("timed out") {
                "查询超时：建议增加过滤条件（WHERE）或减少返回列".into()
            } else {
                "可点击「快速修复 SQL」，让模型结合错误信息自动修复".into()
            }
        }
    }
}
