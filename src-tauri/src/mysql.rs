//! mysql.rs — MySQL 连接管理：连接池 / SSH 端点 / 结构内省 / 只读查询执行
//!
//! 安全约束：
//! - 执行前经 [crate::guard] 的 AST 校验，仅放行 SELECT
//! - 单次执行受超时（默认 60s）与取消（run_id -> CancellationToken）双重控制；
//!   超时 / 取消都会主动断开该连接并向服务端补发 KILL QUERY，避免语句在库上继续跑
//! - SQL 原样下发：不改写用户语句（不追加 LIMIT），结果集完整返回；
//!   大结果集由前端分页渲染，见 ResultGrid.vue
//!
//! 并发模型：连接池 / 隧道的增删由一把**短临界区**互斥锁保护，只在取连接的那一刻持锁；
//! 查询与结构同步本身不持有任何全局锁，因此慢查询不会互相阻塞，也不会阻塞取消
//! （取消走独立的 [RunningRegistry]，不需要等待正在执行的查询）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{Column, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, Value};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::AppError;
use crate::guard;
use crate::models::*;
use crate::store::Store;
use crate::tunnel::Tunnel;

/* ---------- 采集上限（保护超大库） ---------- */
const MAX_DATABASES: usize = 60;
const MAX_TABLES_PER_DB: usize = 2000;
const SAMPLE_ROWS: usize = 2;
/// 右键「查看建表语句」时实时取样的行数
const SAMPLE_ROWS_LIVE: usize = 20;
/*
 * 历史说明：此前有过 `MAX_RESULT_ROWS = 5000` 的结果集硬截断，已按需求移除。
 * 现在由**服务端分页**控制：一次只取一页回客户端，单页行数上限 1000。
 */
/// 单页行数硬上限（防御性：即使前端传更大的值也按此截断，避免一次拉回过多行）
pub const MAX_PAGE_SIZE: u32 = 1000;

const SYSTEM_DATABASES: &[&str] = &["information_schema", "mysql", "performance_schema", "sys"];

/* ---------- 连接保活策略（应对长时间闲置后被单方面断开） ---------- */
/// TCP keepalive 间隔（秒）：空闲链路上发探测包，尽早发现已失效的连接
const TCP_KEEPALIVE_SECS: u32 = 60;
/// 池中闲置连接存活上限：超过则回收，避免拿到服务端已关闭的连接
const POOL_INACTIVE_TTL: Duration = Duration::from_secs(300);
/// 单条连接绝对存活上限（含抖动）：即便持续使用也定期换新，规避服务端 wait_timeout
const POOL_ABS_TTL: Duration = Duration::from_secs(1800);
const POOL_ABS_TTL_JITTER: Duration = Duration::from_secs(60);
/// 取连接 / 心跳校验超时
const CONN_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct DbManager {
    store: Arc<Store>,
    /// 连接池与 SSH 隧道。临界区只包住「查表 / 插表」，绝不跨 await 持有。
    conns: StdMutex<ConnPoolSet>,
    /// 建连闸门：同一条连接被并发请求时只建一次池、一次隧道
    connect: tokio::sync::Mutex<()>,
    /// run_id -> 取消令牌。独立于连接池，取消路径无需任何锁等待。
    running: RunningRegistry,
}

#[derive(Default)]
struct ConnPoolSet {
    pools: HashMap<String, Pool>,
    tunnels: HashMap<String, Tunnel>,
}

/// 运行中任务登记表（查询与 SQL 生成共用同一套 run_id 命名空间）。
///
/// 用 `std::sync::Mutex` 而不是 `tokio::sync::Mutex`：临界区里只做 HashMap 操作，
/// 不跨 await，且取消是同步语义——必须能在「数据库正忙」时立刻生效。
#[derive(Default)]
pub struct RunningRegistry {
    map: StdMutex<HashMap<String, CancellationToken>>,
}

impl RunningRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CancellationToken>> {
        // 锁中毒时沿用内部数据：这里存的是取消令牌，不存在被破坏的不变量
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn insert(&self, run_id: &str, token: CancellationToken) {
        self.lock().insert(run_id.to_string(), token);
    }

    fn remove(&self, run_id: &str) -> Option<CancellationToken> {
        self.lock().remove(run_id)
    }

