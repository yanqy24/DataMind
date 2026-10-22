//! store.rs — 本地文件持久化：连接 / 模型 / 设置 / 记忆 / SQL 日志
//!
//! 数据目录：{app_data_dir}
//!   ├── connections.json   连接配置（含密码，明文存储，详见 README 安全说明）
//!   ├── models.json        模型配置
//!   ├── settings.json      应用设置
//!   ├── sql_log.json       SQL 日志（环形上限 500 条）
//!   └── memory/{conn_id}.json  每个连接的结构记忆

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::models::*;

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Result<Self, AppError> {
        fs::create_dir_all(root.join("memory"))?;
        Ok(Self { root })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read_json<T: serde::de::DeserializeOwned>(&self, p: &Path, default: T) -> Result<T, AppError> {
        if !p.exists() {
            return Ok(default);
        }
        let raw = fs::read_to_string(p)?;
        serde_json::from_str(&raw).or_else(|_| Ok(default))
    }

    fn write_json<T: serde::Serialize + ?Sized>(&self, p: &Path, v: &T) -> Result<(), AppError> {
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
        fs::rename(&tmp, p)?; // 原子写入，避免断电写坏
        Ok(())
    }

    /* ---------- 连接 ---------- */

    pub fn load_connections(&self) -> Result<Vec<ConnConfig>, AppError> {
        self.read_json(&self.path("connections.json"), Vec::new())
    }

    pub fn save_connections(&self, conns: &[ConnConfig]) -> Result<(), AppError> {
        self.write_json(&self.path("connections.json"), conns)
    }

    /* ---------- 模型 ---------- */

    pub fn load_models(&self) -> Result<Vec<ModelConfig>, AppError> {
        self.read_json(&self.path("models.json"), Vec::new())
    }

    pub fn save_models(&self, models: &[ModelConfig]) -> Result<(), AppError> {
        self.write_json(&self.path("models.json"), models)
    }

    /* ---------- 设置 ---------- */

    pub fn load_settings(&self) -> Result<AppSettings, AppError> {
        self.read_json(&self.path("settings.json"), AppSettings::default())
    }

    pub fn save_settings(&self, s: &AppSettings) -> Result<(), AppError> {
        self.write_json(&self.path("settings.json"), s)
    }

    /* ---------- 结构记忆 ---------- */

    pub fn load_memory(&self, conn_id: &str) -> Result<Option<ConnMemory>, AppError> {
        let p = self.root.join("memory").join(format!("{conn_id}.json"));
        if !p.exists() {
            return Ok(None);
        }
        Ok(self.read_json(&p, Option::<ConnMemory>::None)?)
    }

    pub fn save_memory(&self, mem: &ConnMemory) -> Result<(), AppError> {
        let p = self.root.join("memory").join(format!("{}.json", mem.conn_id));
        self.write_json(&p, mem)
    }

    pub fn delete_memory(&self, conn_id: &str) -> Result<(), AppError> {
        let p = self.root.join("memory").join(format!("{conn_id}.json"));
        if p.exists() {
            fs::remove_file(p)?;
        }
        Ok(())
    }

    /* ---------- SQL 日志 ---------- */

    const LOG_MAX: usize = 500;

    pub fn append_log(&self, item: SqlLogItem) -> Result<(), AppError> {
        let mut list: Vec<SqlLogItem> = self.read_json(&self.path("sql_log.json"), Vec::new())?;
        list.insert(0, item);
        if list.len() > Self::LOG_MAX {
            list.truncate(Self::LOG_MAX);
        }
        self.write_json(&self.path("sql_log.json"), &list)
    }

    pub fn load_log(&self) -> Result<Vec<SqlLogItem>, AppError> {
        self.read_json(&self.path("sql_log.json"), Vec::new())
    }

    pub fn clear_log(&self) -> Result<(), AppError> {
        self.write_json(&self.path("sql_log.json"), &Vec::<SqlLogItem>::new())
    }
}
