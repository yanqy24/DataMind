//! export.rs — 结果集导出：Excel（xlsx）与 JSON
//!
//! 设计要点：
//! 1. **与界面一致**：导出的列名、列顺序、行数完全来自前端传来的当前结果集，服务端不做二次裁剪。
//! 2. **类型可读**：Excel 在「数据」表之外额外输出「字段说明」表（列名 + MySQL 类型）；
//!    JSON 中数值写为 number、NULL 写为 null、其余写为 string，避免全部退化成字符串。
//! 3. **失败可控**：路径 / 扩展名 / 数据量做前置校验，写盘与序列化错误统一转成带 hint 的 AppError。

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::AppError;

/* ---------- 规模上限（防御性保护，正常结果集上限为 5000 行） ---------- */
/// 单次导出最大行数：超出直接报错，避免写出几百 MB 的文件把界面拖死
const MAX_EXPORT_ROWS: usize = 50_000;
/// 单个单元格写入 Excel 前的最大长度（Excel 单元格上限 32767）
const MAX_CELL_CHARS: usize = 30_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportColumn {
    pub name: String,
    #[serde(default)]
    pub r#type: String,
}

/// 结果来源（连接 / 库 / SQL），写入导出文件便于溯源
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExportSource {
    #[serde(default)]
    pub conn: String,
    #[serde(default)]
    pub db: String,
    #[serde(default)]
    pub sql: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPayload {
    pub columns: Vec<ExportColumn>,
    /// 行 -> 单元格（NULL 为 null）
    pub rows: Vec<Vec<Option<String>>>,
    #[serde(default)]
    pub source: Option<ExportSource>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStat {
    pub path: String,
    pub format: String,
    pub rows: usize,
    pub columns: usize,
    pub bytes: u64,
}

/// 统一出口：按 format 分派并做前置校验
pub fn export_to(path: &str, format: &str, payload: &ExportPayload) -> Result<ExportStat, AppError> {
    let kind = match format.trim().to_ascii_lowercase().as_str() {
        "xlsx" | "excel" => "xlsx",
        "json" => "json",
        other => {
            return Err(AppError::new(
                "EXPORT_FORMAT",
                format!("不支持的导出格式：{other}"),
            )
            .with_hint("目前支持 Excel（.xlsx）与 JSON（.json）"))
        }
    };

    if path.trim().is_empty() {
        return Err(AppError::new("EXPORT_PATH", "未选择保存路径"));
    }
    if payload.columns.is_empty() {
        return Err(AppError::new("EXPORT_EMPTY", "结果集没有列，无法导出"));
    }
    if payload.rows.is_empty() {
        return Err(AppError::new("EXPORT_EMPTY", "当前结果集没有数据行，无法导出"));
    }
    if payload.rows.len() > MAX_EXPORT_ROWS {
        return Err(
            AppError::new("EXPORT_TOO_LARGE", format!(
                "结果集 {} 行，超过单次导出上限 {} 行",
                fmt_usize(payload.rows.len()), MAX_EXPORT_ROWS
            ))
            .with_hint("请在 SQL 中缩小范围（加时间条件或聚合）后再导出"),
        );
    }

    // 扩展名校验：避免把 xlsx 内容写进 .json 之类的错位文件
    let lower = path.to_ascii_lowercase();
    let ext_ok = if kind == "xlsx" { lower.ends_with(".xlsx") } else { lower.ends_with(".json") };
    if !ext_ok {
        return Err(AppError::new(
            "EXPORT_EXT",
            format!("保存路径的扩展名与导出格式（{kind}）不匹配"),
        )
        .with_hint(if kind == "xlsx" {
            "请选择 .xlsx 结尾的文件名"
        } else {
            "请选择 .json 结尾的文件名"
        }));
    }

    let p = Path::new(path);
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            return Err(AppError::new("EXPORT_DIR", format!("目录不存在：{}", dir.display())));
        }
    }

    match kind {
        "xlsx" => write_xlsx(p, payload)?,
        _ => write_json(p, payload)?,
    }

    let bytes = std::fs::metadata(p)
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(ExportStat {
        path: path.to_string(),
        format: kind.to_string(),
        rows: payload.rows.len(),
        columns: payload.columns.len(),
        bytes,
    })
}

