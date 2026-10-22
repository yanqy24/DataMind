//! guard.rs — 只读保护：AST 级校验仅放行 SELECT 查询，并为大结果集追加安全上限
//!
//! 安全模型：前端预检 + 后端 AST 双重校验，任何写操作（INSERT/UPDATE/DELETE/DDL/存储过程等）
//! 都会在执行前被拦截，保证工具对数据库绝对只读。

use sqlparser::ast::{SetExpr, Statement};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::error::AppError;

/// 校验 SQL 为纯 SELECT 查询（允许 WITH ... SELECT）
pub fn ensure_select_only(sql: &str) -> Result<(), AppError> {
    let stmts = Parser::parse_sql(&MySqlDialect {}, sql)
        .map_err(|e| AppError::new("SQL_PARSE", format!("SQL 解析失败：{e}")))?;

    if stmts.is_empty() {
        return Err(AppError::new("SQL_EMPTY", "SQL 为空"));
    }
    if stmts.len() > 1 {
        return Err(AppError::new("SQL_MULTI", "一次只能执行一条 SELECT 语句"));
    }
    match &stmts[0] {
        // WITH ... INSERT/UPDATE 会被解析为别的 Statement，到这里的一定是查询
        Statement::Query(q) => check_query(q),
        _ => Err(AppError::new(
            "1142",
            "当前工具仅支持 SELECT 查询，增删改等写操作已被拦截。",
        )
        .with_hint("如需查询数据，请使用 SELECT 语句")),
    }
}

/// 递归校验查询体：放行纯 SELECT / UNION 等只读组合，拦截带副作用的写法
fn check_query(q: &sqlparser::ast::Query) -> Result<(), AppError> {
    // FOR UPDATE / FOR SHARE 会加行锁，属于写语义
    if !q.locks.is_empty() {
        return Err(AppError::new(
            "1142",
            "FOR UPDATE / FOR SHARE 会对数据加锁，已被拦截。",
        )
        .with_hint("该工具仅支持只读查询，请去掉加锁子句"));
    }
    check_set_expr(q.body.as_ref())
}

fn check_set_expr(e: &SetExpr) -> Result<(), AppError> {
    match e {
        SetExpr::Select(s) => {
            // SELECT ... INTO OUTFILE/DUMPFILE 会向数据库服务器写文件
            if s.into.is_some() {
                return Err(AppError::new(
                    "1142",
                    "SELECT ... INTO OUTFILE/DUMPFILE 会写出文件，已被拦截。",
                )
                .with_hint("该工具仅支持读取查询"));
            }
            Ok(())
        }
        // UNION / EXCEPT / INTERSECT 均为只读组合，递归校验两侧
        SetExpr::SetOperation { left, right, .. } => {
            check_set_expr(left.as_ref())?;
            check_set_expr(right.as_ref())
        }
        // 括号包裹的子查询
        SetExpr::Query(inner) => check_query(inner),
        _ => Err(AppError::new(
            "1142",
            "当前工具仅支持 SELECT 查询，该查询形式不被允许执行。",
        )),
    }
}

/// 为无 LIMIT 的纯 SELECT 追加行数上限，防止误查全表拖垮内存
/// （结果集在 [crate::mysql] 中另有一层 5000 行硬截断，此处是 SQL 级保护）
pub const SAFE_LIMIT: u64 = 5001;

pub fn with_safe_limit(sql: &str) -> String {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    // WITH 开头的语句在子查询语义下拼接 LIMIT 兼容性差，交由行级截断保护
    if trimmed.to_ascii_lowercase().starts_with("with") {
        return trimmed.to_string();
    }
    let lower = trimmed.to_ascii_lowercase();
    // 已含顶层 LIMIT / 锁定子句则不重复处理
    if lower.contains(" limit ") || lower.ends_with(" limit")
        || lower.contains(" for update")
        || lower.contains(" lock in share mode")
    {
        return trimmed.to_string();
    }
    format!("{trimmed} LIMIT {SAFE_LIMIT}")
}