    /// 取消指定任务；返回是否命中。命中即已发出信号，调用方无需等待任务收尾。
    pub fn cancel(&self, run_id: &str) -> bool {
        match self.lock().get(run_id) {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }
}

impl DbManager {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            conns: StdMutex::new(ConnPoolSet::default()),
            connect: tokio::sync::Mutex::new(()),
            running: RunningRegistry::default(),
        }
    }

    fn conns(&self) -> std::sync::MutexGuard<'_, ConnPoolSet> {
        self.conns.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn find_config(&self, conn_id: &str) -> Result<ConnConfig, AppError> {
        self.store
            .load_connections()?
            .into_iter()
            .find(|c| c.id == conn_id)
            .ok_or_else(|| AppError::new("NO_CONN", "连接不存在，请重新选择"))
    }

    /// 登记一个可取消任务（查询 / 生成共用），返回其取消令牌
    pub fn begin_run(&self, run_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.running.insert(run_id, token.clone());
        token
    }

    /// 注销任务登记（任务结束 / 失败 / 取消后都必须调用）
    pub fn end_run(&self, run_id: &str) {
        self.running.remove(run_id);
    }

    /// 取消指定 run_id 的执行（查询或生成）。
    ///
    /// 不触碰 `connect` 闸门、也不等待 DbManager 内部锁，因此**在长查询执行期间调用也能立刻返回**。
    pub fn cancel(&self, run_id: &str) -> bool {
        self.running.cancel(run_id)
    }

    /// 连接配置变化 / 删除后，销毁旧连接池与隧道
    pub fn reset(&self, conn_id: &str) {
        let (pool, tunnel) = {
            let mut set = self.conns();
            (set.pools.remove(conn_id), set.tunnels.remove(conn_id))
        };
        drop(pool);
        if let Some(t) = tunnel {
            t.stop();
        }
    }

    fn pool_of(&self, conn_id: &str) -> Option<Pool> {
        self.conns().pools.get(conn_id).cloned()
    }

    fn tunnel_port(&self, conn_id: &str) -> Option<u16> {
        self.conns().tunnels.get(conn_id).map(|t| t.local_port)
    }

    /// 取得 MySQL 访问端点（必要时建立 SSH 隧道）
    async fn endpoint(&self, cfg: &ConnConfig) -> Result<(String, u16), AppError> {
        if !cfg.ssh.enabled {
            return Ok((cfg.host.clone(), cfg.port));
        }
        if let Some(port) = self.tunnel_port(&cfg.id) {
            return Ok(("127.0.0.1".into(), port));
        }
        let t = Tunnel::start(&cfg.ssh, &cfg.host, cfg.port).await?;
        let port = t.local_port;
        // 并发下可能已被别的任务抢先建好隧道：保留先到的，后来者就地关闭
        let mut set = self.conns();
        let port = match set.tunnels.entry(cfg.id.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => o.get().local_port,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(t);
                port
            }
        };
        Ok(("127.0.0.1".into(), port))
    }

    /// 取得（或建立）连接池
    async fn ensure_pool(&self, conn_id: &str) -> Result<Pool, AppError> {
        if let Some(p) = self.pool_of(conn_id) {
            return Ok(p);
        }
        // 只有「建池 / 建隧道」走串行；拿到池后立刻释放闸门，查询之间互不阻塞
        let _gate = self.connect.lock().await;
        if let Some(p) = self.pool_of(conn_id) {
            return Ok(p);
        }
        let cfg = self.find_config(conn_id)?;
        let (host, port) = self.endpoint(&cfg).await?;
        let pool = Pool::new(build_opts(&host, port, &cfg));
        // 首次握手验证可用性（8s 内未连通则报错）
        let conn = tokio::time::timeout(Duration::from_secs(8), pool.get_conn())
            .await
            .map_err(|_| AppError::new("2003", "连接 MySQL 超时，请检查网络或 SSH 隧道"))??;
        drop(conn);
        self.conns().pools.insert(conn_id.to_string(), pool.clone());
        Ok(pool)
    }

    /// 取一条「活着」的连接：长时间闲置后，服务端 wait_timeout / 防火墙 / SSH 隧道
    /// 可能已单方面关闭连接，池中连接看似存在实则不可用。
    /// 这里先做一次心跳校验（SELECT 1），失败则销毁池与隧道后重建，再取一次。
    async fn healthy_conn(&self, conn_id: &str) -> Result<mysql_async::Conn, AppError> {
        let mut last = AppError::new("2003", "获取连接失败：连接不可用");
        for _attempt in 0..2u8 {
            let pool = match self.ensure_pool(conn_id).await {
                Ok(p) => p,
                Err(e) => {
                    last = e;
                    self.reset(conn_id); // 可能 SSH 隧道已断，重建
                    continue;
                }
            };
            match tokio::time::timeout(CONN_CHECK_TIMEOUT, pool.get_conn()).await {
                Ok(Ok(mut conn)) => {
                    if conn.query_drop("SELECT 1").await.is_ok() {
                        // 强制会话字符集为 UTF-8：若服务端 character_set_results 非 utf8（latin1 / ascii 等），
                        // 中文在服务端转码时会被替换成 '?'，这是「中文变问号」最常见的原因
                        ensure_utf8(&mut conn, conn_id).await;
                        return Ok(conn);
                    }
                    last = AppError::new("2013", "连接已被服务端关闭（可能闲置过久）").as_conn_broken();
                }
                Ok(Err(e)) => {
                    last = AppError::from(e);
                }
                Err(_) => {
                    last = AppError::new("2003", "获取连接超时，请检查网络或 SSH 隧道").as_conn_broken();
                }
            }
            // 走到这里说明连接不可用：销毁连接池（含隧道），下一轮重建
            trace_conn(&format!(
                "CONN_STALE conn={} err={} → 重建连接",
                conn_id, last.message
            ));
            self.reset(conn_id);
        }
        Err(last)
    }

    /// 测试连接（不落库，测试配置草稿）
    pub async fn test(&self, cfg: &ConnConfig) -> Result<u128, AppError> {
        let start = Instant::now();
        let mut tunnel = None;
        let (host, port) = if cfg.ssh.enabled {
            let t = Tunnel::start(&cfg.ssh, &cfg.host, cfg.port).await?;
            let p = t.local_port;
            tunnel = Some(t);
            ("127.0.0.1".to_string(), p)
        } else {
            (cfg.host.clone(), cfg.port)
        };
        let pool = Pool::new(build_opts(&host, port, cfg));
        let _conn = tokio::time::timeout(Duration::from_secs(8), pool.get_conn()).await??;
        drop(pool);
        drop(tunnel);
        Ok(start.elapsed().as_millis())
    }

    /* ---------- 结构内省（本地记忆） ---------- */

    /// 全量读取连接下所有库的 DDL / 列 / 关系 / 取样，形成本地记忆
    pub async fn sync_memory(
        &self,
        conn_id: &str,
        progress: &(dyn Fn(&str) + Send + Sync),
    ) -> Result<ConnMemory, AppError> {
        progress("连接数据库");
        let mut conn = self.healthy_conn(conn_id).await?;

        progress("读取数据库列表");
        let db_rows: Vec<Row> = conn.query("SHOW DATABASES").await?;
        let db_names: Vec<String> = db_rows
            .iter()
            .filter_map(|r| value_string(val(r, 0)))
            .filter(|n| !SYSTEM_DATABASES.iter().any(|s| s.eq_ignore_ascii_case(n)))
            .take(MAX_DATABASES)
            .collect();

        let mut databases = Vec::with_capacity(db_names.len());
        for db in &db_names {
            progress(&format!("解析表结构（{db}）"));
            databases.push(self.introspect_db(&mut conn, db).await?);
        }

        let mem = ConnMemory {
            conn_id: conn_id.to_string(),
            synced_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            databases,
        };
        self.store.save_memory(&mem)?;
        Ok(mem)
    }

    async fn introspect_db(
        &self,
        conn: &mut mysql_async::Conn,
        db: &str,
    ) -> Result<DatabaseMeta, AppError> {
        /* 表清单 */
        let table_rows: Vec<Row> = conn
            .exec(
                "SELECT TABLE_NAME, IFNULL(TABLE_ROWS,0), IFNULL(TABLE_COMMENT,'') \
                 FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_TYPE='BASE TABLE' \
                 ORDER BY TABLE_NAME",
                (db,),
            )
            .await?;
        let mut tables: Vec<TableMeta> = Vec::with_capacity(table_rows.len());
        for r in table_rows.iter().take(MAX_TABLES_PER_DB) {
            tables.push(TableMeta {
                name: value_string(val(r, 0)).unwrap_or_default(),
                comment: value_string(val(r, 2)).unwrap_or_default(),
                rows: value_u64(val(r, 1)),
                ddl: String::new(),
                columns: Vec::new(),
                sample: Vec::new(),
            });
        }

        /* 列（整个库一次取回，避免 N+1 查询） */
        let col_rows: Vec<Row> = conn
            .exec(
                "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IFNULL(COLUMN_COMMENT,'') \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
                (db,),
            )
            .await?;
        for r in &col_rows {
            let tname = value_string(val(r, 0)).unwrap_or_default();
            if let Some(t) = tables.iter_mut().find(|t| t.name == tname) {
                t.columns.push(ColumnMeta {
                    name: value_string(val(r, 1)).unwrap_or_default(),
                    r#type: value_string(val(r, 2)).unwrap_or_default(),
                    comment: value_string(val(r, 3)).unwrap_or_default(),
                });
            }
        }

        /* DDL + 取样 */
        for t in tables.iter_mut() {
            let q = format!(
                "SHOW CREATE TABLE `{}`.`{}`",
                db.replace('`', "``"),
                t.name.replace('`', "``")
            );
            if let Ok(rows) = conn.query::<Row, _>(q).await {
                if let Some(r) = rows.first() {
                    t.ddl = value_string(val(r, 1)).unwrap_or_default();
                }
            }
            if !t.columns.is_empty() {
                let q = format!(
                    "SELECT * FROM `{}`.`{}` LIMIT {SAMPLE_ROWS}",
                    db.replace('`', "``"),
                    t.name.replace('`', "``")
                );
                if let Ok(rows) = conn.query::<Row, _>(q).await {
                    for r in &rows {
                        let mut obj = serde_json::Map::new();
                        for (i, c) in r.columns_ref().iter().enumerate() {
                            obj.insert(
                                c.name_str().to_string(),
                                match val(r, i) {
                                    Value::NULL => serde_json::Value::Null,
                                    v => serde_json::Value::String(
                                        value_string(v).unwrap_or_default(),
                                    ),
                                },
                            );
                        }
                        t.sample.push(obj);
                    }
                }
            }
        }

        /* 关系（外键推导） */
        let rel_rows: Vec<Row> = conn
            .exec(
                "SELECT TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL",
                (db,),
            )
            .await?;
        let relations = rel_rows
            .iter()
            .map(|r| RelationMeta {
                from: format!(
                    "{}.{}",
                    value_string(val(r, 0)).unwrap_or_default(),
                    value_string(val(r, 1)).unwrap_or_default()
                ),
                to: format!(
                    "{}.{}",
                    value_string(val(r, 2)).unwrap_or_default(),
                    value_string(val(r, 3)).unwrap_or_default()
                ),
                desc: "外键".into(),
            })
            .collect();

        Ok(DatabaseMeta {
            name: db.to_string(),
            comment: String::new(),
            tables,
            relations,
        })
    }

    /* ---------- 单表详情 ---------- */

    /// 实时读取单表详情（建表语句 + 取样），右键「查看建表语句」用。
    ///
    /// 不走本地记忆：记忆可能是旧版本在错误字符集下采集的（当时注释已被服务端转成 '?'），
    /// 这里直接向数据库要最新文本，保证显示与当前库一致。
    pub async fn table_detail(
        &self,
        conn_id: &str,
        db: &str,
        table: &str,
    ) -> Result<TableDetail, AppError> {
        let mut conn = self.healthy_conn(conn_id).await?;
        let q = format!(
            "SHOW CREATE TABLE `{}`.`{}`",
            db.replace('`', "``"),
            table.replace('`', "``")
        );
        let rows: Vec<Row> = conn.query(q).await?;
        let ddl = rows
            .first()
            .and_then(|r| value_string(val(r, 1)))
            .unwrap_or_default();
        if ddl.trim().is_empty() {
            return Err(AppError::new("DDL_EMPTY", "未能读取该表的建表语句"));
        }

        let mut sample = Vec::new();
        let q = format!(
            "SELECT * FROM `{}`.`{}` LIMIT {SAMPLE_ROWS_LIVE}",
            db.replace('`', "``"),
            table.replace('`', "``")
        );
        if let Ok(rows) = conn.query::<Row, _>(q).await {
            for r in &rows {
                let mut obj = serde_json::Map::new();
                for (i, c) in r.columns_ref().iter().enumerate() {
                    obj.insert(
                        c.name_str().to_string(),
                        match val(r, i) {
                            Value::NULL => serde_json::Value::Null,
                            v => serde_json::Value::String(value_string(v).unwrap_or_default()),
                        },
                    );
                }
                sample.push(obj);
            }
        }
        Ok(TableDetail { ddl, sample })
    }

    /* ---------- SQL 执行 ---------- */

    /// 执行只读 SELECT（超时自动取消；run_id 可被 [Self::cancel] 手动取消）
    /// 若连接在执行途中被断开（闲置超时 / 网络中断），自动重建连接重试一次
    pub async fn execute(
        &self,
        conn_id: &str,
        db: &str,
        sql: &str,
        timeout_secs: u64,
        run_id: &str,
    ) -> Result<QueryResult, AppError> {
        guard::ensure_select_only(sql)?;
        // 原样下发：不追加 LIMIT，结果集完整返回（改写用户 SQL 会改变其语义与执行计划）
        let sql_capped = sql.to_string();

        // 令牌建在重试循环之外：用户取消对「重建连接后的第二次尝试」同样有效
        let token = self.begin_run(run_id);
        let mut last: Option<AppError> = None;
        for attempt in 0..2u8 {
            if token.is_cancelled() {
                self.end_run(run_id);
                return Err(AppError::new("CANCELLED", "已取消执行"));
            }
            let r = self
                .execute_once(conn_id, db, sql_capped.clone(), timeout_secs, &token)
                .await;
            match r {
                Err(e) if e.conn_broken && attempt == 0 && !token.is_cancelled() => {
                    trace_conn(&format!(
                        "CONN_BROKEN conn={} err={} → 重建连接并重试一次",
                        conn_id, e.message
                    ));
                    last = Some(e);
                    self.reset(conn_id); // 销毁失效连接池（含隧道），下一轮重建
                    continue;
                }
                other => {
                    self.end_run(run_id);
                    return other;
                }
            }
        }
        self.end_run(run_id);
        Err(last.unwrap_or_else(|| AppError::new("MYSQL", "执行失败")))
    }

    /// 服务端分页执行：只取第 `page` 页的 `page_size` 行回客户端。
    ///
    /// - 用户 SQL 自带 LIMIT / OFFSET 时**不改写**，原样执行（界面也不分页）；
    /// - 否则追加 `LIMIT page_size OFFSET (page-1)*page_size`，并额外统计一次总行数用于展示页码；
    ///   统计失败不影响翻页（此时 `total` 为 None，界面降级为「上一页 / 下一页」）。
    pub async fn execute_paged(
        &self,
        conn_id: &str,
        db: &str,
        sql: &str,
        timeout_secs: u64,
        run_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<QueryResult, AppError> {
        guard::ensure_select_only(sql)?;
        let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
        let page = page.max(1);
        let paged = guard::append_paging(sql, page, page_size);
        let run_sql = paged.as_deref().unwrap_or(sql);
        let mut res = self.execute(conn_id, db, run_sql, timeout_secs, run_id).await?;

        let total = if paged.is_some() {
            match guard::count_wrap(sql) {
                Some(count_sql) => {
                    // 独立 run_id：统计查询不占用主查询的取消令牌
                    let count_run = format!("{run_id}:count");
                    match self
                        .execute(conn_id, db, &count_sql, timeout_secs, &count_run)
                        .await
                    {
                        Ok(r) => r
                            .rows
                            .first()
                            .and_then(|row| row.first().cloned().flatten())
                            .and_then(|v| v.trim().parse::<u64>().ok()),
                        Err(e) => {
                            trace_conn(&format!("PAGE_COUNT_FAILED err={}", e.message));
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            Some(res.rows.len() as u64)
        };

        res.total = total;
        res.page = page;
        res.page_size = page_size;
        Ok(res)
    }

    /// 单次执行（不含重连重试）
    async fn execute_once(
        &self,
        conn_id: &str,
        db: &str,
        sql_capped: String,
        timeout_secs: u64,
        token: &CancellationToken,
    ) -> Result<QueryResult, AppError> {
        // 取连接期间用户可能已经取消（拿连接本身可能要等上几秒）
        if token.is_cancelled() {
            return Err(AppError::new("CANCELLED", "已取消执行"));
        }
        let mut conn = self.healthy_conn(conn_id).await?;
        if !db.is_empty() {
            conn.query_drop(format!("USE `{}`", db.replace('`', "``")))
                .await?;
        }

        // 服务端线程 id：超时 / 取消后用它发 KILL QUERY，让语句真正停下来
        let thread_id = conn.id();
        let start = Instant::now();
        let task_token = token.clone();
        let (tx, rx) = oneshot::channel::<Result<Vec<Row>, AppError>>();

        tokio::spawn(async move {
            // select! 先轮询取消分支：取消时立刻丢弃查询 future（等价于中断请求）
            let res: Option<Result<Vec<Row>, AppError>> = tokio::select! {
                biased;
                _ = task_token.cancelled() => None,
                r = conn.query::<Row, _>(&sql_capped) => Some(r.map_err(AppError::from)),
            };
            match res {
                Some(r) => {
                    let _ = tx.send(r);
                }
                None => {
                    // 被中断的查询会让协议流失去同步，这条连接不能还回池复用，直接断开
                    let _ = conn.disconnect().await;
                    let _ = tx.send(Err(AppError::new("CANCELLED", "已取消执行")));
                }
            }
        });

        let out = tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), rx).await;
        match out {
            Err(_) => {
                // 客户端超时：先通知任务中止，再补一刀 KILL QUERY
                // （此前这里第二次 remove 恒为 None，取消从未真正生效）
                token.cancel();
                self.kill_query(conn_id, thread_id);
                Err(AppError::new(
                    "TIMEOUT",
                    format!("执行超过 {timeout_secs}s，已自动取消"),
                ))
            }
            Ok(Err(_)) => Err(AppError::new("CANCELLED", "已取消执行")),
            Ok(Ok(Err(e))) => {
                // 手动取消同样要让服务端停下（令牌已由 cancel() 发出，这里补服务端侧）
                if e.code == "CANCELLED" {
                    self.kill_query(conn_id, thread_id);
                }
                Err(e)
            }
            Ok(Ok(Ok(rows))) => Ok(convert_rows(rows, start.elapsed().as_millis() as u64)),
        }
    }

    /// 请求服务端终止指定线程上的语句（尽力而为：权限不足或连接已断时静默失败）。
    ///
    /// 客户端断开 socket 未必能让 MySQL 停下正在执行的语句，必须显式 KILL QUERY。
    /// 这里丢到后台执行：取消 / 超时的响应不该再多等一个取连接的往返。
    fn kill_query(&self, conn_id: &str, thread_id: u32) {
        let Some(pool) = self.pool_of(conn_id) else {
            return;
        };
        let conn_id = conn_id.to_string();
        tokio::spawn(async move {
            let Ok(Ok(mut conn)) =
                tokio::time::timeout(Duration::from_secs(5), pool.get_conn()).await
            else {
                trace_conn(&format!("KILL_QUERY conn={conn_id} 取连接失败，跳过"));
                return;
            };
            match conn.query_drop(format!("KILL QUERY {thread_id}")).await {
                Ok(_) => trace_conn(&format!(
                    "KILL_QUERY conn={conn_id} thread={thread_id} 已发送"
                )),
                Err(e) => trace_conn(&format!(
                    "KILL_QUERY conn={conn_id} thread={thread_id} 失败：{e}"
                )),
            }
        });
    }

    /* ---------- 数据探查（Agent 模式：生成 SQL 前主动摸清真实数据） ---------- */

    /// 跑一条只读探查 SQL（走同一套超时/取消/只读保护）
    pub async fn probe_query(
        &self,
        conn_id: &str,
        db: &str,
        sql: &str,
        timeout_secs: u64,
    ) -> Result<QueryResult, AppError> {
        let run_id = format!("probe:{}:{}", conn_id, chrono::Utc::now().timestamp_millis());
        self.execute(conn_id, db, sql, timeout_secs, &run_id).await
    }

    /// 用 EXPLAIN 验证 SQL 可执行（字段/表是否存在）。
    ///
    /// 注意：EXPLAIN **不总是只读**——MySQL 8.0.18+ 的 `EXPLAIN ANALYZE <DML>` 会真正执行语句。
    /// 因此这里必须自己再校验一次，不能依赖调用方「应该已经校验过」（纵深防御）。
    pub async fn explain(&self, conn_id: &str, db: &str, sql: &str) -> Result<(), AppError> {
        guard::ensure_select_only(sql)?;
        let mut conn = self.healthy_conn(conn_id).await?;
        if !db.is_empty() {
            conn.query_drop(format!("USE `{}`", db.replace('`', "``")))
                .await?;
        }
        // 固定用不带 ANALYZE 的 EXPLAIN，保证不会真实执行语句
        let q = format!("EXPLAIN {}", sql);
        tokio::time::timeout(Duration::from_secs(15), conn.query_drop(q))
            .await
            .map_err(|_| AppError::new("EXPLAIN_TIMEOUT", "执行计划检查超时"))?
            .map_err(AppError::from)
    }
}

/* ---------- 值转换 ---------- */

/// 连接自愈留痕：写入数据目录 diagnostic.log（无控制台环境下便于排查闲置断连问题）
fn trace_conn(msg: &str) {
    use std::io::Write;
    if let Some(dir) = crate::DATA_DIR.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("diagnostic.log"))
        {
            let _ = writeln!(
                f,
                "[{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                msg
            );
        }
    }
}