/* ---------- Excel ---------- */

fn write_xlsx(path: &Path, payload: &ExportPayload) -> Result<(), AppError> {
    use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook};

    let mut wb = Workbook::new();

    /* --- 表 1：数据 --- */
    let sheet = wb.add_worksheet();
    sheet.set_name("数据").map_err(xlsx_err("创建工作表失败"))?;

    let head_fmt = Format::new()
        .set_bold()
        .set_font_color(Color::RGB(0x1F_2937))
        .set_background_color(Color::RGB(0xE5_E7_EB))
        .set_border(FormatBorder::Thin)
        .set_align(FormatAlign::Center);
    let cell_fmt = Format::new().set_align(FormatAlign::Left).set_text_wrap();

    for (c, col) in payload.columns.iter().enumerate() {
        sheet
            .write_string_with_format(0, c as u16, &col.name, &head_fmt)
            .map_err(xlsx_err("写入表头失败"))?;
        sheet
            .set_column_width(c as u16, column_width(payload, c))
            .map_err(xlsx_err("设置列宽失败"))?;
    }

    for (r, row) in payload.rows.iter().enumerate() {
        let excel_row = (r + 1) as u32;
        for (c, _col) in payload.columns.iter().enumerate() {
            let v = row.get(c).cloned().flatten();
            match v {
                None => {
                    sheet
                        .write_blank(excel_row, c as u16, &cell_fmt)
                        .map_err(xlsx_err("写入空单元格失败"))?;
                }
                Some(s) => {
                    let s = clip_cell(&s);
                    match numeric(&s) {
                        Some(n) => {
                            sheet
                                .write_number_with_format(excel_row, c as u16, n, &cell_fmt)
                                .map_err(xlsx_err("写入数值失败"))?;
                        }
                        None => {
                            // 以 '=' '+' '-' '@' 开头的值会被 Excel 当公式，前置单引号保持原样
                            let safe = if starts_like_formula(&s) {
                                format!("'{s}")
                            } else {
                                s
                            };
                            sheet
                                .write_string_with_format(excel_row, c as u16, &safe, &cell_fmt)
                                .map_err(xlsx_err("写入单元格失败"))?;
                        }
                    }
                }
            }
        }
    }

    let last_row = payload.rows.len() as u32; // 表头占第 1 行，数据到 last_row
    let last_col = (payload.columns.len().saturating_sub(1)) as u16;
    sheet
        .autofilter(0, 0, last_row, last_col)
        .map_err(xlsx_err("设置筛选失败"))?;
    sheet
        .set_freeze_panes(1, 0)
        .map_err(xlsx_err("冻结表头失败"))?;

    /* --- 表 2：字段说明（列名 + 数据类型，保证类型可读） --- */
    let meta = wb.add_worksheet();
    meta.set_name("字段说明").map_err(xlsx_err("创建字段说明表失败"))?;
    meta.write_string_with_format(0, 0, "列名", &head_fmt)
        .map_err(xlsx_err("写入字段说明失败"))?;
    meta.write_string_with_format(0, 1, "数据类型", &head_fmt)
        .map_err(xlsx_err("写入字段说明失败"))?;
    meta.write_string_with_format(0, 2, "示例值", &head_fmt)
        .map_err(xlsx_err("写入字段说明失败"))?;
    for (i, col) in payload.columns.iter().enumerate() {
        let r = (i + 1) as u32;
        meta.write_string_with_format(r, 0, &col.name, &cell_fmt)
            .map_err(xlsx_err("写入字段说明失败"))?;
        meta.write_string_with_format(r, 1, if col.r#type.is_empty() { "-" } else { &col.r#type }, &cell_fmt)
            .map_err(xlsx_err("写入字段说明失败"))?;
        let sample = payload
            .rows
            .iter()
            .take(3)
            .filter_map(|row| row.get(i).cloned().flatten())
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default();
        meta.write_string_with_format(r, 2, &clip_cell(&sample), &cell_fmt)
            .map_err(xlsx_err("写入字段说明失败"))?;
    }
    meta.set_column_width(0, 24)
        .map_err(xlsx_err("设置列宽失败"))?;
    meta.set_column_width(1, 20)
        .map_err(xlsx_err("设置列宽失败"))?;
    meta.set_column_width(2, 32)
        .map_err(xlsx_err("设置列宽失败"))?;

    wb.save(path).map_err(xlsx_err("保存 Excel 文件失败"))
}

