// 批量导入控制台：迁移自 MySQL_Batch_Import_Tool 的工作流，含 12 项改造优化
// 流程：连接 → 解析批量文件 → 删除旧数据(事务内) → 批量 INSERT →
//       行数核对 → COMMIT/ROLLBACK → 历史台账
// 关键设计：
// - 核对在 COMMIT 前完成（rows_affected 累加对比期望值），不符自动回滚
// - 全程不关闭唯一/外键检查（坏数据直接失败回滚，不做静默补救）
// - 所有 SQL 在单一事务内执行：任一环节失败 → 整体回滚（含删除）
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::mysql::{MySqlConnectOptions, MySqlSslMode};
use sqlx::Connection;
use tauri::{AppHandle, Emitter};

use crate::db::ConnectionConfig;
use crate::history::{find_hits, record_run, HistoryEntry, ReplayConfig, TableCount};
use crate::runlog;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BAD_LINE_SAMPLES: usize = 20;

// 串行化导入任务，防止双击并发两条管线
static IMPORT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------- 类型 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptInput {
    pub path: Option<String>, // 绝对路径；Rust 侧读取文件，不通过 IPC 传内容
    pub text: Option<String>, // 粘贴文本；非空优先
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest {
    pub connection: ConnectionConfig,
    pub files: Vec<String>,
    pub delete_script: Option<ScriptInput>,
    #[serde(default = "default_batch_rows")]
    pub batch_rows: u32,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    pub use_tls: bool,
    pub skip_history_warning: bool,
    /// 临时关闭外键检查（速度优先）；唯一检查始终开启
    #[serde(default)]
    pub disable_fk_checks: bool,
    /// 剔除脚本中携带的自增主键列（默认开启，由目标库重新生成主键）
    #[serde(default = "default_strip_auto_increment")]
    pub strip_auto_increment: bool,
    /// 导出分片上限（字节）；0 = 不分片（仅导出功能使用）
    #[serde(default)]
    pub max_part_bytes: u64,
}

fn default_batch_rows() -> u32 {
    1000
}
fn default_max_bytes() -> u64 {
    1_000_000
}
fn default_strip_auto_increment() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedFile {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub encoding: String,
    pub sha256: String,
    pub rows: u64,
    pub bad_lines: u64,
    pub bad_line_samples: Vec<String>,
    pub table_counts: Vec<TableCount>,
    pub batch_count: u32,
    /// 已剔除的自增列（"表.列" 形式）
    pub stripped_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteImpact {
    pub no: u32,
    pub sql_preview: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePreview {
    pub statement_count: u32,
    pub per_statement: Vec<DeleteImpact>,
    pub total_rows: u64,
    pub all_zero: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewReport {
    pub files: Vec<ParsedFile>,
    pub total_rows: u64,
    pub delete_preview: Option<DeletePreview>,
    pub history_hits: Vec<HistoryEntry>,
    pub tls_warning: Option<String>,
    pub max_allowed_packet: u64,
    pub effective_batch_bytes: u64,
    /// 本次将剔除的自增主键列（聚合去重）
    pub stripped_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCompare {
    pub table: String,
    pub expected: u64,
    pub actual: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedBatch {
    pub batch_no: u32,
    pub first_line: u64,
    pub last_line: u64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteStepResult {
    pub skipped: bool,
    pub skip_reason: Option<String>,
    pub total_rows: u64,
    pub per_statement: Vec<DeleteImpact>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertStepResult {
    pub batches: u32,
    pub rows: u64,
    pub tables: Vec<TableCompare>,
    pub failed_batch: Option<FailedBatch>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub committed: bool,
    pub error: Option<String>,
    pub files: Vec<ParsedFile>,
    pub delete_step: Option<DeleteStepResult>,
    pub insert_step: InsertStepResult,
    /// 其他提示（TLS、外键检查等，条数少）
    pub warnings: Vec<String>,
    /// 已剔除的自增主键列（聚合去重）
    pub stripped_columns: Vec<String>,
    pub history_recorded: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub stage: String,
    pub percent: u8,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPart {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub parts: Vec<ExportPart>,
    pub batches: u32,
    pub rows: u64,
    pub stripped_columns: Vec<String>,
    pub warnings: Vec<String>,
}

// ---------- 内部结构 ----------

#[derive(Debug)]
struct ParsedInsert {
    table: String,
    cols: Vec<String>,   // 列名（保留原始写法，可能带反引号）
    values: Vec<String>, // 与 cols 一一对应的值
}

#[derive(Debug)]
struct Batch {
    table: String,
    header: String,
    tails: Vec<String>,
    rows: u32,
    bytes: usize,
    first_line: u64,
    last_line: u64,
}

impl Batch {
    fn new(table: &str, header: &str, tail: String, line_no: u64) -> Self {
        let bytes = header.len() + tail.len() + 1; // header + 空格 + tail
        Self {
            table: table.to_string(),
            header: header.to_string(),
            tails: vec![tail],
            rows: 1,
            bytes,
            first_line: line_no,
            last_line: line_no,
        }
    }

    fn statement(&self) -> String {
        format!("{} {};", self.header, self.tails.join(","))
    }
}

struct FileParse {
    parsed: ParsedFile,
    batches: Vec<Batch>,
}

// ---------- 文件解析 ----------

/// 解析单行 INSERT：空行/注释返回 None，格式错误返回 Err（计坏行）
fn parse_insert_line(line: &str) -> Result<Option<ParsedInsert>, String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with("--") || t.starts_with('#') {
        return Ok(None);
    }
    let lower = t.to_lowercase();
    let insert_idx = lower
        .find("insert into")
        .ok_or("非 INSERT 语句")?;
    let rest = t[insert_idx + "insert into".len()..].trim_start();
    let (table, after_table) = if let Some(stripped) = rest.strip_prefix('`') {
        let end = stripped.find('`').ok_or("表名反引号未闭合")?;
        (&stripped[..end], &stripped[end + 1..])
    } else {
        let end = rest
            .find(|c: char| c == ' ' || c == '(')
            .ok_or("无法解析表名")?;
        (&rest[..end], &rest[end..])
    };
    let after_table = after_table.trim_start();
    if !after_table.starts_with('(') {
        return Err("缺少列列表".into());
    }
    // 括号深度匹配找到列列表结束
    let mut depth = 0usize;
    let mut col_end = None;
    for (i, ch) in after_table.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    col_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let col_end = col_end.ok_or("列列表括号未闭合")?;
    let cols = &after_table[..=col_end];
    let after_cols = after_table[col_end + 1..].trim_start();
    if !after_cols.to_lowercase().starts_with("values") {
        return Err("缺少 VALUES".into());
    }
    let tail = after_cols["values".len()..].trim();
    let tail = tail.strip_suffix(';').ok_or("语句缺少分号结尾")?.trim();
    if !tail.starts_with('(') || !tail.ends_with(')') {
        return Err("VALUES 格式异常".into());
    }
    // 切分列清单与值元组（顶层逗号切分，正确处理字符串/括号/转义）
    let cols = split_top_level(&cols[1..cols.len() - 1]);
    let values = split_top_level(&tail[1..tail.len() - 1]);
    if cols.len() != values.len() {
        return Err(format!("列数({})与值数({})不一致", cols.len(), values.len()));
    }
    Ok(Some(ParsedInsert {
        table: table.to_string(),
        cols,
        values,
    }))
}

/// 按顶层逗号切分：尊重单/双引号与反引号字符串（含 \\ 转义与 '' 双写转义）、
/// 括号深度（字符串内的逗号/括号不会误切）。
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            cur.push(c);
            if c == '\\' {
                // 反斜杠转义：吞掉下一个字符
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                continue;
            }
            if c == q {
                if chars.peek() == Some(&q) {
                    // '' 或 "" 双写转义
                    cur.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur = String::new();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

/// 解码：严格 UTF-8 优先（去 BOM），失败回退 GB18030
fn decode_bytes(bytes: &[u8]) -> (String, String) {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "UTF-8".into()),
        Err(_) => {
            let (s, _, _) = encoding_rs::GB18030.decode(bytes);
            (s.into_owned(), "GB18030".into())
        }
    }
}

/// 解析文件 + 合并批次。budget = min(max_bytes, max_allowed_packet/2)。
/// strip_map：表 → 自增主键列名；命中时从列清单与每行值中同步剔除。
fn parse_import_file(
    path: &str,
    batch_rows: u32,
    budget: usize,
    strip_map: &HashMap<String, String>,
) -> Result<FileParse, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取文件失败 {path}: {e}"))?;
    let size = bytes.len() as u64;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let (content, encoding) = decode_bytes(&bytes);

    let mut table_counts: HashMap<String, u64> = HashMap::new();
    let mut bad_lines = 0u64;
    let mut bad_line_samples: Vec<String> = Vec::new();
    let mut stripped_columns: Vec<String> = Vec::new();
    let mut batches: Vec<Batch> = Vec::new();
    let mut cur: Option<Batch> = None;

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx as u64 + 1;
        match parse_insert_line(line) {
            Ok(None) => continue,
            Ok(Some(mut ins)) => {
                *table_counts.entry(ins.table.clone()).or_default() += 1;
                // 剔除自增主键列（列与值按同一序号同步删除）
                if let Some(ai_col) = strip_map.get(&ins.table) {
                    let target = ai_col.to_lowercase();
                    let idx = ins.cols.iter().position(|c| {
                        c.trim().trim_matches('`').to_lowercase() == target
                    });
                    if let Some(i) = idx {
                        ins.cols.remove(i);
                        ins.values.remove(i);
                        let label = format!("{}.{}", ins.table, ai_col);
                        if !stripped_columns.contains(&label) {
                            stripped_columns.push(label);
                        }
                    }
                }
                let header = format!(
                    "INSERT INTO `{}` ({}) VALUES",
                    ins.table,
                    ins.cols.join(", ")
                );
                let tail = format!("({})", ins.values.join(", "));
                match cur.take() {
                    None => cur = Some(Batch::new(&ins.table, &header, tail, line_no)),
                    Some(mut b) => {
                        let add = b.bytes + tail.len() + 1; // +1 逗号
                        if b.table == ins.table && b.rows < batch_rows && add <= budget {
                            b.tails.push(tail);
                            b.rows += 1;
                            b.bytes = add;
                            b.last_line = line_no;
                            cur = Some(b);
                        } else {
                            batches.push(b);
                            cur = Some(Batch::new(&ins.table, &header, tail, line_no));
                        }
                    }
                }
            }
            Err(e) => {
                bad_lines += 1;
                if bad_line_samples.len() < BAD_LINE_SAMPLES {
                    bad_line_samples.push(format!("第 {line_no} 行: {e}"));
                }
            }
        }
    }
    if let Some(b) = cur {
        batches.push(b);
    }

    let rows = table_counts.values().sum();
    let mut table_counts: Vec<TableCount> = table_counts
        .into_iter()
        .map(|(table, rows)| TableCount { table, rows })
        .collect();
    table_counts.sort_by(|a, b| a.table.cmp(&b.table));

    let name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());

    Ok(FileParse {
        parsed: ParsedFile {
            path: path.to_string(),
            name,
            size,
            encoding,
            sha256,
            rows,
            bad_lines,
            bad_line_samples,
            table_counts,
            batch_count: batches.len() as u32,
            stripped_columns,
        },
        batches,
    })
}

/// 查询每张表的自增主键列名：table → column（目标库无自增列的表不在映射中）
async fn query_auto_increment_columns(
    conn: &mut sqlx::MySqlConnection,
    tables: &[String],
) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    for table in tables {
        let col: Option<String> = sqlx::query_scalar(
            "SELECT COLUMN_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND EXTRA LIKE '%auto_increment%'",
        )
        .bind(table)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| format!("查询表 {table} 自增列失败: {e}"))?;
        if let Some(c) = col {
            map.insert(table.clone(), c);
        }
    }
    Ok(map)
}

/// 解析所有文件；strip=true 时先识别目标库自增列再做剔除解析（两遍解析）。
/// 返回 (解析结果, 自增列映射)。
async fn parse_files_with_strip(
    files: &[String],
    batch_rows: u32,
    budget: usize,
    strip: bool,
    conn: &mut sqlx::MySqlConnection,
) -> Result<(Vec<FileParse>, HashMap<String, String>), String> {
    let paths = files.to_vec();
    let mut parses = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|p| parse_import_file(p, batch_rows, budget, &HashMap::new()))
            .collect::<Result<Vec<FileParse>, String>>()
    })
    .await
    .map_err(|e| e.to_string())??;

    let strip_map = if strip {
        let mut tables: Vec<String> = Vec::new();
        for f in &parses {
            for tc in &f.parsed.table_counts {
                tables.push(tc.table.clone());
            }
        }
        tables.sort();
        tables.dedup();
        let map = query_auto_increment_columns(conn, &tables).await?;
        if !map.is_empty() {
            // 第二遍：按剔除映射重新解析
            let paths2 = files.to_vec();
            let m2 = map.clone();
            parses = tokio::task::spawn_blocking(move || {
                paths2
                    .iter()
                    .map(|p| parse_import_file(p, batch_rows, budget, &m2))
                    .collect::<Result<Vec<FileParse>, String>>()
            })
            .await
            .map_err(|e| e.to_string())??;
        }
        map
    } else {
        HashMap::new()
    };
    Ok((parses, strip_map))
}