/// 构造 MySQL 连接参数：启用 TCP keepalive，并限制池中 / 单条连接的存活时长，
/// 避免长时间闲置后拿到一条已被服务端静默关闭的连接。
fn build_opts(host: &str, port: u16, cfg: &ConnConfig) -> OptsBuilder {
    OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some(cfg.user.clone()))
        .pass(Some(cfg.password.clone()))
        .prefer_socket(false)
        .tcp_keepalive(Some(TCP_KEEPALIVE_SECS))
        .pool_opts(
            PoolOpts::default()
                .with_constraints(PoolConstraints::new(0, 5).expect("pool constraints 0..5"))
                .with_inactive_connection_ttl(POOL_INACTIVE_TTL)
                .with_abs_conn_ttl(Some(POOL_ABS_TTL))
                .with_abs_conn_ttl_jitter(Some(POOL_ABS_TTL_JITTER)),
        )
}

/* ---------- 字符集保障 ---------- */

/// 已经打印过字符集诊断的连接（避免每条连接重复写日志）
static CHARSET_LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();
/// 非 UTF-8 字节的出现情况（按「连接+列」去重，最多记录 40 条）
static CHARSET_MISS_LOGGED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn charset_logged() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    CHARSET_LOGGED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}
fn miss_logged() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    CHARSET_MISS_LOGGED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn row_str(r: &Row, i: usize) -> String {
    match r.as_ref(i) {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(v) => value_string(v.clone()).unwrap_or_default(),
        None => String::new(),
    }
}

