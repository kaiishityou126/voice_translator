//! 会话转写落盘 + 启动清理 + 摘要文件定位。
//! 每次 start→stop 是一个会话文件：app 配置目录/transcripts/<时间戳>.jsonl（逐段一行一条 JSON）。
//! 每行结构：{id, ts, original, translated, speaker}；translated 为空串表示未成功翻译。
//! 对应摘要 <时间戳>.summary.<lang>.md。启动时清理 keep_days 之前的转写文件；摘要按开关决定是否同删。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// 一段会话记录（JSONL 每行一条）。speaker 预留给后续说话人分离，当前恒为 None。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: u64,
    pub ts: u128,
    pub original: String,
    pub translated: String,
    #[serde(default)]
    pub speaker: Option<String>,
}

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

    /// 追加一段（一行 JSON）。文件首次写入时惰性创建。
    /// translated 传空串表示未成功翻译（提炼时会跳过空译文）。
    pub fn append(&self, id: u64, ts: u128, original: &str, translated: &str) {
        let _g = self.lock.lock().unwrap();
        let seg = Segment {
            id,
            ts,
            original: original.to_string(),
            translated: translated.to_string(),
            speaker: None,
        };
        if let Ok(line) = serde_json::to_string(&seg) {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
}

/// 读取并解析一个 .jsonl 会话文件为有序段落列表（跳过无法解析的行）。
pub fn read_session(path: &Path) -> std::io::Result<Vec<Segment>> {
    let content = fs::read_to_string(path)?;
    let segs = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Segment>(l).ok())
        .collect();
    Ok(segs)
}

/// 历史会话条目元信息（供历史列表 UI）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    /// 文件名（不含扩展），即 YYYY-MM-DD_HHmmss
    pub stem: String,
    /// 绝对路径（load_session 用）
    pub path: String,
    /// 段落数
    pub count: usize,
    /// 预览：前若干段原文拼接（截断）
    pub preview: String,
}

/// 列出所有 .jsonl 历史会话，按文件名（时间戳）倒序（新→旧）。
pub fn list_sessions(app: &AppHandle) -> Vec<SessionMeta> {
    let Some(dir) = transcripts_dir(app) else {
        return Vec::new();
    };
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut metas: Vec<SessionMeta> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session")
            .to_string();
        let segs = read_session(&p).unwrap_or_default();
        if segs.is_empty() {
            continue;
        }
        let mut preview = String::new();
        for s in segs.iter().take(3) {
            if !preview.is_empty() {
                preview.push_str(" / ");
            }
            preview.push_str(s.original.trim());
            if preview.chars().count() > 60 {
                break;
            }
        }
        let preview: String = preview.chars().take(60).collect();
        metas.push(SessionMeta {
            stem,
            path: p.to_string_lossy().into_owned(),
            count: segs.len(),
            preview,
        });
    }
    // 文件名按时间戳格式，字典序倒序即时间倒序
    metas.sort_by(|a, b| b.stem.cmp(&a.stem));
    metas
}

/// 最新的会话译文文件（按修改时间），用于「提炼本次会话」。
pub fn latest_transcript(app: &AppHandle) -> Option<PathBuf> {
    let dir = transcripts_dir(app)?;
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let p = entry.path();
        // 只看 .jsonl 转写文件；.summary.<lang>.md 扩展是 md，自动排除
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
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

/// 目标语言归一化为摘要文件后缀（已知语种用自身 code，其余按 zh）。
pub fn lang_suffix(target_lang: &str) -> &'static str {
    match target_lang {
        "en" => "en",
        "ja" => "ja",
        "ko" => "ko",
        "yue" => "yue",
        "fr" => "fr",
        "es" => "es",
        "de" => "de",
        "ru" => "ru",
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
/// - .jsonl 转写文件按 transcript_days 清理（0 表示不清理）
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
        let ext = p.extension().and_then(|e| e.to_str());
        // .jsonl 为当前转写格式；.txt 为旧格式，同按 transcript_days 自然老化
        let is_transcript = ext == Some("jsonl") || ext == Some("txt");
        // 该类目保留天数为 0 表示永久保留，跳过
        let days = if is_summary {
            summary_days
        } else if is_transcript {
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