/// 聚合所有文件剔除的自增列（去重保序）
fn collect_stripped(files: &[FileParse]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        for s in &f.parsed.stripped_columns {
            if !out.contains(s) {
                out.push(s.clone());
            }
        }
    }
    out
}

// ---------- 脚本解析 ----------

/// 解析脚本输入：粘贴文本非空优先，否则读文件路径（解码同导入文件）
async fn resolve_script(input: Option<&ScriptInput>) -> Result<Option<(String, String)>, String> {
    let Some(input) = input else {
        return Ok(None);
    };
    if let Some(text) = &input.text {
        let t = text.trim().to_string();
        if !t.is_empty() {
            return Ok(Some((t, "粘贴文本".into())));
        }
    }
    if let Some(path) = &input.path {
        let p = path.clone();
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&p))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("读取脚本失败 {path}: {e}"))?;
        let (content, _) = decode_bytes(&bytes);
        let t = content.trim().to_string();
        if !t.is_empty() {
            return Ok(Some((t, path.clone())));
        }
    }
    Ok(None)
}

/// 按行切分语句：跳过空行与 -- / # 注释，去掉尾部 ;
fn split_statements(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("--") && !l.starts_with('#'))
        .map(|l| l.strip_suffix(';').unwrap_or(l).to_string())
        .collect()
}