/// 把连接的 client / connection / results 字符集全部设为 UTF-8，并留痕服务端真实取值。
/// `SET NAMES utf8mb4` 在极老的 MySQL（<5.5.3）上会失败，此时回退 `SET NAMES utf8`。
async fn ensure_utf8(conn: &mut mysql_async::Conn, conn_id: &str) {
    // 先记录服务端原本的字符集设置，便于定位「是谁把中文转成了 ?」
    let first = charset_logged()
        .lock()
        .map(|mut s| s.insert(conn_id.to_string()))
        .unwrap_or(true);
    if first {
        if let Ok(rows) = conn
            .query::<Row, _>(
                "SELECT @@version, @@character_set_client, @@character_set_connection, \
                 @@character_set_results, @@character_set_server",
            )
            .await
        {
            if let Some(r) = rows.first() {
                trace_conn(&format!(
                    "CHARSET conn={} version={} client={} connection={} results={} server={}",
                    conn_id,
                    row_str(r, 0),
                    row_str(r, 1),
                    row_str(r, 2),
                    row_str(r, 3),
                    row_str(r, 4),
                ));
            }
        }
    }

    if conn.query_drop("SET NAMES utf8mb4").await.is_ok() {
        return;
    }
    // 极老服务端不支持 utf8mb4 → 回退 utf8(utf8mb3)
    if conn.query_drop("SET NAMES utf8").await.is_ok() && first {
        trace_conn(&format!("CHARSET conn={} 回退为 utf8(utf8mb3)", conn_id));
    }
}

