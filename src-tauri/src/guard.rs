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
 * 该行为已按需求移除 —— 改为**服务端分页**：执行时按页追加 `LIMIT n OFFSET m`，
 * 一次只取一页（默认 1000 行）回客户端，翻页时再查下一页，而不是把整表拉回来前端切分。
 */

/// 顶层是否已带 LIMIT / OFFSET（AST 精确判断，不会误判子查询里的 LIMIT）
pub fn has_row_limit(sql: &str) -> bool {
    match Parser::parse_sql(&MySqlDialect {}, sql) {
        Ok(stmts) => matches!(
            stmts.first(),
            Some(Statement::Query(q)) if q.limit.is_some() || q.offset.is_some()
        ),
        Err(_) => false,
    }
}

/// 服务端分页：为**无 LIMIT** 的 SELECT 追加 `LIMIT n OFFSET m`。
///
/// 返回 `None` 表示用户 SQL 已自带 LIMIT / OFFSET —— 那是用户显式指定的行数，
/// 保持原样执行，不覆盖其意图（此时界面也不分页）。
pub fn append_paging(sql: &str, page: u32, page_size: u32) -> Option<String> {
    if has_row_limit(sql) {
        return None;
    }
    let trimmed = sql.trim().trim_end_matches(';').trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let page = page.max(1);
    let page_size = page_size.max(1);
    let offset = (page as u64 - 1) * page_size as u64;
    // 换行后再拼接：即便 SQL 末尾是 `-- 行注释` 也不会把 LIMIT 吞进注释里
    Some(if offset == 0 {
        format!("{trimmed}\nLIMIT {page_size}")
    } else {
        format!("{trimmed}\nLIMIT {page_size} OFFSET {offset}")
    })
}

/// 统计总行数用的包装 SQL（`SELECT COUNT(*) FROM (原 SQL) AS t`）。
/// `WITH ...` 语句无法安全包进派生表，返回 `None`（此时界面降级为「上一页 / 下一页」）。
pub fn count_wrap(sql: &str) -> Option<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim_end();
    if trimmed.is_empty() || trimmed.to_ascii_lowercase().starts_with("with") {
        return None;
    }
    Some(format!(
        "SELECT COUNT(*) AS `_dm_total` FROM (\n{trimmed}\n) AS `_dm_src`"
    ))
}

#[cfg(test)]
mod tests {
    use super::{append_paging, count_wrap, ensure_select_only};

    /// 不限制结果集：校验只判断「是否只读」，既不改写语句也不要求必须带 LIMIT
    #[test]
    fn select_without_limit_is_allowed() {
        assert!(ensure_select_only("SELECT * FROM orders").is_ok());
        assert!(ensure_select_only("SELECT * FROM orders LIMIT 10").is_ok());
        assert!(ensure_select_only("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    }

    /// 服务端分页：无 LIMIT 的 SQL 追加 LIMIT/OFFSET，已带 LIMIT 的保持原样
    #[test]
    fn paging_appended_only_when_no_limit() {
        assert_eq!(
            append_paging("SELECT * FROM orders", 1, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000"
        );
        assert_eq!(
            append_paging("SELECT * FROM orders", 3, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000 OFFSET 2000"
        );
        // 用户自带 LIMIT / OFFSET：保持原样，不覆盖用户意图
        assert_eq!(append_paging("SELECT * FROM orders LIMIT 50", 1, 1000), None);
        assert_eq!(append_paging("SELECT * FROM orders LIMIT 50 OFFSET 10", 1, 1000), None);
        // 子查询里的 LIMIT 不算顶层限制，仍应分页
        assert!(append_paging(
            "SELECT id, (SELECT x FROM t2 LIMIT 1) AS y FROM t1",
            2,
            500
        )
        .unwrap()
        .ends_with("LIMIT 500 OFFSET 500"));
        // UNION 语句：LIMIT 作用于整体
        assert!(append_paging("SELECT a FROM t1 UNION SELECT a FROM t2", 2, 200)
            .unwrap()
            .contains("LIMIT 200 OFFSET 200"));
        // 第 0 页按第 1 页处理
        assert!(append_paging("SELECT 1", 0, 100).unwrap().ends_with("LIMIT 100"));
    }

    /// 总数统计包装：WITH 语句不做包装（界面降级为上一页/下一页）
    #[test]
    fn count_wrap_skips_cte() {
        assert!(count_wrap("SELECT * FROM t").unwrap().starts_with("SELECT COUNT(*)"));
        assert!(count_wrap("SELECT * FROM t;").unwrap().contains("AS `_dm_src`"));
        assert_eq!(count_wrap("WITH x AS (SELECT 1) SELECT * FROM x"), None);
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
