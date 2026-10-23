//! llm.rs — 模型推理：调用 OpenAI 兼容 /chat/completions 生成 SQL

use serde::{Deserialize, Serialize};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::models::{ConnMemory, ModelConfig};

const TIMEOUT: Duration = Duration::from_secs(120);
/// 上下文（schema 摘要）硬上限，避免超大库撑爆请求
const MAX_CONTEXT_CHARS: usize = 40_000;

#[derive(Serialize)]
struct ChatMsg {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMsg>,
    temperature: f32,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMessage,
}

#[derive(Deserialize)]
struct RespMessage {
    content: Option<String>,
}

/// 生成 SQL 请求（new = 自然语言新查询，fix = 结合错误修复）
pub struct GenerateReq<'a> {
    pub prompt: &'a str,
    pub mode: &'a str,
    pub prev_sql: &'a str,
    pub error: &'a str,
    pub memory: Option<&'a ConnMemory>,
    pub db: &'a str,
    pub table: &'a str,
}

/// 通用对话调用：返回模型原始文本（Agent 各阶段复用）。
///
/// `cancel` 用于中断在途请求：用户点「停止」或生成总预算耗尽时，
/// 立刻丢弃这个 HTTP 请求，而不是干等 120s 超时。
pub async fn chat(
    model: &ModelConfig,
    system: &str,
    user: &str,
    temperature: f32,
    cancel: &CancellationToken,
) -> Result<String, AppError> {
    call_model(model, system, user, temperature, cancel).await
}

/// 从模型输出中提取 JSON 对象（容忍 ```json 围栏与前后废话）
pub fn json_from(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    let bare = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim().trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    if let Some(start) = bare.find('{') {
        // 从最后一个 } 往前截取，兼容结尾多余文本
        if let Some(end) = bare.rfind('}') {
            if end > start {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&bare[start..=end]) {
                    return Some(v);
                }
            }
        }
    }
    serde_json::from_str::<serde_json::Value>(bare).ok()
}

pub async fn generate_sql(
    model: &ModelConfig,
    req: GenerateReq<'_>,
    cancel: &CancellationToken,
) -> Result<(String, String), AppError> {
    let (system, user) = build_messages(&req);
    let content = call_model(model, &system, &user, 0.2, cancel).await?;
    parse_answer(&content)
}

/// 实际发起 /chat/completions 请求
async fn call_model(
    model: &ModelConfig,
    system: &str,
    user: &str,
    temperature: f32,
    cancel: &CancellationToken,
) -> Result<String, AppError> {
    let base = model.base_url.trim().trim_end_matches('/');
    let url = if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    };
    let body = ChatRequest {
        model: model.model_id.clone(),
        messages: vec![
            ChatMsg { role: "system", content: system.to_string() },
            ChatMsg { role: "user", content: user.to_string() },
        ],
        temperature,
        stream: false,
    };

    let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
    let sending = client
        .post(&url)
        .bearer_auth(&model.api_key)
        .json(&body)
        .send();
    let resp = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(AppError::new("CANCELLED", "已取消生成")),
        r = sending => r.map_err(|e| AppError::new("LLM_HTTP", format!("模型请求失败：{e}")))?,
    };

    let status = resp.status();
    let reading = resp.text();
    let text = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(AppError::new("CANCELLED", "已取消生成")),
        r = reading => r.map_err(|e| AppError::new("LLM_HTTP", format!("读取响应失败：{e}")))?,
    };
    if !status.is_success() {
        // 常见：401 Key 无效 / 429 限流 / 400 模型名错误
        return Err(AppError::new(
            &format!("LLM_{}", status.as_u16()),
            format!("模型服务返回 {}：{}", status.as_u16(), truncate(&text, 400)),
        ));
    }

    let parsed: ChatResponse = serde_json::from_str(&text)
        .map_err(|e| AppError::new("LLM_PARSE", format!("响应解析失败：{e}")))?;
    let content = parsed
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .ok_or_else(|| AppError::new("LLM_EMPTY", "模型返回内容为空"))?;

    Ok(content)
}

/* ---------- Prompt 构造 ---------- */

