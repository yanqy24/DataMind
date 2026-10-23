//! commands.rs — Tauri 命令层：前端 invoke 入口（薄封装，业务在 db / llm / store）

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::AppError;
use crate::llm;
use crate::models::*;
use crate::mysql::DbManager;
use crate::store::Store;

/// SQL 生成整条流水线的总预算：模型单次调用 120s × 最多 6 次，超时即中止，
/// 避免用户守着「推理中…」等十分钟。
const GENERATE_BUDGET: Duration = Duration::from_secs(300);

/// 全局共享状态。
///
/// `db` 是 `Arc` 而不是 `Mutex`：连接池内部自带短临界区，长查询 / 结构同步
/// 不再互相串行，取消命令也无需排队等锁（详见 [crate::mysql::DbManager]）。
pub struct AppState {
    pub store: std::sync::Arc<Store>,
    pub db: std::sync::Arc<DbManager>,
}

/* ---------- 连接 ---------- */

#[tauri::command]
pub async fn conn_list(state: State<'_, AppState>) -> Result<Vec<ConnConfig>, AppError> {
    state.store.load_connections()
}

#[tauri::command]
pub async fn conn_save(
    conn: ConnConfig,
    state: State<'_, AppState>,
) -> Result<ConnConfig, AppError> {
    let mut list = state.store.load_connections()?;
    let mut saved = conn;

    if saved.id.is_empty() {
        saved.id = uuid::Uuid::new_v4().to_string();
        list.push(saved.clone());
    } else {
        match list.iter_mut().find(|c| c.id == saved.id) {
            Some(c) => *c = saved.clone(),
            None => list.push(saved.clone()),
        }
    }
    state.store.save_connections(&list)?;
    // 配置变化后重建连接（销毁旧池 / 旧隧道）
    state.db.reset(&saved.id);
    Ok(saved)
}

#[tauri::command]
pub async fn conn_delete(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    let mut list = state.store.load_connections()?;
    list.retain(|c| c.id != id);
    state.store.save_connections(&list)?;
    state.db.reset(&id);
    state.store.delete_memory(&id)?;
    Ok(())
}

#[tauri::command]
pub async fn conn_test(conn: ConnConfig, state: State<'_, AppState>) -> Result<TestResult, AppError> {
    let ms = state.db.test(&conn).await?;
    Ok(TestResult { ok: true, ms: ms as u64, message: String::new() })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub ms: u64,
    pub message: String,
}

