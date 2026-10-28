//! guard.rs — 只读保护：AST 级校验仅放行 SELECT 查询
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

/*
 * 历史说明：此处曾为无 LIMIT 的 SELECT 自动追加 `LIMIT 5001`，并在结果侧做 5000 行硬截断。
 * 该行为已按需求移除 —— SQL 原样下发、结果集完整返回，大结果集由前端分页按需渲染
 * （见 ResultGrid.vue）。若将来需要保护，应在**设置项**中显式开启，而不是默认改写用户 SQL。
 */

#[cfg(test)]
mod tests {
    use super::ensure_select_only;

    /// 不限制结果集：校验只判断「是否只读」，既不改写语句也不要求必须带 LIMIT
    #[test]
    fn select_without_limit_is_allowed() {
        assert!(ensure_select_only("SELECT * FROM orders").is_ok());
        assert!(ensure_select_only("SELECT * FROM orders LIMIT 10").is_ok());
        assert!(ensure_select_only("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    }

    /// 移除 LIMIT 逻辑不能以放松只读约束为代价
    #[test]
    fn write_and_lock_are_still_rejected() {
        assert!(ensure_select_only("DELETE FROM orders").is_err());
        assert!(ensure_select_only("UPDATE orders SET a = 1").is_err());
        assert!(ensure_select_only("SELECT * FROM orders FOR UPDATE").is_err());
        assert!(ensure_select_only("SELECT * FROM orders INTO OUTFILE '/tmp/a'").is_err());
    }
}