fn build_messages(req: &GenerateReq<'_>) -> (String, String) {
    let mut system = String::from(
        "你是资深 MySQL 数据分析专家，任务是把自然语言需求转成一条可直接执行的 SELECT 语句。\n\
         规则：\n\
         1. 只输出 JSON，格式：{\"sql\": \"...\", \"note\": \"...\"}，不要输出其他任何内容。\n\
         2. sql 必须是单条只读 SELECT（MySQL 8 语法），禁止 INSERT/UPDATE/DELETE/DDL。\n\
         3. 严格使用下方表结构中存在的表和字段，不要臆造字段；关系查询优先利用给出的外键关系。\n\
         4. note 用一句简体中文说明思路或取舍，可为空字符串。",
    );

    if let Some(mem) = req.memory {
        if let Some(db) = mem.databases.iter().find(|d| d.name == req.db) {
            let mut ctx = format!("\n\n数据库 `{}` 的结构上下文：\n", db.name);
            if !db.relations.is_empty() {
                ctx.push_str("外键关系：\n");
                for r in &db.relations {
                    ctx.push_str(&format!("  {}.{} -> {}\n", db.name, r.from, r.to));
                }
            }
            ctx.push_str("表结构：\n");
            for t in &db.tables {
                let cols: Vec<String> = t
                    .columns
                    .iter()
                    .map(|c| {
                        if c.comment.is_empty() {
                            format!("{} {}", c.name, c.r#type)
                        } else {
                            format!("{} {} ({})", c.name, c.r#type, c.comment)
                        }
                    })
                    .collect();
                ctx.push_str(&format!(
                    "  {}{}: {}\n",
                    t.name,
                    if t.comment.is_empty() {
                        String::new()
                    } else {
                        format!("「{}」", t.comment)
                    },
                    cols.join(", ")
                ));
                // 选中表附上完整 DDL 与取样
                if !req.table.is_empty() && t.name == req.table {
                    ctx.push_str(&format!("  -- {} 完整建表语句：\n{}\n", t.name, t.ddl));
                    if !t.sample.is_empty() {
                        ctx.push_str(&format!(
                            "  -- {} 取样数据：{}\n",
                            t.name,
                            serde_json::to_string(&t.sample).unwrap_or_default()
                        ));
                    }
                }
            }
            if ctx.len() > MAX_CONTEXT_CHARS {
                // 注意：String::truncate 落在 UTF-8 字符边界中间会 panic（中文上下文必触发），
                // 必须先回退到合法边界再截断。
                let mut end = MAX_CONTEXT_CHARS;
                while !ctx.is_char_boundary(end) {
                    end -= 1;
                }
                ctx.truncate(end);
            }
            system.push_str(&ctx);
        }
    }

    let user = if req.mode == "fix" {
        format!(
            "此前的 SQL 执行出错，请修复它，保持原意。\n原 SQL：\n{}\n错误信息：{}\n{}",
            req.prev_sql,
            req.error,
            if req.prompt.trim().is_empty() {
                String::new()
            } else {
                format!("补充要求：{}", req.prompt)
            }
        )
    } else {
        let focus = if req.table.is_empty() {
            String::new()
        } else {
            format!("用户当前选中的数据表是 {}，优先在该表查询，确需关联其他表时再联查。", req.table)
        };
        format!("需求：{}\n{}", req.prompt, focus)
    };

    (system, user)
}

/* ---------- 响应解析 ---------- */

pub fn parse_answer(content: &str) -> Result<(String, String), AppError> {
    let trimmed = content.trim();
    // 剥掉 ```json ... ``` 围栏
    let bare = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim().trim_end_matches("```").trim())
        .unwrap_or(trimmed);

    // 优先按 JSON 解析
    if bare.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(bare) {
            let sql = v
                .get("sql")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let note = v
                .get("note")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            if !sql.is_empty() {
                return Ok((sql, note));
            }
        }
    }

    // 兜底：把整段内容当作 SQL（做一次基础清洗）
    let sql = bare
        .trim()
        .trim_start_matches("sql")
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string();
    if sql.is_empty() {
        return Err(AppError::new("LLM_EMPTY", "模型未返回有效 SQL"));
    }
    Ok((sql, String::new()))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
