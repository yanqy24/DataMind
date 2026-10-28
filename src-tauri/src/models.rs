//! models.rs — 前后端共享数据结构（serde 命名与前端 TS 类型一致：camelCase）

use serde::{Deserialize, Serialize};

/* ---------- 连接 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: String, // "password" | "key"
    pub secret: String,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: "password".into(),
            secret: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnConfig {
    pub id: String,
    pub name: String,
    /// 目前仅 "MySQL"，为后续拓展预留
    #[serde(rename = "type")]
    pub db_type: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub ssh: SshConfig,
}

impl ConnConfig {
    pub fn sanitized(&self) -> Self {
        let mut c = self.clone();
        c.password = if c.password.is_empty() { String::new() } else { "********".into() };
        c.ssh.secret = if c.ssh.secret.is_empty() { String::new() } else { "********".into() };
        c
    }
}

/// 单表详情（右键「查看建表语句」实时读取，不走本地记忆）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDetail {
    pub ddl: String,
    /// 取样：行 -> {列名: 值}
    pub sample: Vec<serde_json::Map<String, serde_json::Value>>,
}

/* ---------- 结构记忆 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub r#type: String,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMeta {
    pub name: String,
    pub comment: String,
    pub rows: u64,
    pub ddl: String,
    pub columns: Vec<ColumnMeta>,
    /// 取样数据：行 -> {列名: 值}
    pub sample: Vec<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationMeta {
    pub from: String,
    pub to: String,
    pub desc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseMeta {
    pub name: String,
    pub comment: String,
    pub tables: Vec<TableMeta>,
    pub relations: Vec<RelationMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnMemory {
    pub conn_id: String,
    pub synced_at: String,
    pub databases: Vec<DatabaseMeta>,
}

/* ---------- 模型 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub provider: String, // deepseek | zhipu | custom
    pub api_key: String,
    pub model_id: String,
    pub base_url: String,
    pub active: bool,
}

/* ---------- 设置 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub theme: String, // dark | light
    pub sql_timeout: u64,
    /// 两阶段 Agent 生成（分析 → 探查 → 设计 → 生成 → 校验），关闭则单次直出
    #[serde(default = "default_true")]
    pub agent_mode: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            sql_timeout: 60,
            agent_mode: true,
        }
    }
}

/* ---------- 多轮上下文 ---------- */

/// 对话中的一轮（前端 tab.messages 的精简投影），用于多轮生成时带上历史语境
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    /// user | ai
    pub role: String,
    pub text: String,
}

/* ---------- SQL 日志 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlLogItem {
    pub id: String,
    pub ts: String,
    pub sql: String,
    pub status: String, // idle | running | success | error | cancelled
    pub conn: String,
    pub db: String,
    pub ms: Option<u64>,
}

/* ---------- 查询结果 ---------- */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<QueryColumn>,
    /// 行 -> 单元格（NULL 为 null）。完整结果集，不做行数截断。
    pub rows: Vec<Vec<Option<String>>>,
    pub ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryColumn {
    pub name: String,
    pub r#type: String,
}