fn xlsx_err(ctx: &str) -> impl Fn(rust_xlsxwriter::XlsxError) -> AppError + '_ {
    move |e: rust_xlsxwriter::XlsxError| {
        AppError::new("EXPORT_XLSX", format!("{ctx}：{e}"))
            .with_hint("请确认文件未被 Excel 打开占用，或换一个保存位置重试")
    }
}

/// 列宽：取表头与少量样本的最大显示宽度，夹在 10–42 之间
fn column_width(payload: &ExportPayload, ci: usize) -> f64 {
    let mut max = payload
        .columns
        .get(ci)
        .map(|c| display_width(&c.name))
        .unwrap_or(10);
    for row in payload.rows.iter().take(50) {
        if let Some(Some(v)) = row.get(ci) {
            max = max.max(display_width(v));
        }
    }
    (max as f64).clamp(10.0, 42.0)
}

/// 显示宽度：中文按 2 个字符宽计
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if c.is_ascii() { 1 } else { 2 })
        .sum()
}

fn clip_cell(s: &str) -> String {
    if s.chars().count() <= MAX_CELL_CHARS {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX_CELL_CHARS).collect();
        out.push_str("…(已截断)");
        out
    }
}

fn starts_like_formula(s: &str) -> bool {
    matches!(s.chars().next(), Some('=') | Some('+') | Some('-') | Some('@'))
}

/* ---------- JSON ---------- */

fn write_json(path: &Path, payload: &ExportPayload) -> Result<(), AppError> {
    let cols: Vec<serde_json::Value> = payload
        .columns
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "type": if c.r#type.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(c.r#type.clone()) },
            })
        })
        .collect();

    let rows: Vec<serde_json::Value> = payload
        .rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in payload.columns.iter().enumerate() {
                let key = if obj.contains_key(&col.name) {
                    // 重名列（如两个 id 列）加序号后缀，避免覆盖
                    format!("{}#{}", col.name, i + 1)
                } else {
                    col.name.clone()
                };
                let v = row.get(i).cloned().flatten();
                obj.insert(key, json_cell(v.as_deref()));
            }
            serde_json::Value::Object(obj)
        })
        .collect();

    let src = payload.source.clone().unwrap_or_default();
    let doc = serde_json::json!({
        "exportedAt": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "tool": "DataMind",
        "source": {
            "connection": src.conn,
            "database": src.db,
            "sql": src.sql,
        },
        "columns": cols,
        "rowCount": rows.len(),
        "rows": rows,
    });

    let text = serde_json::to_string_pretty(&doc)
        .map_err(|e| AppError::new("EXPORT_JSON", format!("JSON 序列化失败：{e}")))?;
    std::fs::write(path, text.as_bytes()).map_err(|e| {
        AppError::new("EXPORT_JSON", format!("写入文件失败：{e}"))
            .with_hint("请确认目录可写、文件未被占用，或换一个保存位置重试")
    })
}

fn json_cell(v: Option<&str>) -> serde_json::Value {
    match v {
        None => serde_json::Value::Null,
        Some(s) => match numeric(s) {
            Some(n) => serde_json::Value::from(n),
            None => serde_json::Value::String(s.to_string()),
        },
    }
}