fn preview_sql(sql: &str, max: usize) -> String {
    let chars: Vec<char> = sql.chars().collect();
    if chars.len() <= max {
        sql.to_string()
    } else {
        let cut: String = chars.into_iter().take(max).collect();
        format!("{cut}…")
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- 连接 ----------

async fn connect_mysql(
    config: &ConnectionConfig,
    use_tls: bool,
) -> Result<(sqlx::MySqlConnection, u64, Option<String>), String> {
    let mut opts = MySqlConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(&config.password)
        .database(&config.database);
    if use_tls {
        opts = opts.ssl_mode(MySqlSslMode::Required);
    }
    let tls_warning = if !use_tls && !["localhost", "127.0.0.1", "::1"].contains(&config.host.as_str())
    {
        Some(format!(
            "目标主机 {} 为远程地址且未强制 TLS，连接可能未加密",
            config.host
        ))
    } else {
        None
    };
    let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, sqlx::MySqlConnection::connect_with(&opts))
        .await
        .map_err(|_| format!("连接超时（{} 秒），请检查主机地址与端口", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|e| format!("连接失败: {e}"))?;
    // @@max_allowed_packet 是 BIGINT UNSIGNED，用 u64 解码（i64 会类型不匹配）
    let packet: u64 = sqlx::query_scalar("SELECT @@max_allowed_packet")
        .fetch_one(&mut conn)
        .await
        .map_err(|e| format!("读取 max_allowed_packet 失败: {e}"))?;
    Ok((conn, packet, tls_warning))
}

fn emit(app: &AppHandle, stage: &str, percent: u8, message: &str) {
    let _ = app.emit(
        "import-progress",
        ImportProgress {
            stage: stage.into(),
            percent,
            message: message.into(),
        },
    );
}

// ---------- 命令 ----------

#[tauri::command]
pub async fn stat_import_files(paths: Vec<String>) -> Result<Vec<FileStat>, String> {
    tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|p| {
                let meta = std::fs::metadata(p).map_err(|e| format!("{p}: {e}"))?;
                Ok(FileStat {
                    path: p.clone(),
                    name: Path::new(p)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.clone()),
                    size: meta.len(),
                })
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn preview_import(
    app: AppHandle,
    request: ImportRequest,
) -> Result<PreviewReport, String> {
    let _guard = IMPORT_LOCK.lock().await;
    if request.connection.db_type != "mysql" {
        return Err("仅支持 MySQL 类型的连接".into());
    }
    if request.files.is_empty() {
        return Err("请先选择导入文件".into());
    }

    emit(&app, "connect", 3, "正在连接数据库…");
    let (mut conn, packet, tls_warning) = connect_mysql(&request.connection, request.use_tls).await?;
    let budget = request.max_bytes.min(packet / 2);

    emit(&app, "parse", 10, "正在解析导入文件…");
    let (parses, _strip_map) = parse_files_with_strip(
        &request.files,
        request.batch_rows,
        budget as usize,
        request.strip_auto_increment,
        &mut conn,
    )
    .await?;

    emit(&app, "parse", 40, "正在检查导入历史…");
    let delete_text = resolve_script(request.delete_script.as_ref()).await?;

    let shas: Vec<String> = parses.iter().map(|f| f.parsed.sha256.clone()).collect();
    let history_hits = find_hits(&app, &shas).await?;
    runlog::append(
        &app,
        "预检",
        &format!(
            "完成: {} 个文件, {} 行, 历史命中 {} 个",
            parses.len(),
            parses.iter().map(|f| f.parsed.rows).sum::<u64>(),
            history_hits.len()
        ),
    )
    .await;

    // 删除 dry-run：事务内执行后回滚，精确影响行数、零数据变更
    let delete_preview = match delete_text.as_ref() {
        None => None,
        Some((text, _)) => {
            emit(&app, "dry_run", 60, "正在统计删除影响行数…");
            let stmts = split_statements(text);
            if stmts.is_empty() {
                Some(DeletePreview {
                    statement_count: 0,
                    per_statement: Vec::new(),
                    total_rows: 0,
                    all_zero: true,
                })
            } else {
                let mut tx = conn.begin().await.map_err(|e| format!("开启事务失败: {e}"))?;
                let mut per = Vec::new();
                for (i, sql) in stmts.iter().enumerate() {
                    let res = sqlx::query(sql)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| format!("删除脚本第 {} 条执行失败: {e}", i + 1))?;
                    per.push(DeleteImpact {
                        no: i as u32 + 1,
                        sql_preview: preview_sql(sql, 80),
                        rows: res.rows_affected(),
                    });
                }
                tx.rollback().await.map_err(|e| e.to_string())?;
                let total_rows = per.iter().map(|d| d.rows).sum();
                Some(DeletePreview {
                    statement_count: stmts.len() as u32,
                    all_zero: total_rows == 0,
                    per_statement: per,
                    total_rows,
                })
            }
        }
    };

    emit(&app, "done", 100, "预检完成");
    let total_rows = parses.iter().map(|f| f.parsed.rows).sum();
    let stripped_columns = collect_stripped(&parses);
    Ok(PreviewReport {
        files: parses.into_iter().map(|f| f.parsed).collect(),
        total_rows,
        delete_preview,
        history_hits,
        tls_warning,
        max_allowed_packet: packet,
        effective_batch_bytes: budget,
        stripped_columns,
    })
}

#[tauri::command]
pub async fn execute_import(
    app: AppHandle,
    request: ImportRequest,
) -> Result<ImportReport, String> {
    let _guard = IMPORT_LOCK.lock().await;
    let start = Instant::now();

    if request.connection.db_type != "mysql" {
        return Err("仅支持 MySQL 类型的连接".into());
    }
    if request.files.is_empty() {
        return Err("请先选择导入文件".into());
    }

    let mut warnings = Vec::new();
    emit(&app, "connect", 2, "正在连接数据库…");
    let (mut conn, packet, tls_warning) = connect_mysql(&request.connection, request.use_tls).await?;
    if let Some(w) = tls_warning {
        warnings.push(w);
    }
    let budget = request.max_bytes.min(packet / 2);

    emit(&app, "parse", 5, "正在解析导入文件…");
    let (parses, _strip_map) = parse_files_with_strip(
        &request.files,
        request.batch_rows,
        budget as usize,
        request.strip_auto_increment,
        &mut conn,
    )
    .await?;

    // 历史防重导
    let shas: Vec<String> = parses.iter().map(|f| f.parsed.sha256.clone()).collect();
    let history_hits = find_hits(&app, &shas).await?;
    if !request.skip_history_warning && !history_hits.is_empty() {
        return Err("所选文件此前已成功导入过，如需重复导入请在确认弹窗中继续".into());
    }
    runlog::append(
        &app,
        "导入",
        &format!("开始执行: {} 个文件", request.files.len()),
    )
    .await;

    let delete_text = resolve_script(request.delete_script.as_ref()).await?;

    // ---------- 事务开始 ----------
    emit(&app, "begin", 8, "开启事务…");
    let mut tx = conn.begin().await.map_err(|e| format!("开启事务失败: {e}"))?;
    let mut error: Option<String> = None;

    // 可选：临时关闭外键检查（会话级变量，本连接专用；唯一检查始终开启）
    if request.disable_fk_checks {
        sqlx::query("SET FOREIGN_KEY_CHECKS=0")
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("关闭外键检查失败: {e}"))?;
        warnings.push("已临时关闭外键检查（速度优先），建议配合核对脚本验证数据完整性".into());
    }

    // 删除旧数据
    let mut delete_step: Option<DeleteStepResult> = None;
    if let Some((text, _)) = delete_text.as_ref() {
        let stmts = split_statements(text);
        if !stmts.is_empty() {
            emit(&app, "delete", 12, &format!("执行删除脚本（{} 条）…", stmts.len()));
            let mut per = Vec::new();
            let mut failed = false;
            for (i, sql) in stmts.iter().enumerate() {
                match sqlx::query(sql).execute(&mut *tx).await {
                    Ok(res) => per.push(DeleteImpact {
                        no: i as u32 + 1,
                        sql_preview: preview_sql(sql, 80),
                        rows: res.rows_affected(),
                    }),
                    Err(e) => {
                        error = Some(format!("删除脚本第 {} 条失败: {e}", i + 1));
                        failed = true;
                        break;
                    }
                }
            }
            let total_rows = per.iter().map(|d| d.rows).sum();
            delete_step = Some(DeleteStepResult {
                skipped: failed,
                skip_reason: None,
                total_rows,
                per_statement: per,
            });
        }
    }

    // 批量 INSERT
    let mut insert_map: HashMap<String, u64> = HashMap::new();
    let mut failed_batch: Option<FailedBatch> = None;
    let mut batch_no = 0u32;
    let total_batches: u32 = parses.iter().map(|f| f.batches.len() as u32).sum();
    if error.is_none() {
        'insert: for f in &parses {
            for b in &f.batches {
                batch_no += 1;
                let stmt = b.statement();
                match sqlx::query(&stmt).execute(&mut *tx).await {
                    Ok(res) => {
                        let rows = res.rows_affected();
                        *insert_map.entry(b.table.clone()).or_default() += rows;
                        if batch_no % 10 == 0 || batch_no == total_batches {
                            let percent = 15 + (batch_no as u64 * 70 / total_batches.max(1) as u64) as u8;
                            emit(
                                &app,
                                "insert",
                                percent,
                                &format!(
                                    "第 {batch_no}/{total_batches} 批（源文件第 {}-{} 行）已插入",
                                    b.first_line, b.last_line
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        error = Some(format!("{}", e));
                        failed_batch = Some(FailedBatch {
                            batch_no,
                            first_line: b.first_line,
                            last_line: b.last_line,
                            message: e.to_string(),
                        });
                        break 'insert;
                    }
                }
            }
        }
    }

    // 行数核对：expected（解析统计）vs actual（rows_affected 累加）
    let mut table_compare: Vec<TableCompare> = Vec::new();
    if error.is_none() {
        let mut expected_total: HashMap<String, u64> = HashMap::new();
        for f in &parses {
            for tc in &f.parsed.table_counts {
                *expected_total.entry(tc.table.clone()).or_default() += tc.rows;
            }
        }
        for (table, expected) in &expected_total {
            let actual = insert_map.get(table).copied().unwrap_or(0);
            let ok = actual == *expected;
            if !ok && error.is_none() {
                error = Some(format!("表 {table} 行数核对不符：预期 {expected} 行，实际插入 {actual} 行"));
            }
            table_compare.push(TableCompare {
                table: table.clone(),
                expected: *expected,
                actual,
                ok,
            });
        }
        table_compare.sort_by(|a, b| a.table.cmp(&b.table));
    }

    // ---------- 提交 / 回滚 ----------
    let mut committed = false;
    let mut history_recorded = false;
    match &error {
        None => {
            emit(&app, "commit", 98, "全部检查通过，正在提交…");
            tx.commit().await.map_err(|e| format!("提交失败: {e}"))?;
            committed = true;
            // 写历史台账（SQLite，含可重放配置）
            let pf: Vec<&ParsedFile> = parses.iter().map(|f| &f.parsed).collect();
            let replay = ReplayConfig {
                connection_id: request.connection.id.clone(),
                file_paths: request.files.clone(),
                delete_script: request.delete_script.clone(),
                batch_rows: request.batch_rows,
                max_bytes: request.max_bytes,
                use_tls: request.use_tls,
                disable_fk_checks: request.disable_fk_checks,
                strip_auto_increment: request.strip_auto_increment,
            };
            record_run(&app, &pf, &replay).await?;
            history_recorded = true;
        }
        Some(_) => {
            emit(&app, "rollback", 99, "存在失败项，正在回滚…");
            let _ = tx.rollback().await;
        }
    }

    runlog::append(
        &app,
        "导入",
        &format!(
            "结束: {} ({} 行, 耗时 {}s)",
            if committed { "已提交" } else { "已回滚" },
            insert_map.values().sum::<u64>(),
            start.elapsed().as_secs_f64()
        ),
    )
    .await;
    if let Some(err) = &error {
        runlog::append(&app, "导入", &format!("失败原因: {err}")).await;
    }

    emit(&app, "done", 100, if committed { "导入完成" } else { "已回滚" });
    let insert_rows: u64 = insert_map.values().sum();
    Ok(ImportReport {
        committed,
        error,
        files: parses.iter().map(|f| f.parsed.clone()).collect(),
        delete_step,
        insert_step: InsertStepResult {
            batches: batch_no,
            rows: insert_rows,
            tables: table_compare,
            failed_batch,
        },
        warnings,
        stripped_columns: collect_stripped(&parses),
        history_recorded,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}


/// 导出合并后的批量 SQL 文件（供内网等离线环境使用）。
/// 连接可选：选了且能连通 → 做自增列剔除；未选 → 原样导出并提示。
#[tauri::command]
pub async fn export_import_script(
    app: AppHandle,
    request: ImportRequest,
    out_path: String,
) -> Result<ExportResult, String> {
    let _guard = IMPORT_LOCK.lock().await;
    if request.files.is_empty() {
        return Err("请先选择导入文件".into());
    }
    let mut warnings = Vec::new();

    // 解析：有连接 → 探测自增列并剔除（两遍）；无连接 → 原样导出
    let (parses, _strip_map) = if !request.connection.host.trim().is_empty() {
        match connect_mysql(&request.connection, request.use_tls).await {
            Ok((mut conn, packet, tls_warning)) => {
                if let Some(w) = tls_warning {
                    warnings.push(w);
                }
                let budget = request.max_bytes.min(packet / 2);
                parse_files_with_strip(
                    &request.files,
                    request.batch_rows,
                    budget as usize,
                    request.strip_auto_increment,
                    &mut conn,
                )
                .await?
            }
            Err(e) => {
                return Err(format!(
                    "目标连接不可用（{e}）。若无需剔除自增列，请取消选择目标连接后重试导出"
                ));
            }
        }
    } else {
        warnings.push(
            "未选择目标连接：导出内容未做自增主键列剔除，若脚本携带自增主键请注意目标环境冲突".into(),
        );
        let paths = request.files.clone();
        let batch_rows = request.batch_rows;
        let budget = request.max_bytes as usize;
        let parses = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .map(|p| parse_import_file(p, batch_rows, budget, &HashMap::new()))
                .collect::<Result<Vec<FileParse>, String>>()
        })
        .await
        .map_err(|e| e.to_string())??;
        (parses, HashMap::new())
    };

    // 语句列表（含每个文件的注释行）
    let mut stmts: Vec<String> = Vec::new();
    let mut batches = 0u32;
    let mut rows = 0u64;
    for f in &parses {
        stmts.push(format!("-- ---- 文件: {} ----\n", f.parsed.name));
        for b in &f.batches {
            stmts.push(format!("{}\n", b.statement()));
            batches += 1;
            rows += b.rows as u64;
        }
    }

    // 组装（按批次边界分片，每个分片独立事务，可顺序 source）
    let max_part = request.max_part_bytes;
    let (part_paths, part_sizes): (Vec<String>, Vec<u64>) = if max_part == 0 {
        let mut out = part_header(&parses, 1, 1, request.disable_fk_checks);
        for s in &stmts {
            out.push_str(s);
        }
        out.push_str("COMMIT;\n");
        let size = out.len() as u64;
        let p = out_path.clone();
        tokio::task::spawn_blocking(move || std::fs::write(&p, out.as_bytes()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("写入导出文件失败: {e}"))?;
        (vec![out_path], vec![size])
    } else {
        // 先干跑一遍估算分片数（头部长度用基准值近似，误差忽略）
        let base_len = part_header(&parses, 1, 1, request.disable_fk_checks).len();
        let mut n_parts = 1u32;
        let mut cur_len = base_len;
        for s in &stmts {
            if cur_len + s.len() + "COMMIT;\n".len() > max_part as usize {
                n_parts += 1;
                cur_len = base_len;
            }
            cur_len += s.len();
        }
        // 实际切分写入
        let mut paths = Vec::new();
        let mut sizes = Vec::new();
        let mut part_no = 1u32;
        let mut cur = part_header(&parses, part_no, n_parts, request.disable_fk_checks);
        for s in &stmts {
            if part_no < n_parts && cur.len() + s.len() + "COMMIT;\n".len() > max_part as usize {
                cur.push_str("COMMIT;\n");
                let path = part_path(&out_path, part_no);
                let size = cur.len() as u64;
                let (p, content) = (path.clone(), cur);
                tokio::task::spawn_blocking(move || std::fs::write(&p, content.as_bytes()))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("写入导出文件失败: {e}"))?;
                paths.push(path);
                sizes.push(size);
                part_no += 1;
                cur = part_header(&parses, part_no, n_parts, request.disable_fk_checks);
            }
            if s.len() + cur.len() > max_part as usize && part_no == n_parts && s.starts_with("INSERT") {
                warnings.push(format!(
                    "存在超过分片上限的单个批量语句（约 {} 字节），已单独成片",
                    s.len()
                ));
            }
            cur.push_str(s);
        }
        cur.push_str("COMMIT;\n");
        let path = part_path(&out_path, part_no);
        let size = cur.len() as u64;
        let (p, content) = (path.clone(), cur);
        tokio::task::spawn_blocking(move || std::fs::write(&p, content.as_bytes()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("写入导出文件失败: {e}"))?;
        paths.push(path);
        sizes.push(size);
        (paths, sizes)
    };

    let parts: Vec<ExportPart> = part_paths
        .into_iter()
        .zip(part_sizes)
        .map(|(path, size)| ExportPart { path, size })
        .collect();
    runlog::append(
        &app,
        "导出",
        &format!("完成: {} 批, {} 行, {} 个文件", batches, rows, parts.len()),
    )
    .await;
    Ok(ExportResult {
        parts,
        batches,
        rows,
        stripped_columns: collect_stripped(&parses),
        warnings,
    })
}

/// 导出文件头部（元信息 + SET 语句）；total > 1 时标注分片序号
fn part_header(parses: &[FileParse], part: u32, total: u32, disable_fk_checks: bool) -> String {
    let mut h = String::new();
    h.push_str("-- 再保运维工具 批量导入脚本");
    if total > 1 {
        h.push_str(&format!(" 第 {part}/{total} 部分"));
    }
    h.push('\n');
    h.push_str(&format!("-- 生成时间(unix ms): {}\n", now_ms()));
    for f in parses {
        h.push_str(&format!(
            "-- 源文件: {}  sha256: {}\n",
            f.parsed.name, f.parsed.sha256
        ));
    }
    let stripped = collect_stripped(parses);
    if !stripped.is_empty() {
        h.push_str(&format!("-- 已剔除自增主键列: {}\n", stripped.join(", ")));
    }
    if total > 1 {
        h.push_str("-- 注意：请在目标环境按 1..N 顺序执行；每个分片为独立事务\n");
    }
    h.push_str("SET NAMES utf8mb4;\nSET AUTOCOMMIT = 0;\n");
    if disable_fk_checks {
        h.push_str("SET FOREIGN_KEY_CHECKS = 0;\n");
    }
    h
}

/// 分片文件名：foo.sql → foo_part1.sql
fn part_path(base: &str, no: u32) -> String {
    match base.rfind('.') {
        Some(i) => format!("{}_part{}{}", &base[..i], no, &base[i..]),
        None => format!("{base}_part{no}"),
    }
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_temp(name: &str, content: &str) -> PathBuf {
        // 每个用例独立文件名：cargo test 并行执行，避免并发覆盖同一文件
        let dir = std::env::temp_dir().join(format!("import_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_line_normal() {
        let ins = parse_insert_line("INSERT INTO `fz_acc` (`a`, `b`) VALUES (1, 'x');")
            .unwrap()
            .unwrap();
        assert_eq!(ins.table, "fz_acc");
        assert_eq!(ins.cols, vec!["`a`", "`b`"]);
        assert_eq!(ins.values, vec!["1", "'x'"]);
    }

    #[test]
    fn parse_line_comment_and_blank() {
        assert!(parse_insert_line("").unwrap().is_none());
        assert!(parse_insert_line("   ").unwrap().is_none());
        assert!(parse_insert_line("-- comment").unwrap().is_none());
        assert!(parse_insert_line("# comment").unwrap().is_none());
    }

    #[test]
    fn parse_line_bad() {
        assert!(parse_insert_line("INSERT INTO `t` (`a`) VALUES (1)").is_err()); // 无分号
        assert!(parse_insert_line("SELECT 1;").is_err()); // 非 INSERT
        assert!(parse_insert_line("INSERT INTO `t` (`a`) VALUSE (1);").is_err()); // 缺少 VALUES
    }

    #[test]
    fn merge_respects_batch_rows() {
        let content = "INSERT INTO `t` (`a`) VALUES (1);\nINSERT INTO `t` (`a`) VALUES (2);\nINSERT INTO `t` (`a`) VALUES (3);\nINSERT INTO `t` (`a`) VALUES (4);\nINSERT INTO `t` (`a`) VALUES (5);\n";
        let path = write_temp("rows.sql", content);
        let fp = parse_import_file(path.to_str().unwrap(), 2, 1_000_000, &HashMap::new()).unwrap();
        assert_eq!(fp.parsed.rows, 5);
        assert_eq!(fp.batches.len(), 3);
        assert_eq!(fp.batches[0].first_line, 1);
        assert_eq!(fp.batches[0].last_line, 2);
        assert_eq!(fp.batches[2].first_line, 5);
        // 行号追踪：第二批覆盖 3-4 行
        assert_eq!(fp.batches[1].first_line, 3);
        assert_eq!(fp.batches[1].last_line, 4);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn merge_respects_budget() {
        let content = "INSERT INTO `t` (`a`) VALUES (1);\nINSERT INTO `t` (`a`) VALUES (2);\nINSERT INTO `t` (`a`) VALUES (3);\nINSERT INTO `t` (`a`) VALUES (4);\nINSERT INTO `t` (`a`) VALUES (5);\n";
        let path = write_temp("budget.sql", content);
        // 小预算：header 28 字节 + tail 3 字节 + 空格 = 32 字节/行，
        // 预算 35 只够装 1 行 → 每行独立成批
        let fp = parse_import_file(path.to_str().unwrap(), 1000, 35, &HashMap::new()).unwrap();
        assert_eq!(fp.batches.len(), 5, "小预算下每行应独立成批");
        assert_eq!(fp.batches[0].rows, 1);
        let stmt = fp.batches[0].statement();
        assert!(stmt.ends_with(';'));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn merge_splits_on_table_change() {
        let content = "INSERT INTO `t1` (`a`) VALUES (1);\nINSERT INTO `t2` (`a`) VALUES (2);\nINSERT INTO `t1` (`a`) VALUES (3);\n";
        let path = write_temp("tables.sql", content);
        let fp = parse_import_file(path.to_str().unwrap(), 1000, 1_000_000, &HashMap::new()).unwrap();
        assert_eq!(fp.batches.len(), 3); // 表切换必然切批
        assert_eq!(fp.parsed.table_counts.len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn split_statements_skips_comments_and_semicolon() {
        let text = "-- c\n# d\nDELETE FROM t WHERE a=1;\n\nDELETE FROM t WHERE a=2;";
        let stmts = split_statements(text);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "DELETE FROM t WHERE a=1");
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;

    #[test]
    fn split_top_level_handles_quotes_and_parens() {
        let parts = split_top_level("1, 'a,b', 'it''s', \"x(y)\", NULL, f(1,2), 'esc\\'q'");
        assert_eq!(parts.len(), 7);
        assert_eq!(parts[0], "1");
        assert_eq!(parts[1], "'a,b'");
        assert_eq!(parts[2], "'it''s'");
        assert_eq!(parts[3], "\"x(y)\"");
        assert_eq!(parts[4], "NULL");
        assert_eq!(parts[5], "f(1,2)");
        assert_eq!(parts[6], "'esc\\'q'");
    }

    #[test]
    fn strip_auto_increment_column() {
        let content = "INSERT INTO `t` (`id`, `a`, `b`) VALUES (1, 'x', 'y');\nINSERT INTO `t` (`id`, `a`, `b`) VALUES (2, 'p', 'q');\n";
        let dir = std::env::temp_dir().join(format!("import_test_strip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("strip.sql");
        std::fs::write(&path, content).unwrap();
        let mut map = HashMap::new();
        map.insert("t".to_string(), "id".to_string());
        let fp = parse_import_file(path.to_str().unwrap(), 1000, 1_000_000, &map).unwrap();
        assert_eq!(fp.parsed.rows, 2);
        assert_eq!(fp.parsed.stripped_columns, vec!["t.id"]);
        let stmt = fp.batches[0].statement();
        assert!(!stmt.contains("`id`"));
        assert!(stmt.contains("(`a`, `b`) VALUES"));
        assert!(stmt.contains("('x', 'y'),('p', 'q')"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_column_not_in_script_is_noop() {
        let content = "INSERT INTO `t` (`a`, `b`) VALUES ('x', 'y');\n";
        let dir = std::env::temp_dir().join(format!("import_test_noop_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("noop.sql");
        std::fs::write(&path, content).unwrap();
        let mut map = HashMap::new();
        map.insert("t".to_string(), "id".to_string());
        let fp = parse_import_file(path.to_str().unwrap(), 1000, 1_000_000, &map).unwrap();
        assert!(fp.parsed.stripped_columns.is_empty());
        let stmt = fp.batches[0].statement();
        assert!(stmt.contains("(`a`, `b`) VALUES"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn column_value_count_mismatch_is_bad_line() {
        assert!(parse_insert_line("INSERT INTO `t` (`a`, `b`) VALUES (1);").is_err());
        assert!(parse_insert_line("INSERT INTO `t` (`a`) VALUES (1, 2);").is_err());
    }
}
