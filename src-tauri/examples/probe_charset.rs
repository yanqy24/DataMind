//! 一次性诊断工具：确认连接会话字符集与数据字节的真实情况（只读，SELECT only）
//!
//! 用法：cargo run --release --example probe_charset -- <连接名> [每库最多扫描表数]
//! 从应用数据目录读取 connections.json，支持 SSH 隧道（与 App 同款实现）。

use std::collections::BTreeSet;
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
    let mut session: Handle<H> = client::connect(Arc::new(client::Config::default()), (host, port), H).await?;
    if !session.authenticate_password(user, secret).await? {
        return Err("SSH 认证失败".into());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local = listener.local_addr()?.port();
    let th = target_host.to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else { break };
            let Ok(ch) = session.channel_open_direct_tcpip(&th, target_port as u32, "127.0.0.1", 0).await else {
                let _ = tcp.shutdown().await;
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

fn cp1252_to_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u <= 0xFF { return Some(u as u8); }
    const T: [(u32, u8); 27] = [
        (0x20AC,0x80),(0x201A,0x82),(0x0192,0x83),(0x201E,0x84),(0x2026,0x85),
        (0x2020,0x86),(0x2021,0x87),(0x02C6,0x88),(0x2030,0x89),(0x0160,0x8A),
        (0x2039,0x8B),(0x0152,0x8C),(0x017D,0x8E),(0x2018,0x91),(0x2019,0x92),
        (0x201C,0x93),(0x201D,0x94),(0x2022,0x95),(0x2013,0x96),(0x2014,0x97),
        (0x02DC,0x98),(0x2122,0x99),(0x0161,0x9A),(0x203A,0x9B),(0x0153,0x9C),
        (0x017E,0x9E),(0x0178,0x9F),
    ];
    T.iter().find(|(k, _)| *k == u).map(|(_, b)| *b)
}

fn has_cjk(t: &str) -> bool { t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) }

fn repair_double_encoded(s: &str) -> Option<String> {
    fn once(s: &str) -> Option<String> {
        if s.is_empty() || has_cjk(s) || s.chars().all(|c| c.is_ascii()) { return None; }
        let mut bytes = Vec::with_capacity(s.len());
        for c in s.chars() { bytes.push(cp1252_to_byte(c)?); }
        if bytes.iter().all(|b| b.is_ascii()) { return None; }
        match std::str::from_utf8(&bytes) {
            Ok(t) if has_cjk(t) => Some(t.to_string()),
            _ => None,
        }
    }
    once(s).or_else(|| once(s).and_then(|x| once(&x)))
}

fn s(r: &Row, i: usize) -> String {
    match r.as_ref(i) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(v) => format!("{v:?}"),
        None => String::new(),
    }
}