/* ---------- 数值判定 ---------- */

/// 判定单元格是否为数值：用于 Excel/JSON 保留数值形态。
/// 刻意保守——带前导零的编码（007、00123）与超长整数（>15 位，会丢精度）都保持字符串。
fn numeric(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > 15 {
        return None;
    }
    let bytes = t.as_bytes();
    if !matches!(bytes[0], b'0'..=b'9' | b'-' | b'+' | b'.') {
        return None;
    }
    if t.len() > 1 && t.starts_with('0') && !matches!(bytes[1], b'.') {
        return None;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
    {
        return None;
    }
    t.parse::<f64>().ok()
}

fn fmt_usize(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.insert(0, ',');
        }
        out.insert(0, ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> ExportPayload {
        ExportPayload {
            columns: vec![
                ExportColumn { name: "id".into(), r#type: "bigint".into() },
                ExportColumn { name: "name".into(), r#type: "varchar(32)".into() },
                ExportColumn { name: "amount".into(), r#type: "decimal(10,2)".into() },
            ],
            rows: vec![
                vec![Some("1".into()), Some("张三".into()), Some("12.50".into())],
                vec![Some("2".into()), None, Some("007".into())],
            ],
            source: Some(ExportSource { conn: "local".into(), db: "shop".into(), sql: "SELECT * FROM t".into() }),
        }
    }

    #[test]
    fn numeric_rules() {
        assert_eq!(numeric("12.5"), Some(12.5));
        assert_eq!(numeric("-3"), Some(-3.0));
        assert_eq!(numeric("007"), None, "前导零应保持字符串");
        assert_eq!(numeric("123456789012345678"), None, "超长整数保持字符串");
        assert_eq!(numeric("abc"), None);
        assert_eq!(numeric("2026-10-28"), None);
        assert_eq!(numeric(""), None);
    }

    #[test]
    fn exports_xlsx_and_json() {
        let dir = std::env::temp_dir().join(format!("dm-export-{}", chrono::Utc::now().timestamp_millis()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = payload();

        let xlsx = dir.join("r.xlsx");
        let stat = export_to(xlsx.to_str().unwrap(), "xlsx", &p).unwrap();
        assert_eq!(stat.rows, 2);
        assert!(std::fs::metadata(&xlsx).unwrap().len() > 0);
        // xlsx 本质是 zip：校验本地文件头
        let mut head = [0u8; 2];
        use std::io::Read;
        std::fs::File::open(&xlsx).unwrap().read_exact(&mut head).unwrap();
        assert_eq!(&head, b"PK", "xlsx 应为 zip 容器");

        let js = dir.join("r.json");
        let stat = export_to(js.to_str().unwrap(), "json", &p).unwrap();
        assert_eq!(stat.rows, 2);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&js).unwrap()).unwrap();
        assert_eq!(v["rowCount"], 2);
        assert_eq!(v["rows"][0]["id"], 1.0, "数值列应还原为 number");
        assert_eq!(v["rows"][0]["amount"], 12.5);
        assert!(v["rows"][1]["name"].is_null(), "NULL 应写为 null");
        assert_eq!(v["columns"][1]["type"], "varchar(32)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bad_input() {
        let dir = std::env::temp_dir();
        let p = payload();
        assert_eq!(export_to(dir.join("a.xlsx").to_str().unwrap(), "csv", &p).unwrap_err().code, "EXPORT_FORMAT");
        assert_eq!(export_to(dir.join("a.txt").to_str().unwrap(), "xlsx", &p).unwrap_err().code, "EXPORT_EXT");
        assert_eq!(export_to("", "json", &p).unwrap_err().code, "EXPORT_PATH");
        let empty = ExportPayload { columns: vec![], rows: vec![], source: None };
        assert_eq!(export_to(dir.join("a.json").to_str().unwrap(), "json", &empty).unwrap_err().code, "EXPORT_EMPTY");
    }
}