/// cp1252 / latin1 字符反查字节：二次编码的乱码里常混有 cp1252 的 0x80-0x9F 映射
/// （如 U+20AC ← 0x80、U+0160 ← 0x8A）与 C1 控制符，必须精确还原才能解回原文。
fn cp1252_to_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u <= 0xFF {
        return Some(u as u8);
    }
    const T: [(u32, u8); 27] = [
        (0x20AC, 0x80), (0x201A, 0x82), (0x0192, 0x83), (0x201E, 0x84), (0x2026, 0x85),
        (0x2020, 0x86), (0x2021, 0x87), (0x02C6, 0x88), (0x2030, 0x89), (0x0160, 0x8A),
        (0x2039, 0x8B), (0x0152, 0x8C), (0x017D, 0x8E), (0x2018, 0x91), (0x2019, 0x92),
        (0x201C, 0x93), (0x201D, 0x94), (0x2022, 0x95), (0x2013, 0x96), (0x2014, 0x97),
        (0x02DC, 0x98), (0x2122, 0x99), (0x0161, 0x9A), (0x203A, 0x9B), (0x0153, 0x9C),
        (0x017E, 0x9E), (0x0178, 0x9F),
    ];
    T.iter().find(|(k, _)| *k == u).map(|(_, b)| *b)
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 二次编码还原：UTF-8 文本被当成 latin1/cp1252 读取后又以 UTF-8 存库，
/// 得到「å¤§æ¿ä»·æ ¼」这类字符串。按 cp1252/latin1 反查回字节、再按 UTF-8 解一次；
/// 解出中文即认为修复成功（最多两轮，兼容三次编码）。
fn repair_double_encoded(s: &str) -> Option<String> {
    fn once(s: &str) -> Option<String> {
        if s.is_empty() || has_cjk(s) || s.chars().all(|c| c.is_ascii()) {
            return None;
        }
        let mut bytes = Vec::with_capacity(s.len());
        for c in s.chars() {
            bytes.push(cp1252_to_byte(c)?);
        }
        if bytes.iter().all(|b| b.is_ascii()) {
            return None;
        }
        match std::str::from_utf8(&bytes) {
            Ok(t) if has_cjk(t) => Some(t.to_string()),
            _ => None,
        }
    }
    once(s).or_else(|| once(s).and_then(|x| once(&x)))
}

/// GBK 解码反查表：字符 → 原始字节。
///
/// 不能直接用 `encoding_rs::GBK.encode()` 反推——解码器会把单字节 0x80 解成 '€'，
/// 但编码器不认 '€'，导致整段还原失败。这里反过来遍历所有合法字节组合建表，
/// 保证「解码器能产出的字符」都能精确编回原字节。
fn gbk_rev() -> &'static std::collections::HashMap<char, [u8; 2]> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<char, [u8; 2]>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        fn put(map: &mut std::collections::HashMap<char, [u8; 2]>, bytes: &[u8]) {
            let (s, _, err) = encoding_rs::GBK.decode(bytes);
            let mut it = s.chars();
            let (Some(c), None) = (it.next(), it.next()) else {
                return;
            };
            if err || c == '\u{FFFD}' || c.is_ascii() {
                return;
            }
            map.entry(c).or_insert([bytes[0], *bytes.get(1).unwrap_or(&0)]);
        }
        let mut m = std::collections::HashMap::with_capacity(24_000);
        for b in 0x80u8..=0xFF {
            put(&mut m, &[b]);
        }
        for lead in 0x81u8..=0xFE {
            for trail in 0x40u8..=0xFE {
                if trail == 0x7F {
                    continue;
                }
                put(&mut m, &[lead, trail]);
            }
        }
        m
    })
}

/// 单个「非 ASCII 连续段」的 GBK 乱码还原。
///
/// 成因：UTF-8 字节被当成 GBK 解码后又以 UTF-8 存库（如 `品牌编号` → `鍝佺墝缂栧彿`）。
/// 还原：整段按 GBK 反查回字节，再按 UTF-8 解一次。
/// 正常中文的 GBK 字节几乎不可能构成合法 UTF-8，因此逐段试探不会误伤原生中文
/// （实测某库 4300+ 条正常注释零误判）。
fn fix_gbk_run(run: &str) -> Option<String> {
    let rev = gbk_rev();
    let mut bytes = Vec::with_capacity(run.len() * 2);
    for c in run.chars() {
        // '?' / U+FFFD：历史数据在这一步已经丢了字节，先占位，后面统一处理
        if c == '?' || c == '\u{FFFD}' {
            bytes.push(b'?');
            continue;
        }
        if c.is_ascii() {
            bytes.push(c as u8);
            continue;
        }
        match rev.get(&c) {
            Some([a, 0]) => bytes.push(*a),
            Some([a, b]) => {
                bytes.push(*a);
                bytes.push(*b);
            }
            None => return None, // 非 GBK 可表示字符（emoji / 生僻符号）→ 不动
        }
    }
    // 严格还原：整段字节刚好是合法 UTF-8 且含汉字
    if let Ok(t) = std::str::from_utf8(&bytes) {
        return if t != run && has_cjk(t) { Some(t.to_string()) } else { None };
    }
    // 有损还原：个别字节在写库时已被换成 '?'（或被直接砍掉），这段字节已不可能完整解出。
    // 仅当它看起来确实是「被截断的 UTF-8 中文」时才动手：首字节必须是汉字三字节序列的起始（E4-E9）。
    let b0 = *bytes.first()?;
    if !(0xE4..=0xE9).contains(&b0) {
        return None;
    }
    let lossy = String::from_utf8_lossy(&bytes);
    let total = lossy.chars().count();
    let marks = lossy
        .chars()
        .filter(|c| *c == '?' || *c == '\u{FFFD}')
        .count();
    // 至少留下一个能认出来的字，且「洞」不超过四分之三，否则多半不是这类乱码
    if total < 2 || total - marks < 1 || marks * 4 > total * 3 {
        return None;
    }
    // 替换符一律折成 '?'（表示这里丢了一个字节），并把连续的 '?' 并成一个：
    // `用户�?��删除` → `用户?删除`
    let mut out = String::with_capacity(total);
    for c in lossy.chars() {
        if c == '\u{FFFD}' || c == '?' {
            if !out.ends_with('?') {
                out.push('?');
            }
            continue;
        }
        out.push(c);
    }
    if out != run && has_cjk(&out) {
        Some(out)
    } else {
        None
    }
}