/// 连接 + 全量重建本地记忆（DDL / 关系 / 取样），进度经 `sync-progress` 事件推送
#[tauri::command]
pub async fn conn_sync(
    id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ConnMemory, AppError> {
    // 进度事件闭包：发射到主窗口（克隆 id 避免移动）
    let id_for_event = id.clone();
    let progress = move |step: &str| {
        let _ = app.emit(
            "sync-progress",
            serde_json::json!({ "connId": id_for_event, "step": step }),
        );
    };
    state.db.sync_memory(&id, &progress).await
}

#[tauri::command]
pub async fn memory_get(id: String, state: State<'_, AppState>) -> Result<Option<ConnMemory>, AppError> {
    state.store.load_memory(&id)
}

/* ---------- 单表详情 ---------- */

/// 实时读取单表详情（建表语句 + 取样），不读本地记忆，避免显示修复前采集的旧文本
#[tauri::command]
pub async fn table_detail(
    conn_id: String,
    db: String,
    table: String,
    state: State<'_, AppState>,
) -> Result<TableDetail, AppError> {
    state.db.table_detail(&conn_id, &db, &table).await
}

/* ---------- SQL 执行 ---------- */

#[tauri::command]
pub async fn sql_execute(
    run_id: String,
    conn_id: String,
    db: String,
    sql: String,
    timeout_secs: u64,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<QueryResult, AppError> {
    let result = state.db.execute(&conn_id, &db, &sql, timeout_secs, &run_id).await;

    // 记录执行日志（成功 / 失败 / 取消）
    let conn_name = state
        .store
        .load_connections()?
        .iter()
        .find(|c| c.id == conn_id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "—".into());
    let (status, ms) = match &result {
        Ok(r) => ("success", Some(r.ms)),
        Err(e) if e.code == "TIMEOUT" => ("cancelled", None),
        Err(e) if e.code == "CANCELLED" => ("cancelled", None),
        Err(_) => ("error", None),
    };
    let _ = state.store.append_log(SqlLogItem {
        id: uuid::Uuid::new_v4().to_string(),
        ts: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        sql: sql.trim().to_string(),
        status: status.into(),
        conn: conn_name,
        db,
        ms,
    });
    let _ = app; // 预留事件推送
    result
}

#[tauri::command]
pub async fn sql_cancel(run_id: String, state: State<'_, AppState>) -> Result<bool, AppError> {
    Ok(state.db.cancel(&run_id))
}

/* ---------- SQL 生成（模型推理） ---------- */

#[tauri::command]
pub async fn sql_generate(
    run_id: Option<String>,
    conn_id: String,
    db: String,
    table: String,
    prompt: String,
    mode: String,
    prev_sql: Option<String>,
    error: Option<String>,
    agent: Option<bool>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<GenerateAnswer, AppError> {
    // 1. 启用中的模型
    let models = state.store.load_models()?;
    let model = models
        .iter()
        .find(|m| m.active)
        .ok_or_else(|| AppError::new("NO_MODEL", "请先在右侧「模型配置」中添加并启用一个模型"))?;

    // 2. 结构记忆作为上下文
    let memory = state.store.load_memory(&conn_id)?;
    if memory.is_none() && mode != "fix" {
        return Err(AppError::new(
            "NO_MEMORY",
            "该连接尚未形成本地记忆，请先在连接树点击「刷新结构记忆」",
        ));
    }

    // 3. 推理：两阶段 Agent 模式（分析 → 探查 → 设计 → 生成 → 校验）
    //
    // 整条链路可被 sql_cancel 的 run_id 中断，并受 GENERATE_BUDGET 总预算约束；
    // 生成期间**不再独占数据库锁**，用户仍可正常查询 / 取消（见 AppState::db）。
    let run_id = run_id.unwrap_or_else(|| format!("gen:{}", uuid::Uuid::new_v4()));
    let token = state.db.begin_run(&run_id);
    // 生成期间只持 Arc 克隆，不占用任何全局锁
    let db_mgr = std::sync::Arc::clone(&state.db);
    let model_cfg = model.clone();

    let pipeline = async {
        let mut steps: Vec<AgentStepView> = Vec::new();
        let (sql, note) = if agent.unwrap_or(true) && mode != "fix" {
            let stage_labels: Vec<(String, String)> = vec![
                ("schema".into(), "分析表结构".into()),
                ("explore".into(), "探查真实数据".into()),
                ("design".into(), "设计查询方案".into()),
                ("build".into(), "生成 SQL".into()),
                ("verify".into(), "校验 SQL".into()),
            ];
            let emit = |key: &str, _label: &str| {
                let label = stage_labels
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_else(|| key.to_string());
                let _ = app.emit(
                    "sqlgen-stage",
                    serde_json::json!({ "stage": key, "label": label }),
                );
            };
            let ctx = crate::agent::AgentCtx {
                prompt: &prompt,
                db: &db,
                table: &table,
                conn_id: &conn_id,
                memory: memory.as_ref(),
            };
            match crate::agent::run(&model_cfg, &db_mgr, ctx, &emit, &token).await {
                Ok(ans) => {
                    steps = ans
                        .steps
                        .iter()
                        .map(|s| AgentStepView {
                            key: s.key.clone(),
                            label: s.label.clone(),
                            summary: s.summary.clone(),
                            ok: s.ok,
                        })
                        .collect();
                    (ans.sql, ans.note)
                }
                // 用户主动取消 / 总预算超时：不要再降级重试，直接把中止结果返回
                Err(e) if e.code == "CANCELLED" || e.code == "TIMEOUT" => return Err(e),
                Err(_e) => {
                    // Agent 链路失败 → 退化为单次直出，保证可用性
                    let _ = app.emit(
                        "sqlgen-stage",
                        serde_json::json!({ "stage": "fallback", "label": "降级为直出生成" }),
                    );
                    let req = llm::GenerateReq {
                        prompt: &prompt,
                        mode: &mode,
                        prev_sql: prev_sql.as_deref().unwrap_or(""),
                        error: error.as_deref().unwrap_or(""),
                        memory: memory.as_ref(),
                        db: &db,
                        table: &table,
                    };
                    llm::generate_sql(&model_cfg, req, &token).await?
                }
            }
        } else {
            let req = llm::GenerateReq {
                prompt: &prompt,
                mode: &mode,
                prev_sql: prev_sql.as_deref().unwrap_or(""),
                error: error.as_deref().unwrap_or(""),
                memory: memory.as_ref(),
                db: &db,
                table: &table,
            };
            llm::generate_sql(&model_cfg, req, &token).await?
        };
        Ok::<(String, String, Vec<AgentStepView>), AppError>((sql, note, steps))
    };

    let generated = tokio::time::timeout(GENERATE_BUDGET, pipeline).await;
    // 先注销登记，再处理结果：取消/超时路径不会留下悬挂的 run_id
    state.db.end_run(&run_id);
    let (mut sql, note, steps) = match generated {
        Ok(r) => r?,
        Err(_) => {
            // 总预算耗尽：让仍在进行的模型请求 / 探查查询一起停下
            token.cancel();
            return Err(AppError::new(
                "TIMEOUT",
                format!(
                    "生成耗时超过 {}s 已中止，请精简需求后重试",
                    GENERATE_BUDGET.as_secs()
                ),
            ));
        }
    };

    // 4. 只读保护：模型输出也必须通过 SELECT 校验
    if crate::guard::ensure_select_only(&sql).is_err() {
        // 尝试从输出中提取 SELECT 片段
        if let Some(fixed) = extract_select(&sql) {
            sql = fixed;
        } else {
            return Err(AppError::new(
                "LLM_NOT_SELECT",
                "模型生成的语句不是只读 SELECT，已被拦截，请调整需求描述后重试",
            ));
        }
        crate::guard::ensure_select_only(&sql)?;
    }

    // 5. 记录生成日志
    let conn_name = state
        .store
        .load_connections()?
        .iter()
        .find(|c| c.id == conn_id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "—".into());
    let _ = state.store.append_log(SqlLogItem {
        id: uuid::Uuid::new_v4().to_string(),
        ts: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        sql: sql.clone(),
        status: "idle".into(),
        conn: conn_name,
        db,
        ms: None,
    });

    Ok(GenerateAnswer { sql, note, steps })
}

#[derive(serde::Serialize)]
pub struct GenerateAnswer {
    pub sql: String,
    pub note: String,
    /// 两阶段流水线的过程留痕（前端展示"推理过程"）
    pub steps: Vec<AgentStepView>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStepView {
    pub key: String,
    pub label: String,
    pub summary: String,
    pub ok: bool,
}

/// 从模型输出中提取第一条 SELECT 语句（容错处理）
fn extract_select(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("select")?;
    let rest = &text[start..];
    let end = rest.rfind(';').map(|e| &rest[..e]).unwrap_or(rest);
    Some(end.trim().to_string())
}

/* ---------- 模型 ---------- */

#[tauri::command]
pub async fn model_list(state: State<'_, AppState>) -> Result<Vec<ModelConfig>, AppError> {
    state.store.load_models()
}

#[tauri::command]
pub async fn model_save(model: ModelConfig, state: State<'_, AppState>) -> Result<ModelConfig, AppError> {
    let mut list = state.store.load_models()?;
    let mut saved = model;
    if saved.id.is_empty() {
        saved.id = uuid::Uuid::new_v4().to_string();
        list.push(saved.clone());
    } else {
        match list.iter_mut().find(|m| m.id == saved.id) {
            Some(m) => *m = saved.clone(),
            None => list.push(saved.clone()),
        }
    }
    state.store.save_models(&list)?;
    Ok(saved)
}

#[tauri::command]
pub async fn model_delete(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    let mut list = state.store.load_models()?;
    list.retain(|m| m.id != id);
    state.store.save_models(&list)?;
    Ok(())
}

#[tauri::command]
pub async fn model_activate(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    let mut list = state.store.load_models()?;
    for m in list.iter_mut() {
        m.active = m.id == id;
    }
    state.store.save_models(&list)?;
    Ok(())
}

/* ---------- SQL 日志 ---------- */

#[tauri::command]
pub async fn log_list(state: State<'_, AppState>) -> Result<Vec<SqlLogItem>, AppError> {
    state.store.load_log()
}

#[tauri::command]
pub async fn log_clear(state: State<'_, AppState>) -> Result<(), AppError> {
    state.store.clear_log()
}

/* ---------- 设置 ---------- */

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> Result<AppSettings, AppError> {
    state.store.load_settings()
}

#[tauri::command]
pub async fn settings_set(settings: AppSettings, state: State<'_, AppState>) -> Result<(), AppError> {
    state.store.save_settings(&settings)
}

/* ---------- 版本 ---------- */

#[tauri::command]
pub fn app_version(app: AppHandle) -> Result<VersionInfo, AppError> {
    let pkg = app.package_info();
    Ok(VersionInfo {
        version: pkg.version.to_string(),
        build: format!("Tauri {} / Rust", pkg.crate_name),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub version: String,
    pub build: String,
}

/* ---------- 诊断 ---------- */

/// 前端启动自检 / 崩溃事件上报（写入数据目录 diagnostic.log，便于无控制台环境排查）
#[tauri::command]
pub fn diag_log(payload: String, app: AppHandle) -> Result<(), AppError> {
    use std::io::Write;
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("diagnostic.log"))?;
    writeln!(
        f,
        "[{}] {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        payload
    )?;
    Ok(())
}

/// 运行环境诊断：WebView2 是否存活 + 是否存在已知冲突的第三方输入法
#[tauri::command]
pub fn env_diag() -> Result<EnvDiag, AppError> {
    Ok(EnvDiag {
        webview_alive: crate::diag::webview_child_alive(std::process::id()),
        ime: crate::diag::ime_conflict(),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvDiag {
    /// 本进程是否仍挂着 WebView2 浏览器进程（false ≈ 界面已崩溃）
    pub webview_alive: bool,
    /// 存在冲突风险的第三方输入法名称（微软自带输入法为 null）
    pub ime: Option<String>,
}

/// 手动重建界面（WebView2 崩溃后由用户触发；会重置自动重试计数）
#[tauri::command]
pub fn webview_restart(app: AppHandle) -> Result<(), AppError> {
    #[cfg(windows)]
    {
        crate::watchdog::reset_attempts();
        crate::watchdog::rebuild(&app).map_err(|e| {
            crate::error::AppError::new("WEBVIEW_RESTART", format!("重建界面失败：{e}"))
                .with_hint("若反复失败，通常是第三方输入法与 WebView2 冲突，可临时切换到微软拼音后重启应用。")
        })?;
    }
    #[cfg(not(windows))]
    let _ = app;
    Ok(())
}

/* ---------- AppState 构建 ---------- */

impl AppState {
    pub fn build(app: &tauri::App) -> Result<Self, AppError> {
        let dir = app.path().app_data_dir()?;
        let store = std::sync::Arc::new(Store::new(dir)?);
        let db = std::sync::Arc::new(DbManager::new(std::sync::Arc::clone(&store)));
        Ok(Self { store, db })
    }
}
