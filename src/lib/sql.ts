/* ============================================================
 * sql.ts — SQL 格式化 / 语法高亮 / 只读校验（前端轻量预检）
 * ============================================================ */
import { format as fmt, type SqlLanguage } from "sql-formatter";

/** 使用 sql-formatter 格式化 SQL（MySQL 方言，结构清晰） */
export function formatSQL(raw: string): string {
  if (!raw) return "";
  try {
    return fmt(raw, {
      language: "mysql" as SqlLanguage,
      tabWidth: 2,
      keywordCase: "upper",
      logicalOperatorNewline: "before",
      expressionWidth: 60,
    });
  } catch {
    // 解析失败时退化为简单清理，保证可用
    return raw.replace(/\s+/g, " ").trim().replace(/;+\s*$/, "");
  }
}

/* ---------- SQL 语法高亮（单遍扫描：注释 -> 字符串 -> 数字 -> 函数 -> 关键字） ---------- */

const KEYWORDS = [
  "SELECT", "FROM", "WHERE", "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET",
  "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "CROSS", "ON", "AS", "AND", "OR", "NOT",
  "IN", "IS", "NULL", "LIKE", "BETWEEN", "CASE", "WHEN", "THEN", "ELSE", "END",
  "DISTINCT", "UNION", "ALL", "ASC", "DESC", "WITH", "EXISTS", "INTERVAL",
];
const FUNCS = [
  "SUM", "COUNT", "AVG", "MAX", "MIN", "ROUND", "IFNULL", "COALESCE",
  "DATE_FORMAT", "DATE", "NOW", "CURDATE", "CONCAT", "CAST", "SUBSTRING", "IF",
];
const KEY_RE = new RegExp(`\\b(${KEYWORDS.join("|")})\\b`, "i");

export function escHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

export function highlightSQL(sql: string): string {
  if (!sql) return "";
  const funcSet = new Set(FUNCS);
  const re =
    /(--[^\n]*)|('(?:[^'\\\n]|\\.)*')|(\b\d+(?:\.\d+)?\b)|\b([A-Za-z_]\w*)\s*(?=\()|([A-Za-z_]\w*)/g;
  let out = "";
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(sql)) !== null) {
    out += escHtml(sql.slice(last, m.index));
    if (m[1]) {
      out += `<span class="sql-c">${escHtml(m[1])}</span>`;
    } else if (m[2]) {
      out += `<span class="sql-s">${escHtml(m[2])}</span>`;
    } else if (m[3]) {
      out += `<span class="sql-n">${escHtml(m[3])}</span>`;
    } else if (m[4]) {
      out += funcSet.has(m[4].toUpperCase())
        ? `<span class="sql-f">${escHtml(m[4])}</span>` + escHtml(m[0].slice(m[4].length))
        : escHtml(m[0]);
    } else if (m[5]) {
      out += KEY_RE.test(m[5])
        ? `<span class="sql-k">${escHtml(m[5])}</span>`
        : escHtml(m[5]);
    }
    last = re.lastIndex;
  }
  out += escHtml(sql.slice(last));
  return out;
}

/** 前端预检：仅允许 SELECT / WITH 开头（后端仍会做 AST 级严格校验） */
export function looksLikeSelect(sql: string): boolean {
  return /^\s*(select|with)\b/i.test(sql);
}
