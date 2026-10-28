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

/// 服务端分页：把 SQL 改写成「只取第 `page` 页」的形式，返回 `None` 表示不需要改写。
///
/// - 无顶层 LIMIT：末尾追加 `LIMIT n OFFSET m`（一次只取一页）。
/// - **有顶层 LIMIT 且用户要的行数超过一页**：改写该 LIMIT 子句（`LIMIT 2100` → 第 1 页
///   `LIMIT 1000`、第 2 页 `LIMIT 1000 OFFSET 1000`…），仍然按页取数而不是一次拉回 2100 行。
/// - 有顶层 LIMIT 但一页放得下（如 `LIMIT 50`）：保持用户意图原样执行，`None`。
/// - LIMIT 值不是字面量（如 `LIMIT ?`）或无法安全改写：`None`（保守不改）。
pub fn append_paging(sql: &str, page: u32, page_size: u32) -> Option<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let page = page.max(1) as u64;
    let page_size = page_size.max(1) as u64;
    let page_off = (page - 1) * page_size;

    // 顶层没有 LIMIT：换行后再拼接，即便 SQL 末尾是 `-- 行注释` 也不会把 LIMIT 吞进注释里
    if top_limit_index(trimmed).is_none() {
        return Some(if page_off == 0 {
            format!("{trimmed}\nLIMIT {page_size}")
        } else {
            format!("{trimmed}\nLIMIT {page_size} OFFSET {page_off}")
        });
    }
    match top_limit(trimmed) {
        // 有 LIMIT 但无法安全解析（`LIMIT ?`、尾部还有其它子句）→ 保守不改写
        None => None,
        Some((limit, user_off)) => {
            // 用户显式限制的行数一页放得下 → 尊重原样，不分页
            if limit <= page_size {
                return None;
            }
            // 超出用户指定的范围 → 取空结果（页码被前端总数限制住了，这里只是兜底）
            let take = if page_off >= limit { 0 } else { page_size.min(limit - page_off) };
            // 用户自带的 OFFSET 需要叠加：分页是相对用户结果集的偏移
            replace_top_limit(trimmed, take, user_off + page_off)
        }
    }
}

/* ---------- 顶层 LIMIT / OFFSET 的定位与改写（轻量扫描，不改 SQL 其余部分） ---------- */

