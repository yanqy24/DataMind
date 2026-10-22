//! agent.rs — 两阶段（多步）SQL 生成流水线
//!
//! 问题 → Schema 分析 → 数据探查 → SQL 设计 → SQL 生成 → SQL 校验 → 交付
//!
//! 相比单次直出：先让模型挑出相关表/字段，再**主动查库**摸清真实数据形态
//! （时间单位、枚举取值、NULL 比例、值域），据此做设计与生成，最后经
//! 只读校验 + EXPLAIN + 模型复审三重校验后才交付。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::error::AppError;
use crate::models::{ConnMemory, ModelConfig};
use crate::mysql::DbManager;

/* ---------- 探查规模上限（保护大库与响应延迟） ---------- */
/// Schema 分析阶段最多纳入的表数
const MAX_DIGEST_TABLES: usize = 8;
/// 探查阶段最多探查的表数
const MAX_PROBE_TABLES: usize = 3;
/// 每张表最多探查的列数
const MAX_PROBE_COLS: usize = 6;
/// 单条探查 SQL 超时（秒）
const PROBE_TIMEOUT: u64 = 15;
/// 探查时最多扫描的行数（大表保护）
const PROBE_SAMPLE: u32 = 200_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStep {
    /// schema | explore | design | build | verify
    pub key: String,
    pub label: String,
    /// 该阶段的关键结论（展示给用户的"推理过程"）
    pub summary: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAnswer {
    pub sql: String,
    pub note: String,
    pub steps: Vec<AgentStep>,
}

pub struct AgentCtx<'a> {
    pub prompt: &'a str,
    pub db: &'a str,
    pub table: &'a str,
    pub conn_id: &'a str,
    pub memory: Option<&'a ConnMemory>,
}

/// 通用规则：时间契约 / 只读 / 口径保护，各阶段共享
fn rules() -> String {
    format!(
        "通用规则（必须遵守）：\n\
         1. 当前时间 {}（时区 Asia/Shanghai，UTC+8）；“今天/本月/近 7 天”必须换算成具体区间。\n\
         2. 只能输出单条只读 SELECT（MySQL 8 语法），禁止增删改与 DDL，必须显式写 LIMIT（≤1000）。\n\
         3. 只能使用上下文里出现过的表和字段，不得臆造。\n\
         4. 时间列单位以「数据探查」结论为准：秒级时间戳用 FROM_UNIXTIME(col) 或 col 与 10 位时间戳比较，毫秒级先 /1000。\n\
         5. 一对多关系 JOIN 后再聚合会导致金额/计数翻倍，必要时先子查询去重再聚合。\n\
         6. 注意软删除（is_deleted / deleted_at / del_flag）与状态字段，按需补过滤条件。",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    )
}

