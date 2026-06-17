//! 会话译文落盘 + 启动清理 + 摘要文件定位。
//! 每次 start→stop 是一个会话文件：app 配置目录/transcripts/<时间戳>.txt（双语逐段追加）。
//! 对应摘要 <时间戳>.summary.md。启动时清理 keep_days 之前的译文文件；摘要按开关决定是否同删。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager};

/// transcripts 目录（不存在则创建）。
pub fn transcripts_dir(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    let d = dir.join("transcripts");
    let _ = fs::create_dir_all(&d);
    Some(d)
}

/// 本地时间生成会话文件名（不含扩展）：YYYY-MM-DD_HHmmss。
pub fn session_stem_now() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string()
}

/// 会话译文写入器：多翻译 worker 共享，append 加锁防止两段交错。
pub struct SessionWriter {
    path: PathBuf,
    lock: Mutex<()>,
}

impl SessionWriter {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// 追加一段双语（原文 + 译文 + 空行）。文件首次写入时惰性创建。
    pub fn append(&self, original: &str, translated: &str) {
        let _g = self.lock.lock().unwrap();
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "[原] {}\n[译] {}\n", original, translated);
        }
    }
}

/// 最新的会话译文文件（按修改时间），用于「提炼本次会话」。
pub fn latest_transcript(app: &AppHandle) -> Option<PathBuf> {
    let dir = transcripts_dir(app)?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let p = entry.path();
        // 只看 .txt 译文文件；.summary.md 扩展是 md，自动排除
        if p.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Some(t) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
            continue;
        };
        if newest.as_ref().map_or(true, |(bt, _)| t > *bt) {
            newest = Some((t, p));
        }
    }
    newest.map(|(_, p)| p)
}

/// 目标语言归一化为摘要文件后缀（zh/en/ja，其余按 zh）。
pub fn lang_suffix(target_lang: &str) -> &'static str {
    match target_lang {
        "en" => "en",
        "ja" => "ja",
        _ => "zh",
    }
}

/// 译文文件对应某目标语言的摘要路径：<stem>.summary.<lang>.md。
/// 同一会话可按中/日/英分别存一份摘要，互不覆盖。
pub fn summary_path_for(transcript: &Path, target_lang: &str) -> PathBuf {
    let stem = transcript
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    let lang = lang_suffix(target_lang);
    transcript.with_file_name(format!("{stem}.summary.{lang}.md"))
}

/// 启动清理：
/// - .txt 译文文件按 transcript_days 清理（0 表示不清理）
/// - .summary.<lang>.md 摘要文件按 summary_days 清理（0 表示永久保留）
pub fn cleanup(app: &AppHandle, transcript_days: u64, summary_days: u64) {
    let Some(dir) = transcripts_dir(app) else {
        return;
    };
    let cutoff_for = |days: u64| {
        SystemTime::now()
            .checked_sub(Duration::from_secs(days.saturating_mul(24 * 3600)))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };
    let Ok(rd) = fs::read_dir(&dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_summary = name.contains(".summary.") && name.ends_with(".md");
        let is_txt = p.extension().and_then(|e| e.to_str()) == Some("txt");
        // 该类目保留天数为 0 表示永久保留，跳过
        let days = if is_summary {
            summary_days
        } else if is_txt {
            transcript_days
        } else {
            continue;
        };
        if days == 0 {
            continue;
        }
        let cutoff = cutoff_for(days);
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map_or(false, |t| t < cutoff);
        if old {
            let _ = fs::remove_file(&p);
        }
    }
}