/// 典型「二次编码」痕迹：UTF-8 文本被当成 latin1 读取后再存 UTF-8
fn looks_mojibake(t: &str) -> bool {
    const SIGNS: [&str; 8] = ["Ã", "Â", "ä¸", "å", "æ", "è", "é", "Ã©"];
    SIGNS.iter().any(|s| t.contains(s)) && !t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// GBK 字节被当成 latin1 存进库：取回的字符串全部落在 U+0080..U+00FF，
/// 把每个字符还原成 1 字节后按 GBK 解码，若得到中文则判定为「GBK 乱码」
fn looks_gbk_in_latin1(t: &str) -> Option<String> {
    if t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return None;
    }
    if !t.chars().all(|c| (c as u32) <= 0xFF) || t.chars().all(|c| c.is_ascii()) {
        return None;
    }
    let bytes: Vec<u8> = t.chars().map(|c| c as u32 as u8).collect();
    let (s, _, _) = encoding_rs::GBK.decode(&bytes);
    if s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) && !s.contains('\u{FFFD}') {
        Some(s.into_owned())
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let want = std::env::args().nth(1).unwrap_or_else(|| "本地数据库".into());
    let max_tables: usize = std::env::args()
        .nth(2)
        .and_then(|x| x.parse().ok())
        .unwrap_or(150);
    let dir = std::path::PathBuf::from(std::env::var("APPDATA")?).join("com.datamind.app");
    let conns: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(dir.join("connections.json"))?)?;
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
        println!("SSH 隧道就绪 → 127.0.0.1:{p} (目标 {host}:{port})");
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

    let q = "SELECT @@version, @@character_set_client, @@character_set_connection, \
             @@character_set_results, @@character_set_server, @@collation_connection, @@init_connect";
    let rows: Vec<Row> = conn.query(q).await?;
    let r = &rows[0];
    println!("\n=== 握手后的会话字符集（App 当前行为）===");
    println!("version                  : {}", s(r, 0));
    println!("character_set_client     : {}", s(r, 1));
    println!("character_set_connection : {}", s(r, 2));
    println!("character_set_results    : {}", s(r, 3));
    println!("character_set_server     : {}", s(r, 4));
    println!("collation_connection     : {}", s(r, 5));
    println!("init_connect             : '{}'", s(r, 6));

    // 各库的默认字符集
    let rows: Vec<Row> = conn
        .query("SELECT SCHEMA_NAME, DEFAULT_CHARACTER_SET_NAME FROM information_schema.SCHEMATA")
        .await?;
    println!("\n=== 库默认字符集 ===");
    for r in &rows {
        println!("{:<28} {}", s(r, 0), s(r, 1));
    }

    // 表级字符集分布
    let rows: Vec<Row> = conn
        .query(
            "SELECT TABLE_SCHEMA, TABLE_COLLATION, COUNT(*) FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA NOT IN ('information_schema','mysql','performance_schema','sys') \
             GROUP BY 1,2 ORDER BY 3 DESC LIMIT 20",
        )
        .await?;
    println!("\n=== 表字符集分布（Top 20）===");
    for r in &rows {
        println!("{:<28} {:<24} {}", s(r, 0), s(r, 1), s(r, 2));
    }

    // 全库抽样：统计非 ASCII 值的三种异常
    let dbs: Vec<Row> = conn.query("SHOW DATABASES").await?;
    let dbs: Vec<String> = dbs
        .iter()
        .map(|r| s(r, 0))
        .filter(|d| !["information_schema", "mysql", "performance_schema", "sys"].contains(&d.as_str()))
        .collect();

    println!("\n=== 数据抽样（每库最多 {max_tables} 表，每表 5 行，只看含非 ASCII 的文本值）===");
    let mut bad: BTreeSet<String> = BTreeSet::new();
    let mut nonascii = 0usize;
    let censored;
    let mut scanned_tables = 0usize;
    for db in &dbs {
        let tq = format!("SHOW TABLES FROM `{}`", db.replace('`', "``"));
        let tables: Vec<Row> = match conn.query(tq).await {
            Ok(v) => v,
            Err(e) => {
                println!("[{db}] 读取表清单失败：{e}");
                continue;
            }
        };
        let total = tables.len();
        for tr in tables.iter().take(max_tables) {
            let t = s(tr, 0);
            let q = format!(
                "SELECT * FROM `{}`.`{}` LIMIT 5",
                db.replace('`', "``"),
                t.replace('`', "``")
            );
            let rows: Vec<Row> = match conn.query(q).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            scanned_tables += 1;
            for r in &rows {
                for (i, col) in r.columns_ref().iter().enumerate() {
                    if let Some(Value::Bytes(b)) = r.as_ref(i) {
                        if b.is_empty() || b.iter().all(|x| x.is_ascii()) {
                            continue;
                        }
                        nonascii += 1;
                        let as_utf8 = std::str::from_utf8(b);
                        let qmark = b.iter().all(|x| *x == b'?');
                        let qmark_run = as_utf8.map(|t| t.contains("??")).unwrap_or(false);
                        let moji = as_utf8.map(looks_mojibake).unwrap_or(false);
                        let gbk = as_utf8.ok().and_then(looks_gbk_in_latin1);
                        if as_utf8.is_err() || qmark || qmark_run || moji || gbk.is_some() {
                            let kind = if qmark {
                                "整列问号"
                            } else if qmark_run {
                                "含??连续问号"
                            } else if as_utf8.is_err() {
                                "非法UTF-8"
                            } else if gbk.is_some() {
                                "GBK字节读成latin1"
                            } else {
                                "疑似二次编码"
                            };
                            bad.insert(format!(
                                "{db}.{t}.{} | {kind} | 显示: {}{}",
                                col.name_str(),
                                String::from_utf8_lossy(&b[..b.len().min(40)]),
                                gbk.map(|g| format!(" | GBK还原: {g}"))
                                    .or_else(|| as_utf8.ok().and_then(repair_double_encoded).map(|r| format!(" | 二次编码还原: {r}")))
                                    .unwrap_or_default()
                            ));
                        }
                    }
                }
            }
        }
        println!("[{db}] 表 {total} 张，本次扫描 {} 张", total.min(max_tables));
    }
    censored = scanned_tables;
    println!("\n扫描表数 {censored}，含非 ASCII 的文本值 {nonascii} 个");
    if bad.is_empty() {
        println!("✅ 未发现非法 UTF-8 / 全问号 / 二次编码的数据");
    } else {
        println!("⚠ 发现 {} 处异常（最多列 25 条）：", bad.len());
        for b in bad.iter().take(25) {
            println!("{b}");
        }
    }

    conn.disconnect().await?;
    Ok(())
}