/// 主流程：任何阶段失败都降级继续，最终无法生成 SQL 才报错
pub async fn run(
    model: &ModelConfig,
    db: &mut DbManager,
    ctx: AgentCtx<'_>,
    on_stage: &(dyn Fn(&str, &str) + Send + Sync),
) -> Result<AgentAnswer, AppError> {
    let mut steps: Vec<AgentStep> = Vec::new();

    /* ---------- 1. Schema 分析 ---------- */
    on_stage("schema", "分析表结构");
    let digest = schema_digest(ctx.memory, ctx.db, ctx.table, ctx.prompt);
    let analysis = match stage_schema(model, &digest, ctx.prompt, ctx.table).await {
        Ok(a) => {
            let summary = a.summary();
            steps.push(step("schema", "结构分析", &summary, true));
            a
        }
        Err(e) => {
            steps.push(step(
                "schema",
                "结构分析",
                &format!("模型分析失败（{}），改用关键词匹配定位表", e.message),
                false,
            ));
            Analysis::fallback(ctx.table)
        }
    };

    /* ---------- 2. 数据探查 ---------- */
    on_stage("explore", "探查真实数据");
    let (findings, ok) = stage_explore(db, ctx.conn_id, ctx.db, &analysis, ctx.memory).await;
    steps.push(step("explore", "数据探查", &findings, ok));

    /* ---------- 3. SQL 设计 ---------- */
    on_stage("design", "设计查询方案");
    let design = match stage_design(model, &digest, &analysis, &findings, ctx.prompt).await {
        Ok(d) => {
            let s = d.clone();
            steps.push(step("design", "方案设计", &s, true));
            s
        }
        Err(e) => {
            steps.push(step(
                "design",
                "方案设计",
                &format!("方案设计失败（{}），直接进入生成", e.message),
                false,
            ));
            String::from("（未生成设计方案，请直接按需求与探查结论生成 SQL）")
        }
    };

    /* ---------- 4. SQL 生成 ---------- */
    on_stage("build", "生成 SQL");
    let detail = schema_detail(ctx.memory, ctx.db, &analysis);
    let (mut sql, mut note) = stage_build(model, &detail, &design, &findings, ctx.prompt, "").await?;
    steps.push(step("build", "SQL 生成", &format!("{}\n{}", note, sql), true));

    /* ---------- 5. SQL 校验（最多修 1 轮） ---------- */
    on_stage("verify", "校验 SQL");
    let mut issues = deterministic_check(&sql, ctx.memory, ctx.db);
    // 有真实连接时才做 EXPLAIN；否则（如快速修复 / 无连接场景）只做静态校验
    if !ctx.conn_id.is_empty() {
        if let Err(e) = db.explain(ctx.conn_id, ctx.db, &sql).await {
            issues.push(format!("数据库执行计划检查未通过：{}", e.message));
        }
    }

    if !issues.is_empty() {
        // 带着问题重新生成一次
        on_stage("build", "按校验结果重写 SQL");
        let fix_hint = format!("上一版 SQL 存在问题，请修正：\n- {}", issues.join("\n- "));
        if let Ok((s2, n2)) = stage_build(model, &detail, &design, &findings, ctx.prompt, &fix_hint).await {
            let mut issues2 = deterministic_check(&s2, ctx.memory, ctx.db);
            if !ctx.conn_id.is_empty() && db.explain(ctx.conn_id, ctx.db, &s2).await.is_err() {
                issues2.push("修正版仍未通过执行计划检查".into());
            }
            if issues2.len() < issues.len() {
                sql = s2;
                note = n2;
                issues = issues2;
            }
            steps.push(step(
                "build",
                "按校验结果重写",
                &format!("修正后仍存在问题：{}", if issues.is_empty() { "无".into() } else { issues.join("；") }),
                issues.is_empty(),
            ));
        }
    }

    // 模型复审：确认是否满足设计意图
    let review = stage_review(model, &detail, &design, &findings, &sql, ctx.prompt).await;
    match review {
        Ok(r) => {
            steps.push(step(
                "verify",
                "SQL 校验",
                &if r.ok {
                    r.issues.join("；")
                } else {
                    format!("待确认：{}", r.issues.join("；"))
                },
                r.ok,
            ));
            if !r.ok && !r.sql.is_empty() {
                // 复审给出更正版：仍要通过只读校验才采纳
                if crate::guard::ensure_select_only(&r.sql).is_ok() {
                    sql = r.sql;
                    if !r.note.is_empty() {
                        note = r.note;
                    }
                }
            }
            if !r.issues.is_empty() && note.is_empty() {
                note = r.issues.join("；");
            }
        }
        Err(e) => {
            steps.push(step(
                "verify",
                "SQL 校验",
                &format!("模型复审未返回有效结论（{}），已按确定性校验结果交付", e.message),
                issues.is_empty(),
            ));
            if !issues.is_empty() {
                note = format!("注意：{}", issues.join("；"));
            }
        }
    }

    Ok(AgentAnswer { sql, note, steps })
}

fn step(key: &str, label: &str, summary: &str, ok: bool) -> AgentStep {
    AgentStep {
        key: key.to_string(),
        label: label.to_string(),
        summary: summary.trim().to_string(),
        ok,
    }
}

/* ---------- 1. Schema 分析 ---------- */

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Analysis {
    tables: Vec<String>,
    columns: Vec<String>,
    joins: Vec<String>,
    questions: Vec<String>,
    reason: String,
}

impl Analysis {
    fn fallback(table: &str) -> Self {
        Self {
            tables: if table.is_empty() { vec![] } else { vec![table.to_string()] },
            ..Default::default()
        }
    }
    fn summary(&self) -> String {
        let mut s = String::new();
        if !self.tables.is_empty() {
            s.push_str(&format!("相关表：{}", self.tables.join("、")));
        }
        if !self.columns.is_empty() {
            s.push_str(&format!("\n关键字段：{}", join_limited(&self.columns, 24)));
        }
        if !self.joins.is_empty() {
            s.push_str(&format!("\n关联路径：{}", self.joins.join("；")));
        }
        if !self.questions.is_empty() {
            s.push_str(&format!("\n待探查：{}", self.questions.join("；")));
        }
        if !self.reason.is_empty() {
            s.push_str(&format!("\n判断依据：{}", self.reason));
        }
        s
    }
}

fn join_limited(v: &[String], n: usize) -> String {
    let take: Vec<&String> = v.iter().take(n).collect();
    let mut s = take.iter().map(|x| x.as_str()).collect::<Vec<_>>().join("、");
    if v.len() > n {
        s.push_str(&format!(" 等 {} 个", v.len()));
    }
    s
}

