// 数据连接配置：连接测试 + 本地持久化（connections.json）
// 说明：
// - 密码以明文存入本地 JSON 文件 —— 本地单用户工具的已接受权衡
// - 每条配置带稳定 id，增删改按 id 定位（改名不会产生重复条目）
// - 读-改-写由 CONFIG_LOCK 串行化，避免并发调用互相覆盖
// - 文件 I/O 使用 tokio::fs，避免阻塞异步运行时的工作线程
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::Connection;
use sqlx::mysql::MySqlConnectOptions;
use sqlx::postgres::PgConnectOptions;
use sqlx::sqlite::SqliteConnectOptions;
use tauri::{AppHandle, Manager};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONFIG_FILE: &str = "connections.json";

// 串行化所有配置文件的读-改-写
static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    /// 稳定标识；旧数据无此字段时为 None，加载时自动补齐
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")] // JS 端字段名为 type
    pub db_type: String, // "mysql" | "postgresql" | "sqlite"
    pub host: String,    // sqlite: 文件绝对路径
    pub port: u16,       // sqlite 忽略
    pub username: String,
    pub password: String,
    pub database: String, // sqlite 忽略
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

// ---------- 连接测试 ----------

#[tauri::command]
pub async fn test_connection(config: ConnectionConfig) -> Result<String, String> {
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        match config.db_type.as_str() {
            "mysql" => test_mysql(&config).await,
            "postgresql" => test_postgres(&config).await,
            "sqlite" => test_sqlite(&config).await,
            other => Err(format!("不支持的数据库类型: {other}")),
        }
    })
    .await
    .map_err(|_| format!("连接超时（{} 秒），请检查主机地址与端口", CONNECT_TIMEOUT.as_secs()))?
}

async fn test_mysql(config: &ConnectionConfig) -> Result<String, String> {
    let opts = MySqlConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(&config.password)
        .database(&config.database);
    let mut conn = sqlx::MySqlConnection::connect_with(&opts)
        .await
        .map_err(|e| format!("MySQL 连接失败: {e}"))?;
    sqlx::query("SELECT 1")
        .execute(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("MySQL 连接成功: {}:{}", config.host, config.port))
}

async fn test_postgres(config: &ConnectionConfig) -> Result<String, String> {
    let opts = PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(&config.password)
        .database(&config.database);
    let mut conn = sqlx::PgConnection::connect_with(&opts)
        .await
        .map_err(|e| format!("PostgreSQL 连接失败: {e}"))?;
    sqlx::query("SELECT 1")
        .execute(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("PostgreSQL 连接成功: {}:{}", config.host, config.port))
}

async fn test_sqlite(config: &ConnectionConfig) -> Result<String, String> {
    // host 携带文件路径；create_if_missing(false) 让路径拼写错误直接失败，
    // 而不是悄悄创建一个空库文件。
    let opts = SqliteConnectOptions::new()
        .filename(&config.host)
        .create_if_missing(false);
    let mut conn = sqlx::SqliteConnection::connect_with(&opts)
        .await
        .map_err(|e| format!("SQLite 连接失败: {e}"))?;
    sqlx::query("SELECT 1")
        .execute(&mut conn)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("SQLite 连接成功: {}", config.host))
}

// ---------- 本地持久化 ----------

fn connections_file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(CONFIG_FILE))
}

async fn load_all(app: &AppHandle) -> Result<Vec<ConnectionConfig>, String> {
    let path = connections_file(app)?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), // 首次运行
        Err(e) => return Err(format!("读取配置文件失败: {e}")),
    };
    let mut list: Vec<ConnectionConfig> =
        serde_json::from_str(&raw).map_err(|e| format!("配置文件格式错误: {e}"))?;
    // 旧数据无 id：补齐并回写（一次性迁移）
    let mut changed = false;
    for cfg in &mut list {
        if cfg.id.is_none() {
            cfg.id = Some(new_id());
            changed = true;
        }
    }
    if changed {
        persist_all(app, &list).await?;
    }
    Ok(list)
}

async fn persist_all(app: &AppHandle, list: &[ConnectionConfig]) -> Result<(), String> {
    let path = connections_file(app)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    // 先写临时文件再 rename，近似原子，避免留下半写文件
    // （写入已被 CONFIG_LOCK 串行化，不会出现 tmp 冲突）
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, json).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, &path).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn save_connection(
    app: AppHandle,
    config: ConnectionConfig,
) -> Result<Vec<ConnectionConfig>, String> {
    let _guard = CONFIG_LOCK.lock().await;
    let name = config.name.trim().to_string();
    if name.is_empty() {
        return Err("连接名称不能为空".to_string());
    }
    let mut cfg = config;
    cfg.name = name;
    if cfg.id.is_none() {
        cfg.id = Some(new_id());
    }
    let mut list = load_all(&app).await?;
    // 按 id upsert：编辑改名后保存，替换原条目而不是新增
    if let Some(existing) = list.iter_mut().find(|c| c.id == cfg.id) {
        *existing = cfg;
    } else {
        list.push(cfg);
    }
    persist_all(&app, &list).await?;
    Ok(list)
}

#[tauri::command]
pub async fn list_connections(app: AppHandle) -> Result<Vec<ConnectionConfig>, String> {
    let _guard = CONFIG_LOCK.lock().await;
    // 返回完整配置（含密码）以便「编辑」回填表单 —— 本地单用户工具可接受
    load_all(&app).await
}

#[tauri::command]
pub async fn delete_connection(
    app: AppHandle,
    id: String,
) -> Result<Vec<ConnectionConfig>, String> {
    let _guard = CONFIG_LOCK.lock().await;
    let mut list = load_all(&app).await?;
    let before = list.len();
    list.retain(|c| c.id.as_deref() != Some(id.as_str()));
    if list.len() == before {
        return Err(format!("未找到连接配置: {id}"));
    }
    persist_all(&app, &list).await?;
    Ok(list)
}
