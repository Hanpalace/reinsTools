// 导入历史台账（SQLite）：替代原 JSON 文件方案
// - 文件：{app_config_dir}/import_history.db（WAL 模式，建表幂等）
// - 迁移：首次使用且库文件不存在时，把旧 import_history.json 导入并归档为 .migrated
// - 同一批导入的多文件共享 run_id，查询时按 run 聚合
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use tauri::{AppHandle, Manager};

use crate::import::{ParsedFile, ScriptInput};

static HISTORY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const DB_FILE: &str = "import_history.db";
const OLD_JSON_FILE: &str = "import_history.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCount {
    pub table: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub sha256: String,
    pub ok: bool,
    pub at_ms: i64,
    pub rows: u64,
    pub tables: Vec<TableCount>,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFileRecord {
    pub sha256: String,
    pub name: String,
    pub rows: u64,
    pub tables: Vec<TableCount>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRun {
    pub run_id: i64,
    pub at_ms: i64,
    pub ok: bool,
    pub rows: u64,
    pub remark: String,
    pub files: Vec<HistoryFileRecord>,
    /// 可重放执行配置（旧版本台账为 null）
    pub replay_config: Option<ReplayConfig>,
}

/// 重放一次导入所需的完整配置（执行成功时随台账落库）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayConfig {
    pub connection_id: Option<String>,
    pub file_paths: Vec<String>,
    pub delete_script: Option<ScriptInput>,
    pub batch_rows: u32,
    pub max_bytes: u64,
    pub use_tls: bool,
    pub disable_fk_checks: bool,
    pub strip_auto_increment: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFileInput {
    pub name: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertHistoryRunRequest {
    pub remark: String,
    pub at_ms: i64,
    pub files: Vec<HistoryFileInput>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn pool(app: &AppHandle) -> Result<SqlitePool, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let opts = SqliteConnectOptions::new()
        .filename(dir.join(DB_FILE))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .map_err(|e| format!("打开历史库失败: {e}"))
}

/// 初始化：迁移旧 JSON（若需要）+ 建表建索引（幂等）
async fn init(app: &AppHandle) -> Result<(), String> {
    let _guard = HISTORY_LOCK.lock().await;
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let db_file = dir.join(DB_FILE);
    let old_json = dir.join(OLD_JSON_FILE);

    if !db_file.exists() && old_json.exists() {
        if let Ok(raw) = tokio::fs::read_to_string(&old_json).await {
            if let Ok(entries) = serde_json::from_str::<Vec<HistoryEntry>>(&raw) {
                let p = pool(app).await?;
                for e in &entries {
                    let tables_json =
                        serde_json::to_string(&e.tables).unwrap_or_else(|_| "[]".into());
                    sqlx::query(
                        "INSERT INTO import_records \
                         (run_id, sha256, file_name, rows, tables_json, ok, at_ms) \
                         VALUES (?, ?, ?, ?, ?, ?, ?)",
                    )
                    .bind(e.at_ms) // 旧数据无批次概念：每条一个独立 run
                    .bind(&e.sha256)
                    .bind(&e.name)
                    .bind(e.rows as i64)
                    .bind(&tables_json)
                    .bind(e.ok as i64)
                    .bind(e.at_ms)
                    .execute(&p)
                    .await
                    .map_err(|err| format!("迁移历史记录失败: {err}"))?;
                }
            }
        }
        let _ = tokio::fs::rename(&old_json, dir.join(format!("{OLD_JSON_FILE}.migrated"))).await;
    }

    let p = pool(app).await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS import_records (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            file_name TEXT NOT NULL DEFAULT '',
            rows INTEGER NOT NULL,
            tables_json TEXT NOT NULL DEFAULT '[]',
            ok INTEGER NOT NULL,
            at_ms INTEGER NOT NULL
        )",
    )
    .execute(&p)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_import_at_ms ON import_records(at_ms)")
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_import_sha ON import_records(sha256)")
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS run_meta (
            run_id INTEGER PRIMARY KEY,
            remark TEXT NOT NULL DEFAULT '',
            config_json TEXT NOT NULL DEFAULT 'null'
        )",
    )
    .execute(&p)
    .await
    .map_err(|e| e.to_string())?;
    // 兼容旧库：为已存在的 run_meta 补 config_json 列
    let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('run_meta')")
        .fetch_all(&p)
        .await
        .map_err(|e| e.to_string())?;
    if !cols.iter().any(|c| c == "config_json") {
        sqlx::query("ALTER TABLE run_meta ADD COLUMN config_json TEXT NOT NULL DEFAULT 'null'")
            .execute(&p)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 记录一次成功导入（同批文件共享 run_id），同时保存可重放配置
pub async fn record_run(
    app: &AppHandle,
    parses: &[&ParsedFile],
    config: &ReplayConfig,
) -> Result<(), String> {
    init(app).await?;
    let p = pool(app).await?;
    let run_id = now_ms();
    for f in parses {
        let tables_json = serde_json::to_string(&f.table_counts).map_err(|e| e.to_string())?;
        sqlx::query(
            "INSERT INTO import_records (run_id, sha256, file_name, rows, tables_json, ok, at_ms) \
             VALUES (?, ?, ?, ?, ?, 1, ?)",
        )
        .bind(run_id)
        .bind(&f.sha256)
        .bind(&f.name)
        .bind(f.rows as i64)
        .bind(&tables_json)
        .bind(run_id)
        .execute(&p)
        .await
        .map_err(|e| format!("写入导入历史失败: {e}"))?;
    }
    let config_json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    sqlx::query("INSERT OR IGNORE INTO run_meta (run_id, remark, config_json) VALUES (?, '', ?)")
        .bind(run_id)
        .bind(&config_json)
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 组装单个 HistoryRun（含 remark 与 replay_config）
async fn fetch_run(
    p: &SqlitePool,
    run_id: i64,
    at_ms: i64,
    ok_all: i64,
    total: i64,
) -> Result<HistoryRun, String> {
    let meta = sqlx::query("SELECT remark, config_json FROM run_meta WHERE run_id = ?")
        .bind(run_id)
        .fetch_optional(p)
        .await
        .map_err(|e| e.to_string())?;
    let (remark, replay_config) = match meta {
        Some(r) => (
            r.try_get::<String, _>(0).unwrap_or_default(),
            r.try_get::<String, _>(1)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok()),
        ),
        None => (String::new(), None),
    };
    let files = sqlx::query(
        "SELECT sha256, file_name, rows, tables_json FROM import_records \
         WHERE run_id = ? ORDER BY id",
    )
    .bind(run_id)
    .fetch_all(p)
    .await
    .map_err(|e| e.to_string())?;
    let file_records = files
        .into_iter()
        .map(|f| HistoryFileRecord {
            sha256: f.try_get(0).unwrap_or_default(),
            name: f.try_get(1).unwrap_or_default(),
            rows: f.try_get::<i64, _>(2).unwrap_or(0).max(0) as u64,
            tables: f
                .try_get::<String, _>(3)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
        })
        .collect();
    Ok(HistoryRun {
        run_id,
        at_ms,
        ok: ok_all == 1,
        rows: total.max(0) as u64,
        remark,
        files: file_records,
        replay_config,
    })
}

/// 按 sha256 查最近一次成功导入（防重导预检用）
pub async fn find_hits(app: &AppHandle, sha256s: &[String]) -> Result<Vec<HistoryEntry>, String> {
    if sha256s.is_empty() {
        return Ok(Vec::new());
    }
    init(app).await?;
    let p = pool(app).await?;
    let mut out = Vec::new();
    for sha in sha256s {
        let row = sqlx::query(
            "SELECT sha256, file_name, rows, tables_json, ok, at_ms \
             FROM import_records WHERE sha256 = ? AND ok = 1 ORDER BY at_ms DESC LIMIT 1",
        )
        .bind(sha)
        .fetch_optional(&p)
        .await
        .map_err(|e| e.to_string())?;
        if let Some(r) = row {
            out.push(HistoryEntry {
                sha256: r.try_get(0).unwrap_or_default(),
                name: r.try_get(1).unwrap_or_default(),
                rows: r.try_get::<i64, _>(2).unwrap_or(0).max(0) as u64,
                tables: r
                    .try_get::<String, _>(3)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                ok: r.try_get::<i64, _>(4).unwrap_or(0) == 1,
                at_ms: r.try_get(5).unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// 查询导入批次（按 run 聚合，最新在前）。过滤条件均可选：
/// keyword（说明/文件名模糊）、ok（1 成功 / 0 失败 / None 全部）、时间范围。
pub async fn list_runs(
    app: &AppHandle,
    limit: i64,
    keyword: Option<String>,
    ok: Option<i64>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> Result<Vec<HistoryRun>, String> {
    init(app).await?;
    let p = pool(app).await?;
    let kw = format!("%{}%", keyword.as_deref().unwrap_or_default().trim());
    let rows = sqlx::query(
        "SELECT r.run_id, MAX(r.at_ms), MIN(r.ok), SUM(r.rows), COUNT(*) \
         FROM import_records r \
         LEFT JOIN run_meta m ON m.run_id = r.run_id \
         WHERE (m.remark LIKE ? OR m.remark IS NULL) \
           AND r.run_id IN (SELECT run_id FROM import_records WHERE file_name LIKE ?) \
           AND (?3 IS NULL OR r.ok = ?3) \
           AND (?4 IS NULL OR r.at_ms >= ?4) \
           AND (?5 IS NULL OR r.at_ms <= ?5) \
         GROUP BY r.run_id ORDER BY MAX(r.at_ms) DESC LIMIT ?6",
    )
    .bind(&kw)
    .bind(&kw)
    .bind(ok)
    .bind(from_ms)
    .bind(to_ms)
    .bind(limit)
    .fetch_all(&p)
    .await
    .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for r in rows {
        let run_id: i64 = r.try_get(0).map_err(|e| e.to_string())?;
        let at_ms: i64 = r.try_get(1).map_err(|e| e.to_string())?;
        let ok_all: i64 = r.try_get(2).map_err(|e| e.to_string())?;
        let total: i64 = r.try_get::<Option<i64>, _>(3).map_err(|e| e.to_string())?.unwrap_or(0);
        out.push(fetch_run(&p, run_id, at_ms, ok_all, total).await?);
    }
    Ok(out)
}

/// 查询单条导入记录（含重放配置）
#[tauri::command]
pub async fn get_history_run(app: AppHandle, run_id: i64) -> Result<Option<HistoryRun>, String> {
    init(&app).await?;
    let p = pool(&app).await?;
    let row = sqlx::query(
        "SELECT run_id, MAX(at_ms), MIN(ok), SUM(rows) FROM import_records \
         WHERE run_id = ? GROUP BY run_id",
    )
    .bind(run_id)
    .fetch_optional(&p)
    .await
    .map_err(|e| e.to_string())?;
    let Some(r) = row else {
        return Ok(None);
    };
    let at_ms: i64 = r.try_get(1).map_err(|e| e.to_string())?;
    let ok_all: i64 = r.try_get(2).map_err(|e| e.to_string())?;
    let total: i64 = r.try_get::<Option<i64>, _>(3).map_err(|e| e.to_string())?.unwrap_or(0);
    Ok(Some(fetch_run(&p, run_id, at_ms, ok_all, total).await?))
}

#[tauri::command]
pub async fn list_import_history(
    app: AppHandle,
    keyword: Option<String>,
    ok: Option<i64>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> Result<Vec<HistoryRun>, String> {
    list_runs(&app, 200, keyword, ok, from_ms, to_ms).await
}

/// 手工登记一条导入记录（软件外执行的导入留痕）
#[tauri::command]
pub async fn add_history_run(
    app: AppHandle,
    request: UpsertHistoryRunRequest,
) -> Result<HistoryRun, String> {
    init(&app).await?;
    let files: Vec<&HistoryFileInput> = request
        .files
        .iter()
        .filter(|f| !f.name.trim().is_empty())
        .collect();
    if files.is_empty() {
        return Err("请至少登记一个文件".into());
    }
    let p = pool(&app).await?;
    let run_id = now_ms();
    for f in &files {
        sqlx::query(
            "INSERT INTO import_records (run_id, sha256, file_name, rows, tables_json, ok, at_ms) \
             VALUES (?, '', ?, ?, '[]', 1, ?)",
        )
        .bind(run_id)
        .bind(f.name.trim())
        .bind(f.rows as i64)
        .bind(request.at_ms)
        .execute(&p)
        .await
        .map_err(|e| format!("登记导入记录失败: {e}"))?;
    }
    sqlx::query("INSERT OR REPLACE INTO run_meta (run_id, remark) VALUES (?, ?)")
        .bind(run_id)
        .bind(request.remark.trim())
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    Ok(HistoryRun {
        run_id,
        at_ms: request.at_ms,
        ok: true,
        rows: files.iter().map(|f| f.rows).sum(),
        remark: request.remark.trim().to_string(),
        files: files
            .iter()
            .map(|f| HistoryFileRecord {
                sha256: String::new(),
                name: f.name.trim().to_string(),
                rows: f.rows,
                tables: Vec::new(),
            })
            .collect(),
        replay_config: None,
    })
}

/// 编辑导入记录：整体替换该批次的文件明细与说明
#[tauri::command]
pub async fn update_history_run(
    app: AppHandle,
    run_id: i64,
    request: UpsertHistoryRunRequest,
) -> Result<HistoryRun, String> {
    init(&app).await?;
    let p = pool(&app).await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT run_id FROM import_records WHERE run_id = ? LIMIT 1")
        .bind(run_id)
        .fetch_optional(&p)
        .await
        .map_err(|e| e.to_string())?;
    if exists.is_none() {
        return Err(format!("未找到导入记录: {run_id}"));
    }
    let files: Vec<&HistoryFileInput> = request
        .files
        .iter()
        .filter(|f| !f.name.trim().is_empty())
        .collect();
    if files.is_empty() {
        return Err("请至少保留一个文件".into());
    }
    sqlx::query("DELETE FROM import_records WHERE run_id = ?")
        .bind(run_id)
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    for f in &files {
        sqlx::query(
            "INSERT INTO import_records (run_id, sha256, file_name, rows, tables_json, ok, at_ms) \
             VALUES (?, '', ?, ?, '[]', 1, ?)",
        )
        .bind(run_id)
        .bind(f.name.trim())
        .bind(f.rows as i64)
        .bind(request.at_ms)
        .execute(&p)
        .await
        .map_err(|e| format!("更新导入记录失败: {e}"))?;
    }
    sqlx::query("INSERT OR REPLACE INTO run_meta (run_id, remark) VALUES (?, ?)")
        .bind(run_id)
        .bind(request.remark.trim())
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    Ok(HistoryRun {
        run_id,
        at_ms: request.at_ms,
        ok: true,
        rows: files.iter().map(|f| f.rows).sum(),
        remark: request.remark.trim().to_string(),
        files: files
            .iter()
            .map(|f| HistoryFileRecord {
                sha256: String::new(),
                name: f.name.trim().to_string(),
                rows: f.rows,
                tables: Vec::new(),
            })
            .collect(),
        replay_config: None,
    })
}

/// 删除一条导入记录（含文件明细与说明）
#[tauri::command]
pub async fn delete_history_run(app: AppHandle, run_id: i64) -> Result<(), String> {
    init(&app).await?;
    let p = pool(&app).await?;
    sqlx::query("DELETE FROM import_records WHERE run_id = ?")
        .bind(run_id)
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM run_meta WHERE run_id = ?")
        .bind(run_id)
        .execute(&p)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