/// 顶层 `LIMIT` 关键字的字节下标：跳过字符串字面量、注释与括号内的子查询
fn top_limit_index(sql: &str) -> Option<usize> {
    let b = sql.as_bytes();
    let n = b.len();
    let lower = sql.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < n {
        match b[i] {
            b'\'' | b'"' | b'`' => {
                i = skip_quoted(b, i)? + 1;
            }
            b'-' if i + 1 < n && b[i + 1] == b'-' => {
                i += 2;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'#' => {
                i += 1;
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i = skip_block_comment(b, i)?;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ => {
                if depth == 0
                    && lb[i] == b'l'
                    && i + 5 <= n
                    && &lb[i..i + 5] == b"limit"
                    && (i == 0 || !is_ident_byte(b[i - 1]))
                    && (i + 5 >= n || !is_ident_byte(b[i + 5]))
                {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

/// 解析顶层 `LIMIT`：返回 `(取多少行, 跳过多少行)`。
/// 支持 `LIMIT n` / `LIMIT n OFFSET m` / `LIMIT m, n`（MySQL 逗号形式）。
/// 无法安全解析（非字面量、尾部还有其它子句）时返回 `None`。
pub fn top_limit(sql: &str) -> Option<(u64, u64)> {
    let idx = top_limit_index(sql)?;
    let rest = sql[idx + 5..].trim_start();
    let (n1, tail1) = read_uint(rest)?;
    let t = tail1.trim_start();
    // MySQL 逗号形式：LIMIT <offset>, <count>
    if let Some(s) = t.strip_prefix(',') {
        let (n2, tail2) = read_uint(s.trim_start())?;
        if !rest_is_empty(tail2) {
            return None;
        }
        return Some((n2, n1));
    }
    if t.len() >= 6 && t.to_ascii_lowercase().starts_with("offset") {
        let (off, tail2) = read_uint(t[6..].trim_start())?;
        if !rest_is_empty(tail2) {
            return None;
        }
        return Some((n1, off));
    }
    // LIMIT 后面还有无法识别的内容（如 `LIMIT ?`、表达式）→ 保守处理，不改写
    if !rest_is_empty(t) {
        return None;
    }
    Some((n1, 0))
}

/// 用新的 `LIMIT n [OFFSET m]` 替换顶层 LIMIT 子句，SQL 其余部分保持原样（含大小写与格式）
fn replace_top_limit(sql: &str, limit: u64, offset: u64) -> Option<String> {
    let idx = top_limit_index(sql)?;
    let prefix = sql[..idx].trim_end();
    Some(if offset > 0 {
        format!("{prefix}\nLIMIT {limit} OFFSET {offset}")
    } else {
        format!("{prefix}\nLIMIT {limit}")
    })
}

/// 读取开头的无符号整数，返回 `(值, 剩余部分)`
fn read_uint(s: &str) -> Option<(u64, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let v = s[..end].parse::<u64>().ok()?;
    Some((v, &s[end..]))
}

/// 尾部只剩空白 / 分号才认为解析完整
fn rest_is_empty(s: &str) -> bool {
    let t = s.trim().trim_end_matches(';').trim();
    t.is_empty()
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// 跳过引号包裹的字面量，返回**闭合引号**的下标（未闭合返回 None）
fn skip_quoted(b: &[u8], start: usize) -> Option<usize> {
    let q = b[start];
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == q => {
                // SQL 里 '' 表示转义的引号本身
                if i + 1 < b.len() && b[i + 1] == q {
                    i += 2;
                } else {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// 跳过 `/* ... */` 块注释，返回注释结束后的下标（未闭合返回 None）
fn skip_block_comment(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 2;
    while i + 1 < b.len() {
        if b[i] == b'*' && b[i + 1] == b'/' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
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

    /// 用户 SQL 自带 LIMIT 时：超过一页仍要按页取数（编辑 SQL 后执行也要能分页）
    #[test]
    fn paging_rewrites_user_limit_when_exceeds_page() {
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100", 1, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000"
        );
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100", 2, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000 OFFSET 1000"
        );
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100", 3, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 100 OFFSET 2000"
        );
        // 页码越界 → 空结果，不再把整段数据拉回来
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100", 9, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 0 OFFSET 8000"
        );
        // 一页放得下 → 尊重用户意图，不改写
        assert_eq!(append_paging("SELECT * FROM orders LIMIT 50", 1, 1000), None);
        assert_eq!(append_paging("SELECT * FROM orders LIMIT 1000", 1, 1000), None);
        // 单页行数调小后，原本一页放得下的 LIMIT 也要分页
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 500", 2, 100).unwrap(),
            "SELECT * FROM orders\nLIMIT 100 OFFSET 100"
        );
    }

    /// 用户自带的 OFFSET（含 MySQL 逗号形式）要叠加到分页偏移上
    #[test]
    fn paging_keeps_user_offset() {
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100 OFFSET 500", 1, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000 OFFSET 500"
        );
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 2100 OFFSET 500", 3, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 100 OFFSET 2500"
        );
        // LIMIT <offset>, <count>
        assert_eq!(
            append_paging("SELECT * FROM orders LIMIT 500, 2100", 1, 1000).unwrap(),
            "SELECT * FROM orders\nLIMIT 1000 OFFSET 500"
        );
    }

    /// 只改顶层 LIMIT：子查询 / 字符串 / 注释里的 LIMIT 不能被误伤
    #[test]
    fn paging_only_rewrites_top_level_limit() {
        assert_eq!(
            append_paging(
                "SELECT id, (SELECT x FROM t2 LIMIT 1) AS y FROM t1 LIMIT 2100",
                2,
                1000
            )
            .unwrap(),
            "SELECT id, (SELECT x FROM t2 LIMIT 1) AS y FROM t1\nLIMIT 1000 OFFSET 1000"
        );
        assert_eq!(
            append_paging("SELECT * FROM t WHERE s = 'LIMIT 9' LIMIT 2100", 1, 1000).unwrap(),
            "SELECT * FROM t WHERE s = 'LIMIT 9'\nLIMIT 1000"
        );
        assert_eq!(
            append_paging("SELECT * FROM t -- LIMIT 5\nLIMIT 2100", 1, 1000).unwrap(),
            "SELECT * FROM t -- LIMIT 5\nLIMIT 1000"
        );
        // 括号内没有顶层 LIMIT 时仍走追加
        assert_eq!(
            append_paging("SELECT id, (SELECT x FROM t2 LIMIT 1) AS y FROM t1", 2, 500)
                .unwrap()
                .ends_with("LIMIT 500 OFFSET 500"),
            true
        );
    }

    /// 无法安全解析的 LIMIT（占位符 / 表达式）保持原样执行
    #[test]
    fn paging_skips_unparsable_limit() {
        assert_eq!(append_paging("SELECT * FROM t LIMIT ?", 1, 1000), None);
        assert_eq!(append_paging("SELECT * FROM t LIMIT 2100 FOR UPDATE", 1, 1000), None);
        assert_eq!(append_paging("SELECT * FROM t LIMIT", 1, 1000), None);
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