/// 逐段还原 GBK 乱码（只处理非 ASCII 连续段，ASCII 部分原样保留）
fn repair_gbk_mojibake(s: &str) -> Option<String> {
    if s.is_empty() || !has_cjk(s) {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut run = String::new();
    for c in s.chars() {
        if (c as u32) > 0x7F {
            run.push(c);
            continue;
        }
        // '?' 紧跟在汉字后面时，它多半就是「丢掉的字节」的占位，并入本段一起还原
        if c == '?' && !run.is_empty() {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            match fix_gbk_run(&run) {
                Some(fixed) => {
                    changed = true;
                    out.push_str(&fixed);
                }
                None => out.push_str(&run),
            }
            run.clear();
        }
        out.push(c);
    }
    if !run.is_empty() {
        match fix_gbk_run(&run) {
            Some(fixed) => {
                changed = true;
                out.push_str(&fixed);
            }
            None => out.push_str(&run),
        }
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

/// 字节转字符串：严格 UTF-8 优先；非法时按中文环境常见编码兜底（GBK → BIG5 → latin1），
/// 并留痕一次，便于确认问号是「服务端转码丢失」还是「历史数据非 UTF-8 编码」。
fn decode_bytes(b: &[u8], where_: &str, is_text: bool) -> String {
    // 二进制列（BLOB / BINARY / geometry）不做字符集推断，否则会把二进制当 GBK 解出乱码
    if !is_text {
        return String::from_utf8_lossy(b).into_owned();
    }
    match std::str::from_utf8(b) {
        Ok(s) => {
            // 已经是合法 UTF-8，但如果整列都是 '?'，说明数据在「写入时」就被服务端转成了问号，
            // 客户端无法还原（需要重新导入数据）。这里留痕一次，帮助区分是读的问题还是存的问题。
            if b.len() >= 2 && b.len() <= 64 && b.iter().all(|x| *x == b'?') {
                let key = format!("QMARK:{where_}");
                let fresh = miss_logged()
                    .lock()
                    .map(|mut set| set.len() < 40 && set.insert(key))
                    .unwrap_or(false);
                if fresh {
                    trace_conn(&format!(
                        "CHARSET_STORED_QMARK 列 {} 的值全部为 '?'（{} 个）——数据在写入时已被转码丢失，需重新导入",
                        where_,
                        b.len()
                    ));
                }
            }
            // 合法 UTF-8 但形如「å¤§æ¿ä»·æ ¼」的二次编码 → 自动还原
            if let Some(fixed) = repair_double_encoded(s) {
                let key = format!("REPAIR:{where_}");
                let fresh = miss_logged()
                    .lock()
                    .map(|mut set| set.len() < 40 && set.insert(key))
                    .unwrap_or(false);
                if fresh {
                    trace_conn(&format!(
                        "CHARSET_REPAIRED 列 {} 二次编码已还原：{} → {}",
                        where_,
                        s.chars().take(16).collect::<String>(),
                        fixed.chars().take(16).collect::<String>()
                    ));
                }
                return fixed;
            }
            // 合法 UTF-8 但形如「鍝佺墝缂栧彿」的 GBK 乱码 → 逐段还原
            if let Some(fixed) = repair_gbk_mojibake(s) {
                let key = format!("GBKFIX:{where_}");
                let fresh = miss_logged()
                    .lock()
                    .map(|mut set| set.len() < 40 && set.insert(key))
                    .unwrap_or(false);
                if fresh {
                    trace_conn(&format!(
                        "CHARSET_REPAIRED 列 {} GBK 乱码已还原：{} → {}",
                        where_,
                        s.chars().take(16).collect::<String>(),
                        fixed.chars().take(16).collect::<String>()
                    ));
                }
                return fixed;
            }
            s.to_owned()
        }
        Err(_) => {
            let (g, _, _) = encoding_rs::GBK.decode(b);
            let s = if !g.contains('\u{FFFD}') {
                g.into_owned()
            } else {
                let (t, _, _) = encoding_rs::BIG5.decode(b);
                if !t.contains('\u{FFFD}') {
                    t.into_owned()
                } else {
                    encoding_rs::WINDOWS_1252.decode(b).0.into_owned()
                }
            };
            let key = format!("{where_}");
            let fresh = miss_logged()
                .lock()
                .map(|mut set| {
                    if set.len() >= 40 {
                        false
                    } else {
                        set.insert(key)
                    }
                })
                .unwrap_or(false);
            if fresh {
                let head: String = b
                    .iter()
                    .take(12)
                    .map(|x| format!("{x:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                trace_conn(&format!(
                    "CHARSET_NONUTF8 {} bytes=[{}] → 已按 GBK/BIG5/latin1 还原：{}",
                    where_,
                    head,
                    s.chars().take(20).collect::<String>()
                ));
            }
            s
        }
    }
}

/// 诊断用：按 App 的真实规则解码一段原始字节（examples/probe_ddl 复用，保证验证与线上一致）
#[doc(hidden)]
pub fn decode_diag(b: &[u8]) -> String {
    decode_bytes(b, "diag", true)
}

/// 读取行内第 i 列的原始值（越界 / NULL 归一为 Value::NULL）
fn val(r: &Row, i: usize) -> Value {
    r.as_ref(i).cloned().unwrap_or(Value::NULL)
}

fn value_string(v: Value) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(decode_bytes(&b, "-", true)),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Date(y, m, d, h, mi, s, us) => Some(if us > 0 {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
        } else {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }),
        Value::Time(neg, d, h, mi, s, us) => Some(if us > 0 {
            format!("{}{d}d {h:02}:{mi:02}:{s:02}.{us:06}", if neg { "-" } else { "" })
        } else {
            format!("{}{d}d {h:02}:{mi:02}:{s:02}", if neg { "-" } else { "" })
        }),
    }
}

fn value_u64(v: Value) -> u64 {
    match v {
        Value::Int(i) => i.max(0) as u64,
        Value::UInt(u) => u,
        Value::Bytes(b) => String::from_utf8_lossy(&b)
            .parse()
            .unwrap_or(0),
        _ => 0,
    }
}

/// 列类型 -> 展示名（表头副标题）
fn type_label(c: &Column) -> &'static str {
    match c.column_type() {
        ColumnType::MYSQL_TYPE_TINY => "tinyint",
        ColumnType::MYSQL_TYPE_SHORT => "smallint",
        ColumnType::MYSQL_TYPE_LONG => "int",
        ColumnType::MYSQL_TYPE_LONGLONG => "bigint",
        ColumnType::MYSQL_TYPE_INT24 => "mediumint",
        ColumnType::MYSQL_TYPE_FLOAT => "float",
        ColumnType::MYSQL_TYPE_DOUBLE => "double",
        ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => "decimal",
        ColumnType::MYSQL_TYPE_DATE => "date",
        ColumnType::MYSQL_TYPE_TIME => "time",
        ColumnType::MYSQL_TYPE_DATETIME => "datetime",
        ColumnType::MYSQL_TYPE_TIMESTAMP => "timestamp",
        ColumnType::MYSQL_TYPE_YEAR => "year",
        ColumnType::MYSQL_TYPE_VARCHAR | ColumnType::MYSQL_TYPE_VAR_STRING => "varchar",
        ColumnType::MYSQL_TYPE_STRING => "char",
        ColumnType::MYSQL_TYPE_BIT => "bit",
        ColumnType::MYSQL_TYPE_JSON => "json",
        ColumnType::MYSQL_TYPE_ENUM => "enum",
        ColumnType::MYSQL_TYPE_SET => "set",
        ColumnType::MYSQL_TYPE_TINY_BLOB => "tinyblob",
        ColumnType::MYSQL_TYPE_MEDIUM_BLOB => "mediumblob",
        ColumnType::MYSQL_TYPE_LONG_BLOB => "longblob",
        ColumnType::MYSQL_TYPE_BLOB => "blob/text",
        ColumnType::MYSQL_TYPE_GEOMETRY => "geometry",
        _ => "",
    }
}

