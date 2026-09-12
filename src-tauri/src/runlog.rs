// 运行日志：追加型文本文件 + 大小轮转（超过 5MB 备份为 .1）
// - 文件：{app_config_dir}/import_log.txt
// - 写入在 spawn_blocking 中执行，失败静默（日志不影响主流程）
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

const LOG_FILE: &str = "import_log.txt";
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const LOG_TAIL_DEFAULT: usize = 500;

/// 当前 UTC 时间的 "YYYY-MM-DD HH:MM:SS"（civil-from-days 算法，免 chrono 依赖）
pub fn utc_now_string() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = ms / 1000;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// 追加一行日志：[时间] [阶段] 内容
pub async fn append(app: &AppHandle, stage: &str, message: &str) {
    let Some(dir) = app.path().app_config_dir().ok() else {
        return;
    };
    let line = format!("[{}] [{}] {}\n", utc_now_string(), stage, message);
    let _ = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let path = dir.join(LOG_FILE);
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > LOG_MAX_BYTES {
                let _ = std::fs::rename(&path, dir.join("import_log.1.txt"));
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    })
    .await;
}

#[tauri::command]
pub async fn read_import_log(
    app: AppHandle,
    tail_lines: Option<usize>,
) -> Result<String, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    let n = tail_lines.unwrap_or(LOG_TAIL_DEFAULT).max(1);
    tokio::task::spawn_blocking(move || {
        let raw = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap_or_default();
        let lines: Vec<&str> = raw.lines().collect();
        let start = lines.len().saturating_sub(n);
        Ok::<String, String>(lines[start..].join("\n"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn clear_import_log(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_file(dir.join(LOG_FILE));
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}
