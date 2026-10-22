//! 诊断工具：区分「DDL / 注释里的中文问号」是服务端转码丢失，还是客户端读取路径问题
//!
//! 用法：cargo run --release --example probe_ddl -- <连接名> [库名] [表名]
//! 只读（SHOW / SELECT），不改任何数据。

use std::sync::Arc;

use mysql_async::prelude::Queryable;
use mysql_async::{OptsBuilder, Pool, Row, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use russh::client::{self, Handle};
use russh::ChannelMsg;

struct H;
#[async_trait::async_trait]
impl client::Handler for H {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh_keys::key::PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn ssh_tunnel(
    host: &str,
    port: u16,
    user: &str,
    secret: &str,
    target_host: &str,
    target_port: u16,
) -> Result<u16, Box<dyn std::error::Error>> {
    let mut session: Handle<H> =
        client::connect(Arc::new(client::Config::default()), (host, port), H).await?;
    if !session.authenticate_password(user, secret).await? {
        return Err("SSH 认证失败".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local = listener.local_addr()?.port();
    let th = target_host.to_string();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { break };
            let Ok(ch) = session
                .channel_open_direct_tcpip(&th, target_port as u32, "127.0.0.1", 0)
                .await
            else {
                continue;
            };
            tokio::spawn(pump(tcp, ch, CancellationToken::new()));
        }
    });
    Ok(local)
}

async fn pump(mut tcp: TcpStream, mut ch: russh::Channel<russh::client::Msg>, tk: CancellationToken) {
    let mut buf = vec![0u8; 8192];
    loop {
        tokio::select! {
            biased;
            _ = tk.cancelled() => break,
            r = tcp.read(&mut buf) => match r {
                Ok(0) => { let _ = ch.eof().await; break; }
                Ok(n) => { if ch.data(&buf[..n]).await.is_err() { break; } }
                Err(_) => break,
            },
            m = ch.wait() => match m {
                Some(ChannelMsg::Data { ref data }) => { if tcp.write_all(data).await.is_err() { break; } }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
        }
    }
    let _ = ch.close().await;
    let _ = tcp.shutdown().await;
}

fn s(r: &Row, i: usize) -> String {
    match r.as_ref(i) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(v) => format!("{v:?}"),
        None => String::new(),
    }
}

fn hex_preview(b: &[u8], max: usize) -> String {
    b.iter()
        .take(max)
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 把 HEX() 的十六进制串解回字符串（服务端算出来的字节，绕过传输层转码）
fn from_hex(h: &str) -> Vec<u8> {
    let cs: Vec<char> = h.chars().collect();
    cs.chunks(2)
        .filter_map(|p| {
            if p.len() < 2 {
                return None;
            }
            u8::from_str_radix(&format!("{}{}", p[0], p[1]), 16).ok()
        })
        .collect()
}

fn describe(label: &str, text: &str, hex: &str) {
    let raw = from_hex(hex);
    let utf8 = std::str::from_utf8(&raw);
    println!("\n--- {label} ---");
    println!("  文本   : {}", text);
    println!("  字节   : [{}{}]", hex_preview(&raw, 48), if raw.len() > 48 { " ..." } else { "" });
    println!(
        "  严格UTF8: {}",
        match utf8 {
            Ok(v) => format!("OK → {v}"),
            Err(e) => format!("非法（第 {} 字节处）", e.valid_up_to()),
        }
    );
    let all_q = !raw.is_empty() && raw.iter().all(|x| *x == b'?');
    println!(
        "  判定   : {}",
        if all_q {
            "❌ 全部是 0x3F('?') —— 写入时已被服务端转码丢失，只能重新导注释"
        } else if raw.iter().any(|x| *x == b'?') {
            "⚠ 含 0x3F —— 部分字符在写入/读取时被替换为问号"
        } else if utf8.is_ok() {
            "✅ 合法 UTF-8，中文完好"
        } else {
            "⚠ 非法 UTF-8，可能是 GBK/GB18030 原始字节"
        }
    );
}

fn cjk_count(t: &str) -> usize {
    t.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count()
}

/// 扫描某库全部注释：用 App 的真实解码逻辑处理，给出「修复前 → 修复后」对照
async fn scan_comments(
    conn: &mut mysql_async::Conn,
    db: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let q = format!(
        "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_COMMENT, HEX(COLUMN_COMMENT) \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA='{}' AND COLUMN_COMMENT <> '' LIMIT 20000",
        db.replace('\'', "''")
    );
    let rows: Vec<Row> = conn.query(q).await?;
    let (mut with_cjk, mut changed, mut qmark, mut still_bad) = (0usize, 0usize, 0usize, 0usize);
    let mut samples: Vec<(String, String, String)> = Vec::new();
    let mut leftovers: Vec<String> = Vec::new();
    for r in &rows {
        let raw = from_hex(&s(r, 3));
        let before = String::from_utf8_lossy(&raw).into_owned();
        let after = datamind_lib::decode_diag(&raw);
        if before.contains('?') {
            qmark += 1;
        }
        if cjk_count(&before) == 0 {
            continue;
        }
        with_cjk += 1;
        if after != before {
            changed += 1;
            if samples.len() < 15 {
                samples.push((s(r, 0), before.clone(), after.clone()));
            }
        } else if before.chars().any(|c| ('\u{3400}'..='\u{4DBF}').contains(&c))
            || before.contains('\u{FFFD}')
        {
            still_bad += 1;
            if leftovers.len() < 10 {
                leftovers.push(format!("{}.{}: {before}", s(r, 0), s(r, 1)));
            }
        }
    }
    println!("\n=== [{db}] 注释扫描：共 {} 条非空注释 ===", rows.len());
    println!("含中文 {with_cjk} 条；解码后被修正 {changed} 条；含 '?' 的 {qmark} 条；疑似没救回来 {still_bad} 条");
    println!("\n-- 修复对照（表.列：修复前 → 修复后）--");
    for (t, a, b) in &samples {
        println!("  {t}\n     {a}  →  {b}");
    }
    if !leftovers.is_empty() {
        println!("\n-- 仍可能是乱码（需人工确认）--");
        for x in &leftovers {
            println!("  {x}");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let want = std::env::args().nth(1).unwrap_or_else(|| "本地数据库".into());
    let fix_db = std::env::args().nth(2);
    let fix_table = std::env::args().nth(3);

    let dir = std::path::PathBuf::from(std::env::var("APPDATA")?).join("com.datamind.app");
    let conns: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("connections.json"))?)?;
    let c = conns
        .iter()
        .find(|c| c["name"].as_str() == Some(want.as_str()))
        .ok_or("找不到该连接")?;

    let host = c["host"].as_str().unwrap_or("").to_string();
    let port = c["port"].as_u64().unwrap_or(3306) as u16;
    let user = c["user"].as_str().unwrap_or("").to_string();
    let pass = c["password"].as_str().unwrap_or("").to_string();
    let ssh = &c["ssh"];

    let (chost, cport) = if ssh["enabled"].as_bool().unwrap_or(false) {
        let p = ssh_tunnel(
            ssh["host"].as_str().unwrap_or(""),
            ssh["port"].as_u64().unwrap_or(22) as u16,
            ssh["user"].as_str().unwrap_or(""),
            ssh["secret"].as_str().unwrap_or(""),
            &host,
            port,
        )
        .await?;
        println!("SSH 隧道就绪 → 127.0.0.1:{p}");
        ("127.0.0.1".to_string(), p)
    } else {
        (host.clone(), port)
    };

    let pool = Pool::new(
        OptsBuilder::default()
            .ip_or_hostname(chost)
            .tcp_port(cport)
            .user(Some(user))
            .pass(Some(pass))
            .prefer_socket(false),
    );
    let mut conn = pool.get_conn().await?;
    conn.query_drop("SET NAMES utf8mb4").await?;

    let rows: Vec<Row> = conn
        .query("SELECT @@version, @@character_set_client, @@character_set_results, @@character_set_server")
        .await?;
    println!(
        "\n会话: version={} client={} results={} server={}",
        s(&rows[0], 0),
        s(&rows[0], 1),
        s(&rows[0], 2),
        s(&rows[0], 3)
    );

    /* 0) 指定库时：全库注释扫描 */
    if let Some(db) = &fix_db {
        scan_comments(&mut conn, db).await?;
    }

    /* 1) 找一个确实含 '?' 注释的表 */
    let mut sql = String::from(
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_COMMENT, HEX(COLUMN_COMMENT) \
         FROM information_schema.COLUMNS \
         WHERE COLUMN_COMMENT LIKE '%?%' \
           AND TABLE_SCHEMA NOT IN ('information_schema','mysql','performance_schema','sys')",
    );
    if let Some(db) = &fix_db {
        sql.push_str(&format!(" AND TABLE_SCHEMA = '{}'", db.replace('\'', "''")));
    }
    if let Some(t) = &fix_table {
        sql.push_str(&format!(" AND TABLE_NAME = '{}'", t.replace('\'', "''")));
    }
    sql.push_str(" LIMIT 5");
    let rows: Vec<Row> = conn.query(sql).await?;
    if rows.is_empty() {
        println!("\n✅ information_schema 中没有任何含 '?' 的列注释");
    }
    for r in &rows {
        let (db, tb, col) = (s(r, 0), s(r, 1), s(r, 2));
        describe(
            &format!("列注释 {db}.{tb}.{col}"),
            &s(r, 3),
            &s(r, 4),
        );
    }

    /* 2) 表注释 + 表 DDL（同一张表） */
    if let Some(r) = rows.first() {
        let db = s(r, 0);
        let tb = s(r, 1);

        let q = format!(
            "SELECT TABLE_COMMENT, HEX(TABLE_COMMENT) FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA='{}' AND TABLE_NAME='{}'",
            db.replace('\'', "''"),
            tb.replace('\'', "''")
        );
        if let Ok(v) = conn.query::<Row, _>(q).await {
            if let Some(rr) = v.first() {
                describe(&format!("表注释 {db}.{tb}"), &s(rr, 0), &s(rr, 1));
            }
        }

        let q = format!(
            "SHOW CREATE TABLE `{}`.`{}`",
            db.replace('`', "``"),
            tb.replace('`', "``")
        );
        if let Ok(v) = conn.query::<Row, _>(q).await {
            if let Some(rr) = v.first() {
                let ddl = match rr.as_ref(1) {
                    Some(Value::Bytes(b)) => b.to_vec(),
                    _ => Vec::new(),
                };
                println!("\n=== SHOW CREATE TABLE（{db}.{tb}，共 {} 字节）===", ddl.len());
                println!("{}", String::from_utf8_lossy(&ddl[..ddl.len().min(600)]));
                println!("首 64 字节: [{}]", hex_preview(&ddl, 64));
                let qm = ddl.iter().filter(|x| **x == b'?').count();
                println!("'?' 字节数: {qm} / {}", ddl.len());
                println!("\n=== 修复后（App 实际显示效果）===");
                println!("{}", datamind_lib::decode_diag(&ddl));
            }
        }
    }

    /* 3) 对照：换 latin1 结果集看 DDL 是否变成问号（验证是不是「服务端转码」） */
    conn.query_drop("SET NAMES latin1").await?;
    if let Some(r) = rows.first() {
        let db = s(r, 0);
        let tb = s(r, 1);
        let q = format!(
            "SHOW CREATE TABLE `{}`.`{}`",
            db.replace('`', "``"),
            tb.replace('`', "``")
        );
        if let Ok(v) = conn.query::<Row, _>(q).await {
            if let Some(rr) = v.first() {
                if let Some(Value::Bytes(b)) = rr.as_ref(1) {
                    println!("\n=== 对照：结果集 latin1 时的 DDL 前 300 字节 ===");
                    println!("{}", String::from_utf8_lossy(&b[..b.len().min(300)]));
                }
            }
        }
    }
    conn.query_drop("SET NAMES utf8mb4").await?;

    conn.disconnect().await?;
    Ok(())
}