/// 行 -> 单元格矩阵。结果集完整保留，不做任何行数截断。
fn convert_rows(rows: Vec<Row>, ms: u64) -> QueryResult {
    let mut columns = Vec::new();
    let mut data: Vec<Vec<Option<String>>> = Vec::with_capacity(rows.len());
    if let Some(first) = rows.first() {
        columns = first
            .columns_ref()
            .iter()
            .map(|c| QueryColumn {
                name: c.name_str().to_string(),
                r#type: type_label(c).to_string(),
            })
            .collect();
        // 字符集号 63 = binary：该列内容不是文本，不做编码推断
        let is_text: Vec<bool> = first
            .columns_ref()
            .iter()
            .map(|c| c.character_set() != 63)
            .collect();
        for r in &rows {
            let line: Vec<Option<String>> = (0..columns.len())
                .map(|i| {
                    // 带列名解码：非 UTF-8 时日志能定位到具体列
                    match val(r, i) {
                        Value::NULL => None,
                        Value::Bytes(b) => {
                            Some(decode_bytes(&b, &columns[i].name, is_text[i]))
                        }
                        v => value_string(v),
                    }
                })
                .collect();
            data.push(line);
        }
    }
    QueryResult {
        columns,
        rows: data,
        ms,
        total: None,
        page: 1,
        page_size: 0,
    }
}

/* ---------- 测试：字符集兜底解码 ---------- */
#[cfg(test)]
mod charset_tests {
    use super::decode_bytes;

    #[test]
    fn utf8_passthrough() {
        assert_eq!(decode_bytes("张三".as_bytes(), "c", true), "张三");
    }

    #[test]
    fn gbk_fallback() {
        // "中文" 的 GBK 字节：D6 D0 CE C4（非法 UTF-8）
        let gbk = &[0xD6u8, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode_bytes(gbk, "c", true), "中文");
    }

    #[test]
    fn gbk_long_text() {
        // "测试数据表" GBK: B2 E2 CA D4 CA FD BE DD B1 ED
        let gbk = &[0xB2u8, 0xE2, 0xCA, 0xD4, 0xCA, 0xFD, 0xBE, 0xDD, 0xB1, 0xED];
        assert_eq!(decode_bytes(gbk, "c", true), "测试数据表");
    }

    #[test]
    fn ascii_and_empty() {
        assert_eq!(decode_bytes(b"", "c", true), "");
        assert_eq!(decode_bytes(b"abc123", "c", true), "abc123");
    }

    #[test]
    fn latin1_accent() {
        // 0xE9 = é（latin1），非法 UTF-8 起始-延续组合
        assert_eq!(decode_bytes(&[0xE9u8], "c", true), "é");
    }
}

#[cfg(test)]
mod repair_tests {
    use super::*;

    /// 构造真正的二次编码：把 UTF-8 字节逐个当作 latin1 字符
    fn double_encode(original: &str) -> String {
        original.as_bytes().iter().map(|b| *b as char).collect()
    }

    /// 构造 cp1252 变体：0x80-0x9F 段映射成欧洲标点（真实库里最常见）
    fn double_encode_cp1252(original: &str) -> String {
        fn b2c(b: u8) -> char {
            const T: [(u8, char); 27] = [
                (0x80, '\u{20AC}'), (0x82, '\u{201A}'), (0x83, '\u{0192}'), (0x84, '\u{201E}'),
                (0x85, '\u{2026}'), (0x86, '\u{2020}'), (0x87, '\u{2021}'), (0x88, '\u{02C6}'),
                (0x89, '\u{2030}'), (0x8A, '\u{0160}'), (0x8B, '\u{2039}'), (0x8C, '\u{0152}'),
                (0x8E, '\u{017D}'), (0x91, '\u{2018}'), (0x92, '\u{2019}'), (0x93, '\u{201C}'),
                (0x94, '\u{201D}'), (0x95, '\u{2022}'), (0x96, '\u{2013}'), (0x97, '\u{2014}'),
                (0x98, '\u{02DC}'), (0x99, '\u{2122}'), (0x9A, '\u{0161}'), (0x9B, '\u{203A}'),
                (0x9C, '\u{0153}'), (0x9E, '\u{017E}'), (0x9F, '\u{0178}'),
            ];
            T.iter().find(|(k, _)| *k == b).map(|(_, c)| *c).unwrap_or(b as char)
        }
        original.as_bytes().iter().map(|b| b2c(*b)).collect()
    }

    #[test]
    fn repairs_double_encoded() {
        for origin in ["商品价格", "测试数据表", "订单状态：已支付"] {
            let bad = double_encode(origin);
            assert_eq!(repair_double_encoded(&bad).as_deref(), Some(origin), "bad={bad}");
        }
    }