async fn stage_schema(
    model: &ModelConfig,
    digest: &str,
    prompt: &str,
    table: &str,
) -> Result<Analysis, AppError> {
    let system = format!(
        "你是数据库结构分析专家。根据用户问题，从给定表结构中挑出真正需要的表与关键字段。\n\
         只输出 JSON，不要输出其他内容：\n\
         {{\"tables\":[\"orders\"],\"columns\":[\"orders.status\"],\"joins\":[\"orders.user_id=users.id\"],\
         \"questions\":[\"ctime 是秒级还是毫秒级\"],\"reason\":\"一句话说明判断\"}}\n\
         要求：\n\
         1. tables 只列真正需要的表（≤3 张），宁少勿多。\n\
         2. columns 列出会用到的字段，格式 `表.字段`。\n\
         3. questions 列出必须查库才能确认的不确定点（时间单位、枚举含义、是否含 NULL、软删除标记等）。\n\
         4. 不要臆造上下文之外的表与字段。\n\n{}",
        rules()
    );
    let user = format!(
        "用户问题：{}\n{}\n\n库表结构：\n{}",
        prompt,
        if table.is_empty() {
            String::new()
        } else {
            format!("用户已选中数据表：{}（优先考虑该表）", table)
        },
        digest
    );
    let content = crate::llm::chat(model, &system, &user, 0.0).await?;
    let v = crate::llm::json_from(&content)
        .ok_or_else(|| AppError::new("LLM_PARSE", "结构分析结果无法解析为 JSON"))?;
    let strs = |key: &str| -> Vec<String> {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    if strs("tables").is_empty() {
        return Err(AppError::new("LLM_EMPTY", "结构分析未给出相关表"));
    }
    Ok(Analysis {
        tables: strs("tables"),
        columns: strs("columns"),
        joins: strs("joins"),
        questions: strs("questions"),
        reason: v
            .get("reason")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/* ---------- 2. 数据探查 ---------- */

async fn stage_explore(
    db: &mut DbManager,
    conn_id: &str,
    schema_db: &str,
    analysis: &Analysis,
    memory: Option<&ConnMemory>,
) -> (String, bool) {
    if conn_id.is_empty() || schema_db.is_empty() {
        return ("未连接数据库，跳过数据探查".into(), false);
    }
    let mut out = String::new();
    let mut ok = true;

    let tables: Vec<String> = analysis
        .tables
        .iter()
        .filter(|t| !t.contains('.'))
        .take(MAX_PROBE_TABLES)
        .cloned()
        .collect();
    if tables.is_empty() {
        return ("未确定需要探查的表，跳过".into(), false);
    }

    for t in &tables {
        let cols = pick_columns(memory, schema_db, t, analysis);
        if cols.is_empty() {
            continue;
        }
        match probe_table(db, conn_id, schema_db, t, &cols).await {
            Ok(block) => {
                out.push_str(&block);
                out.push('\n');
            }
            Err(e) => {
                ok = false;
                out.push_str(&format!("表 {} 探查失败：{}\n", t, e.message));
            }
        }
    }

    if out.trim().is_empty() {
        return ("未能取得有效探查结果".into(), false);
    }
    (out.trim().to_string(), ok)
}

/// 挑选要探查的列：优先模型点名的列，其次时间/状态/金额/标识类列
fn pick_columns(memory: Option<&ConnMemory>, db: &str, table: &str, analysis: &Analysis) -> Vec<String> {
    let named: Vec<String> = analysis
        .columns
        .iter()
        .filter_map(|c| {
            let (t, col) = c.split_once('.')?;
            if t == table {
                Some(col.to_string())
            } else {
                None
            }
        })
        .collect();
    if !named.is_empty() {
        return named.into_iter().take(MAX_PROBE_COLS).collect();
    }
    let mem = match memory {
        Some(m) => m,
        None => return vec![],
    };
    let dbmeta = match mem.databases.iter().find(|d| d.name == db) {
        Some(d) => d,
        None => return vec![],
    };
    let tmeta = match dbmeta.tables.iter().find(|t| t.name == table) {
        Some(t) => t,
        None => return vec![],
    };
    let hot = ["time", "date", "at", "day", "status", "state", "type", "flag", "amount", "money", "price", "pay"];
    let mut picked: Vec<String> = tmeta
        .columns
        .iter()
        .filter(|c| {
            let n = c.name.to_ascii_lowercase();
            hot.iter().any(|k| n.contains(k))
        })
        .map(|c| c.name.clone())
        .collect();
    for c in &tmeta.columns {
        if picked.len() >= MAX_PROBE_COLS {
            break;
        }
        if !picked.contains(&c.name) {
            picked.push(c.name.clone());
        }
    }
    picked.into_iter().take(MAX_PROBE_COLS).collect()
}

/// 对单表跑一次聚合探查（行数 / NULL 率 / distinct / min / max）+ 低基数列的取值分布
async fn probe_table(
    db: &mut DbManager,
    conn_id: &str,
    schema_db: &str,
    table: &str,
    cols: &[String],
) -> Result<String, AppError> {
    let e = |s: &str| format!("`{}`", s.replace('`', "``"));
    let mut selects = vec!["COUNT(*) AS n".to_string()];
    for (i, c) in cols.iter().enumerate() {
        let ec = e(c);
        selects.push(format!("COUNT({ec}) AS nn{i}"));
        selects.push(format!("COUNT(DISTINCT {ec}) AS d{i}"));
        selects.push(format!("MIN({ec}) AS mn{i}"));
        selects.push(format!("MAX({ec}) AS mx{i}"));
    }
    // 采样上限：大表只扫描前 PROBE_SAMPLE 行，避免 COUNT(DISTINCT) 拖垮数据库
    let sql = format!(
        "SELECT {} FROM (SELECT * FROM {}.{} LIMIT {}) AS _probe",
        selects.join(", "),
        e(schema_db),
        e(table),
        PROBE_SAMPLE
    );
    let r = db.probe_query(conn_id, schema_db, &sql, PROBE_TIMEOUT).await?;
    let row = r
        .rows
        .first()
        .ok_or_else(|| AppError::new("PROBE_EMPTY", "探查无返回行"))?;
    let get = |i: usize| -> Option<String> { row.get(i).cloned().flatten() };

    let total: f64 = get(0).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let mut out = format!(
        "表 {}（采样 {} 行{}）",
        table,
        fmt_num(total),
        if total >= PROBE_SAMPLE as f64 {
            format!("，已上限 {}", fmt_num(PROBE_SAMPLE as f64))
        } else {
            String::new()
        }
    );

    let mut enum_targets: Vec<(usize, String)> = vec![];
    for (i, c) in cols.iter().enumerate() {
        let base = 1 + i * 4;
        let nn: f64 = get(base).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let d: f64 = get(base + 1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let mn = get(base + 2).unwrap_or_default();
        let mx = get(base + 3).unwrap_or_default();
        let null_pct = if total > 0.0 {
            (1.0 - nn / total) * 100.0
        } else {
            0.0
        };
        let mut line = format!(
            "\n- {}: distinct={}, NULL={:.1}%, min={}, max={}",
            c,
            fmt_num(d),
            null_pct,
            clip(&mn, 24),
            clip(&mx, 24)
        );
        if let Some(unit) = time_unit_hint(c, &mn, &mx) {
            line.push_str(&format!(" → {}", unit));
        }
        out.push_str(&line);
        if d > 0.0 && d <= 20.0 && cols.len() <= MAX_PROBE_COLS {
            enum_targets.push((i, c.clone()));
        }
    }

    // 低基数列：取回真实取值分布（解决 status/type 等枚举语义）
    for (i, c) in enum_targets.iter().take(3) {
        let ec = e(c);
        let sql = format!(
            "SELECT {ec} AS v, COUNT(*) AS n FROM (SELECT * FROM {}.{} LIMIT {}) AS _probe GROUP BY 1 ORDER BY n DESC LIMIT 8",
            e(schema_db),
            e(table),
            PROBE_SAMPLE
        );
        if let Ok(r) = db.probe_query(conn_id, schema_db, &sql, PROBE_TIMEOUT).await {
            let vals: Vec<String> = r
                .rows
                .iter()
                .take(8)
                .map(|row| {
                    let v = row.first().cloned().flatten().unwrap_or_else(|| "NULL".into());
                    let n = row.get(1).cloned().flatten().unwrap_or_else(|| "0".into());
                    format!("{}×{}", clip(&v, 16), n)
                })
                .collect();
            if !vals.is_empty() {
                out.push_str(&format!("\n  · {} 取值分布：{}", c, vals.join(", ")));
            }
        }
        let _ = i;
    }

    Ok(out)
}

/// 时间单位判定：整型且位数符合时间戳量级
fn time_unit_hint(col: &str, mn: &str, mx: &str) -> Option<String> {
    let name = col.to_ascii_lowercase();
    let looks_time = ["time", "date", "_at", "day", "ts"].iter().any(|k| name.contains(k));
    let sample = if mx.chars().all(|c| c.is_ascii_digit()) && !mx.is_empty() {
        mx
    } else if mn.chars().all(|c| c.is_ascii_digit()) && !mn.is_empty() {
        mn
    } else {
        return None;
    };
    let len = sample.len();
    let unit = match len {
        10 => "秒级时间戳（10 位）",
        13 => "毫秒级时间戳（13 位）",
        16 => "微秒级时间戳（16 位）",
        _ => return None,
    };
    if looks_time || len >= 10 {
        Some(format!("{}，比较时{}", unit, if len == 10 { "可直接与 10 位时间戳或 FROM_UNIXTIME(col) 结果比较" } else { "需先 /1000 换算为秒" }))
    } else {
        None
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 {
        let i = v as i64;
        let s = i.to_string();
        let mut out = String::new();
        for (idx, ch) in s.chars().rev().enumerate() {
            if idx > 0 && idx % 3 == 0 {
                out.insert(0, ',');
            }
            out.insert(0, ch);
        }
        out
    } else {
        format!("{:.1}", v)
    }
}

/* ---------- 3. SQL 设计 ---------- */

async fn stage_design(
    model: &ModelConfig,
    digest: &str,
    analysis: &Analysis,
    findings: &str,
    prompt: &str,
) -> Result<String, AppError> {
    let system = format!(
        "你是资深数据分析方案设计专家。基于结构分析与数据探查结论，设计查询方案（不要写 SQL）。\n\
         只输出 JSON：\n\
         {{\"understanding\":\"对用户意图的理解\",\"tables\":[\"orders\"],\"joins\":[\"orders.user_id=users.id\"],\
         \"filters\":[\"status=2 表示已支付\"],\"time\":\"时间字段与区间换算\",\"aggregation\":\"聚合口径与去重方式\",\
         \"assumptions\":[\"假设\"],\"risks\":[\"风险\"]}}\n\
         要求：充分利用探查结论（时间单位、枚举取值、NULL 情况、值域）；明确写清是否存在一对多导致重复计数。\n\n{}",
        rules()
    );
    let user = format!(
        "用户问题：{}\n\n结构分析：\n{}\n\n数据探查结论：\n{}\n\n库表结构（精简）：\n{}",
        prompt,
        analysis.summary(),
        findings,
        digest
    );
    let content = crate::llm::chat(model, &system, &user, 0.1).await?;
    let v = crate::llm::json_from(&content)
        .ok_or_else(|| AppError::new("LLM_PARSE", "设计方案无法解析为 JSON"))?;
    let mut out = String::new();
    if let Some(s) = v.get("understanding").and_then(|x| x.as_str()) {
        out.push_str(&format!("意图理解：{}\n", s));
    }
    for (k, label) in [
        ("tables", "涉及表"),
        ("joins", "关联"),
        ("filters", "过滤"),
        ("time", "时间处理"),
        ("aggregation", "聚合口径"),
        ("assumptions", "假设"),
        ("risks", "风险"),
    ] {
        if let Some(arr) = v.get(k).and_then(|x| x.as_array()) {
            let items: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !items.is_empty() {
                out.push_str(&format!("{}：{}\n", label, items.join("；")));
            }
        } else if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                out.push_str(&format!("{}：{}\n", label, s.trim()));
            }
        }
    }
    if out.trim().is_empty() {
        return Err(AppError::new("LLM_EMPTY", "设计方案为空"));
    }
    Ok(out.trim().to_string())
}

/* ---------- 4. SQL 生成 ---------- */

async fn stage_build(
    model: &ModelConfig,
    detail: &str,
    design: &str,
    findings: &str,
    prompt: &str,
    fix_hint: &str,
) -> Result<(String, String), AppError> {
    let system = format!(
        "你是资深 MySQL 数据分析专家，按给定的查询方案产出一条可直接执行的 SELECT。\n\
         只输出 JSON：{{\"sql\":\"...\",\"note\":\"...\"}}\n\
         规则：\n\
         1. sql 必须是单条只读 SELECT（MySQL 8），禁止增删改与 DDL。\n\
         2. 严格遵循设计方案与数据探查结论（尤其时间单位与枚举取值）。\n\
         3. 只能用上下文出现的表和字段；必须显式写 LIMIT（≤1000）。\n\
         4. 不要写注释，不要输出 Markdown 代码块。\n\
         5. note 用一到两句简体中文说明思路与取值假设。\n\n{}",
        rules()
    );
    let user = format!(
        "用户问题：{}\n\n查询方案：\n{}\n\n数据探查结论：\n{}\n\n表结构详情：\n{}{}",
        prompt,
        design,
        findings,
        detail,
        if fix_hint.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", fix_hint)
        }
    );
    let content = crate::llm::chat(model, &system, &user, 0.1).await?;
    let (sql, note) = crate::llm::parse_answer(&content)?;
    Ok((sql, note))
}

/* ---------- 5. SQL 校验 ---------- */

#[derive(Debug, Deserialize)]
struct Review {
    ok: bool,
    issues: Vec<String>,
    sql: String,
    note: String,
}

async fn stage_review(
    model: &ModelConfig,
    detail: &str,
    design: &str,
    findings: &str,
    sql: &str,
    prompt: &str,
) -> Result<Review, AppError> {
    let system = format!(
        "你是 SQL 审查专家。检查下面这条 SQL 是否满足用户意图与查询方案，并指出问题。\n\
         只输出 JSON：{{\"ok\":true,\"issues\":[\"问题\"],\"sql\":\"ok=false 时给出修正后的完整 SQL，否则空字符串\",\"note\":\"补充说明\"}}\n\
         重点检查：\n\
         1. 表与字段是否都存在于结构上下文（不得臆造）。\n\
         2. 时间字段单位与区间换算是否与探查结论一致。\n\
         3. 聚合是否因一对多 JOIN 而重复计数/金额翻倍。\n\
         4. 是否遗漏必要的过滤条件（软删除、状态、时间范围）。\n\
         5. 是否单条只读 SELECT 且带 LIMIT。\n\
         若没有问题则 ok=true 且 issues 可留空。\n\n{}",
        rules()
    );
    let user = format!(
        "用户问题：{}\n\n查询方案：\n{}\n\n数据探查结论：\n{}\n\n表结构详情：\n{}\n\n待校验 SQL：\n{}",
        prompt, design, findings, detail, sql
    );
    let content = crate::llm::chat(model, &system, &user, 0.0).await?;
    let v = crate::llm::json_from(&content)
        .ok_or_else(|| AppError::new("LLM_PARSE", "校验结果无法解析为 JSON"))?;
    Ok(Review {
        ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        issues: v
            .get("issues")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        sql: v.get("sql").and_then(|x| x.as_str()).unwrap_or_default().trim().to_string(),
        note: v.get("note").and_then(|x| x.as_str()).unwrap_or_default().trim().to_string(),
    })
}

/// 确定性校验：只读、表是否存在、是否有 LIMIT
fn deterministic_check(sql: &str, memory: Option<&ConnMemory>, db: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if let Err(e) = crate::guard::ensure_select_only(sql) {
        issues.push(format!("只读校验未通过：{}", e.message));
    }
    let lower = sql.to_ascii_lowercase();
    if !lower.contains("limit") {
        issues.push("缺少 LIMIT，大结果集会拖慢查询".into());
    }
    if let Some(mem) = memory {
        let known: HashSet<String> = mem
            .databases
            .iter()
            .filter(|d| d.name == db)
            .flat_map(|d| d.tables.iter().map(|t| t.name.to_ascii_lowercase()))
            .collect();
        if !known.is_empty() {
            for t in referenced_tables(sql) {
                if !known.contains(&t.to_ascii_lowercase()) {
                    issues.push(format!("结构记忆中不存在表 `{}`，请确认表名", t));
                }
            }
        }
    }
    issues
}

/// 粗糙提取 SQL 里 FROM / JOIN 后的表名（够用即可，避免引入解析复杂度）
fn referenced_tables(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    // 别名（AS xxx / FROM 后的派生表别名）不算未知表
    let aliases: HashSet<String> = {
        let mut set = HashSet::new();
        let lower = format!(" {} ", sql.to_ascii_lowercase().replace('\n', " "));
        for part in lower.split(" as ") {
            if let Some(first) = part.split_whitespace().next() {
                let a = first.trim_matches(|c: char| c == '`' || c == ',' || c == ')');
                if !a.is_empty() {
                    set.insert(a.to_string());
                }
            }
        }
        set
    };
    let words: Vec<&str> = sql.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        let lw = w.to_ascii_lowercase().trim_matches(',').to_string();
        if (lw == "from" || lw == "join") && i + 1 < words.len() {
            let raw = words[i + 1].trim_matches(',').trim_matches('`');
            // 去掉 `库.表` 前缀
            let name = raw.split('.').last().unwrap_or(raw).to_string();
            if !name.is_empty()
                && !name.starts_with('(')
                && !aliases.contains(&name.to_ascii_lowercase())
            {
                out.push(name);
            }
        }
    }
    out
}

/* ---------- 上下文装配 ---------- */

/// 精简版结构摘要（Schema 分析阶段）：按与问题的相关性召回表，控制 token
pub fn schema_digest(memory: Option<&ConnMemory>, db: &str, table: &str, prompt: &str) -> String {
    let mem = match memory {
        Some(m) => m,
        None => return String::from("（无本地结构记忆）"),
    };
    let dbmeta = match mem.databases.iter().find(|d| d.name == db) {
        Some(d) => d,
        None => return String::from("（结构记忆中没有该数据库）"),
    };
    let keys = tokens(prompt);
    let mut scored: Vec<(usize, &crate::models::TableMeta)> = dbmeta
        .tables
        .iter()
        .map(|t| (relevance(t, &keys), t))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let mut picked: Vec<&crate::models::TableMeta> = Vec::new();
    if !table.is_empty() {
        if let Some(t) = dbmeta.tables.iter().find(|t| t.name == table) {
            picked.push(t);
        }
    }
    for (score, t) in &scored {
        if picked.len() >= MAX_DIGEST_TABLES {
            break;
        }
        if *score > 0 && !picked.iter().any(|p| p.name == t.name) {
            picked.push(t);
        }
    }
    // 召回为空（问题与表名无字面重合）时，给前几张表兜底
    if picked.len() <= 1 {
        for (_, t) in &scored {
            if picked.len() >= MAX_DIGEST_TABLES.min(4) {
                break;
            }
            if !picked.iter().any(|p| p.name == t.name) {
                picked.push(t);
            }
        }
    }

    let mut s = format!("数据库 `{}`（共 {} 张表，以下为相关性最高的 {} 张）\n", db, dbmeta.tables.len(), picked.len());
    if !dbmeta.relations.is_empty() {
        s.push_str("外键关系：\n");
        for r in dbmeta.relations.iter().take(30) {
            s.push_str(&format!("  {} -> {}\n", r.from, r.to));
        }
    }
    for t in &picked {
        let cols: Vec<String> = t
            .columns
            .iter()
            .take(40)
            .map(|c| {
                if c.comment.is_empty() {
                    format!("{} {}", c.name, c.r#type)
                } else {
                    format!("{} {} ({})", c.name, c.r#type, c.comment)
                }
            })
            .collect();
        s.push_str(&format!(
            "表 {}{}（约 {} 行）：{}\n",
            t.name,
            if t.comment.is_empty() {
                String::new()
            } else {
                format!("「{}」", t.comment)
            },
            t.rows,
            cols.join(", ")
        ));
    }
    if dbmeta.tables.len() > picked.len() {
        let rest: Vec<String> = dbmeta
            .tables
            .iter()
            .filter(|t| !picked.iter().any(|p| p.name == t.name))
            .take(60)
            .map(|t| t.name.clone())
            .collect();
        s.push_str(&format!("其他表（仅表名）：{}\n", rest.join("、")));
    }
    s
}

/// 详细结构（生成阶段）：分析阶段选中的表给完整列与 DDL
fn schema_detail(memory: Option<&ConnMemory>, db: &str, analysis: &Analysis) -> String {
    let mem = match memory {
        Some(m) => m,
        None => return String::from("（无本地结构记忆）"),
    };
    let dbmeta = match mem.databases.iter().find(|d| d.name == db) {
        Some(d) => d,
        None => return String::from("（结构记忆中没有该数据库）"),
    };
    let names: Vec<String> = analysis
        .tables
        .iter()
        .map(|t| t.split('.').last().unwrap_or(t).to_string())
        .collect();
    let mut s = String::new();
    if !dbmeta.relations.is_empty() {
        s.push_str("外键关系：\n");
        for r in &dbmeta.relations {
            s.push_str(&format!("  {} -> {}\n", r.from, r.to));
        }
    }
    for t in &dbmeta.tables {
        if !names.iter().any(|n| n == &t.name) {
            continue;
        }
        s.push_str(&format!("\n表 {}{}：\n", t.name, if t.comment.is_empty() { String::new() } else { format!("「{}」", t.comment) }));
        for c in &t.columns {
            s.push_str(&format!(
                "  {} {}{}\n",
                c.name,
                c.r#type,
                if c.comment.is_empty() {
                    String::new()
                } else {
                    format!(" -- {}", c.comment)
                }
            ));
        }
        if !t.ddl.is_empty() {
            s.push_str(&format!("  DDL: {}\n", t.ddl.lines().collect::<Vec<_>>().join(" ")));
        }
        if !t.sample.is_empty() {
            s.push_str(&format!("  取样: {}\n", serde_json::to_string(&t.sample).unwrap_or_default()));
        }
    }
    if s.trim().is_empty() {
        // 分析阶段没给出表，退化给全库精简结构
        s = schema_digest(memory, db, "", "");
    }
    s
}

fn tokens(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in prompt.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let w = w.trim().to_ascii_lowercase();
        if w.len() >= 3 {
            out.push(w);
        }
    }
    let cjk: Vec<char> = prompt.chars().filter(|c| !c.is_ascii()).collect();
    for w in cjk.windows(2) {
        out.push(w.iter().collect::<String>());
    }
    out
}

fn relevance(t: &crate::models::TableMeta, keys: &[String]) -> usize {
    let mut hay = format!("{} {}", t.name, t.comment).to_ascii_lowercase();
    for c in &t.columns {
        hay.push(' ');
        hay.push_str(&c.name.to_ascii_lowercase());
        hay.push(' ');
        hay.push_str(&c.comment.to_ascii_lowercase());
        hay.push(' ');
        hay.push_str(&c.r#type.to_ascii_lowercase());
    }
    keys.iter().filter(|k| hay.contains(k.as_str())).count()
}

/* ---------- 测试：用本地假模型服务跑通整条流水线 ---------- */
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ColumnMeta, DatabaseMeta, RelationMeta, TableMeta};
    use std::sync::Arc;

    fn memory() -> ConnMemory {
        ConnMemory {
            conn_id: "c1".into(),
            synced_at: "2026-10-22 14:00".into(),
            databases: vec![DatabaseMeta {
                name: "shop".into(),
                comment: String::new(),
                tables: vec![TableMeta {
                    name: "orders".into(),
                    comment: "订单表".into(),
                    rows: 1000,
                    ddl: "CREATE TABLE orders (id bigint, status tinyint, ctime int)".into(),
                    columns: vec![
                        ColumnMeta { name: "id".into(), r#type: "bigint".into(), comment: "主键".into() },
                        ColumnMeta { name: "status".into(), r#type: "tinyint".into(), comment: "状态".into() },
                        ColumnMeta { name: "ctime".into(), r#type: "int".into(), comment: "创建时间".into() },
                    ],
                    sample: vec![],
                }],
                relations: vec![RelationMeta {
                    from: "orders.id".into(),
                    to: "users.id".into(),
                    desc: "外键".into(),
                }],
            }],
        }
    }

    /// 起一个假 /chat/completions：按请求里出现的阶段关键词返回对应 JSON
    async fn fake_llm() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 65536];
                    let n = match socket.read(&mut buf).await { Ok(n) if n > 0 => n, _ => return };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let body = if req.contains("结构分析专家") {
                        r#"{"tables":["orders"],"columns":["orders.status","orders.ctime"],"joins":[],"questions":["ctime 单位"],"reason":"订单表包含状态与时间"}"#
                    } else if req.contains("方案设计专家") {
                        r#"{"understanding":"统计订单数","tables":["orders"],"joins":[],"filters":["ctime 为秒级时间戳"],"time":"近 7 天用 UNIX_TIMESTAMP 换算","aggregation":"COUNT(*)"}"#
                    } else if req.contains("SQL 审查专家") {
                        r#"{"ok":true,"issues":["已确认时间单位为秒级"],"sql":"","note":""}"#
                    } else {
                        r#"{"sql":"SELECT COUNT(*) AS cnt FROM orders WHERE ctime >= UNIX_TIMESTAMP(DATE_SUB(CURDATE(), INTERVAL 7 DAY)) LIMIT 1000","note":"按秒级时间戳过滤近 7 天"}"#
                    };
                    let payload = format!("{{\"choices\":[{{\"message\":{{\"content\":{}}}}}]}}", serde_json::to_string(body).unwrap());
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    #[tokio::test]
    async fn pipeline_runs_all_stages() {
        let (base, _h) = fake_llm().await;
        let model = ModelConfig {
            id: "m1".into(),
            name: "fake".into(),
            provider: "openai".into(),
            base_url: base,
            api_key: "k".into(),
            model_id: "fake-model".into(),
            active: true,
        };
        let dir = std::env::temp_dir().join(format!("dm-agent-test-{}", chrono::Utc::now().timestamp_millis()));
        let store = Arc::new(crate::store::Store::new(dir).unwrap());
        let mut db = DbManager::new(store);
        let mem = memory();
        let ctx = AgentCtx {
            prompt: "统计近 7 天的订单数",
            db: "shop",
            table: "orders",
            conn_id: "", // 无真实连接 → 探查阶段跳过
            memory: Some(&mem),
        };
        let ans = run(&model, &mut db, ctx, &|_k, _l| {}).await.unwrap();
        assert!(ans.sql.to_lowercase().starts_with("select"), "SQL 应为 SELECT：{}", ans.sql);
        assert!(ans.sql.to_lowercase().contains("limit"), "应带 LIMIT：{}", ans.sql);
        let keys: Vec<&str> = ans.steps.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"schema"), "缺少结构分析：{:?}", keys);
        assert!(keys.contains(&"explore"), "缺少数据探查：{:?}", keys);
        assert!(keys.contains(&"design"), "缺少方案设计：{:?}", keys);
        assert!(keys.contains(&"build"), "缺少 SQL 生成：{:?}", keys);
        assert!(keys.contains(&"verify"), "缺少 SQL 校验：{:?}", keys);
        println!("SQL: {}\nSTEPS: {:#?}", ans.sql, ans.steps);
    }
}