    /// 真实库中的形态：UTF-8 字节经 cp1252 读入后再存 UTF-8
    #[test]
    fn repairs_cp1252_mojibake() {
        for origin in [
            "库存价格自动抓取",
            "会话token保活",
            "订单支付状态同步（含标点）",
        ] {
            let bad = double_encode_cp1252(origin);
            assert_eq!(repair_double_encoded(&bad).as_deref(), Some(origin), "bad={bad}");
        }
    }

    #[test]
    fn keeps_normal_text() {
        assert_eq!(repair_double_encoded("商品价格"), None);
        assert_eq!(repair_double_encoded("hello"), None);
        assert_eq!(repair_double_encoded(""), None);
        // 正常欧洲文本不应被改写
        assert_eq!(repair_double_encoded("café"), None);
        // CJK 与非拉丁混排（日文假名等）不动
        assert_eq!(repair_double_encoded("テスト"), None);
    }

    #[test]
    fn decode_prefers_repair() {
        let bad = double_encode("商品价格");
        assert_eq!(decode_bytes(bad.as_bytes(), "c", true), "商品价格");
        assert_eq!(decode_bytes("正常中文".as_bytes(), "c", true), "正常中文");
        assert_eq!(decode_bytes(b"plain", "c", true), "plain");
    }

    /// 构造 GBK 乱码：UTF-8 字节被当成 GBK 读出（真实库里 DDL 注释的典型形态）
    fn gbk_mojibake(original: &str) -> String {
        let (s, _, _) = encoding_rs::GBK.decode(original.as_bytes());
        s.into_owned()
    }

    #[test]
    fn repairs_gbk_mojibake() {
        for origin in ["品牌编号", "创建时间", "是否删除", "租户编号", "机构ID"] {
            let bad = gbk_mojibake(origin);
            assert_ne!(bad, origin);
            assert_eq!(repair_gbk_mojibake(&bad).as_deref(), Some(origin), "bad={bad}");
        }
    }

    /// 库里的字节在被 GBK 解码时就丢了（表现为 '?' 或 U+FFFD）：
    /// 能还原的部分必须还原，剩下一个字允许显示为 '?'，但绝不能继续显示乱码。
    #[test]
    fn gbk_repair_with_lost_byte() {
        let bad = gbk_mojibake("订单状态");
        let fixed = repair_gbk_mojibake(&bad).expect("应尽可能还原");
        assert!(fixed.starts_with("订单状"), "bad={bad} fixed={fixed}");
        assert!(!fixed.contains('鐘') && !fixed.contains('舵'), "fixed={fixed}");
    }

    /// 原生中文 / 英文 / 二进制都不能被这条规则改写
    #[test]
    fn gbk_repair_keeps_normal_text() {
        for s in ["品牌编号", "是否删除", "hello world", "", "café", "テスト"] {
            assert_eq!(repair_gbk_mojibake(s), None, "s={s}");
        }
    }

    /// 整条 DDL 里 ASCII 部分必须原样保留，只还原注释里的乱码
    #[test]
    fn repairs_ddl_comment() {
        let bad = format!(
            "CREATE TABLE `t` (\n  `id` int(11) NOT NULL COMMENT '{}',\n  `ctime` datetime COMMENT '{}'\n) ENGINE=InnoDB COMMENT='{}'",
            gbk_mojibake("编号"),
            gbk_mojibake("创建时间"),
            gbk_mojibake("品牌信息")
        );
        let fixed = repair_gbk_mojibake(&bad).expect("应能还原");
        assert!(fixed.contains("COMMENT '编号'"), "{fixed}");
        assert!(fixed.contains("COMMENT '创建时间'"), "{fixed}");
        assert!(fixed.contains("COMMENT='品牌信息'"), "{fixed}");
        assert!(fixed.contains("ENGINE=InnoDB"), "{fixed}");
    }

    /// 历史数据里个别字节在写库时已被换成 '?'：只保留可见损失，不再输出乱码
    #[test]
    fn gbk_repair_partial_loss() {
        // 「状态」丢了「态」的第二个字节 → 库里读出来就是 `鐘舵?`
        let fixed = decode_bytes("鐘舵?".as_bytes(), "c", true);
        assert!(fixed.starts_with('状'), "fixed={fixed}");
        assert!(!fixed.contains('鐘'), "fixed={fixed}");
    }
}

/* ---------- 测试：取消登记与只读前置校验 ---------- */
#[cfg(test)]
mod cancel_tests {
    use super::*;

    fn temp_db() -> DbManager {
        let dir = std::env::temp_dir().join(format!(
            "dm-cancel-test-{}",
            chrono::Utc::now().timestamp_millis()
        ));
        DbManager::new(Arc::new(Store::new(dir).unwrap()))
    }

    /// 取消的基础不变量：命中即真的发出信号；未登记 / 已注销一律 miss，
    /// 保证「取消」既不会失效，也不会误伤别的任务。
    #[test]
    fn cancel_hits_registered_run_only() {
        let db = temp_db();
        let token = db.begin_run("run-1");
        assert!(!token.is_cancelled());

        assert!(db.cancel("run-1"), "已登记的任务必须能被取消");
        assert!(token.is_cancelled(), "cancel 必须真的触发令牌");
        assert!(!db.cancel("run-2"), "未登记的 run_id 不应返回命中");

        db.end_run("run-1");
        assert!(!db.cancel("run-1"), "任务结束后再取消应返回 miss");
    }

    /// 超时路径依赖「先取令牌、再取消」的顺序：登记项被提前摘除会导致取消落空。
    /// 这里锁住该行为——令牌仍然有效（未 end_run 时）。
    #[test]
    fn cancel_still_works_after_lookup() {
        let db = temp_db();
        let token = db.begin_run("run-3");
        let registered = db.running.remove("run-3");
        if let Some(t) = registered {
            t.cancel();
        }
        assert!(token.is_cancelled());
    }

    /// A3：EXPLAIN 前必须先过只读校验。这里连接根本不存在，
    /// 若先建连接会报 NO_CONN；实际返回 1142 证明校验发生在最前面。
    #[tokio::test]
    async fn explain_rejects_non_select_before_connecting() {
        let db = temp_db();
        let err = db
            .explain("no-such-conn", "shop", "DELETE FROM orders")
            .await
            .unwrap_err();
        assert_eq!(err.code, "1142", "应当在只读校验处被拦截：{err}");

        // 最危险的变体：拼成 `EXPLAIN ANALYZE ...` 后 MySQL 会真正执行语句。
        // 无论被解析层拒还是被语义层拒，都必须在「触碰数据库」之前发生。
        let err = db
            .explain("no-such-conn", "shop", "ANALYZE DELETE FROM orders")
            .await
            .unwrap_err();
        assert_ne!(err.code, "NO_CONN", "不得进入建连接阶段：{err}");
    }

    /// 执行入口同样在建立连接之前拦截写操作
    #[tokio::test]
    async fn execute_rejects_write_before_connecting() {
        let db = temp_db();
        let err = db
            .execute("no-such-conn", "shop", "DELETE FROM orders", 5, "run-x")
            .await
            .unwrap_err();
        assert_eq!(err.code, "1142", "应当在只读校验处被拦截：{err}");
    }
}
