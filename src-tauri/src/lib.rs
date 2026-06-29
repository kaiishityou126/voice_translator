mod asr;
mod capture;
mod config;
mod denoise;
mod pipeline;
mod segmenter;
mod transcript;
mod translate;

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager, State};

use config::RuntimeConfig;
use pipeline::PipelineHandle;

#[derive(Default)]
struct AppState {
    pipeline: Mutex<Option<PipelineHandle>>,
    /// 进程内识别器（SenseVoice / Qwen3）；引擎或源语言变更时重建。
    recognizer: Mutex<Option<Arc<dyn asr::Asr>>>,
}

/// emit("summary", ...) 负载：与字幕流式同思路。pending=true 增量帧（全量累计），false 终态。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryEvent {
    text: String,
    pending: bool,
}

/// load_summary 返回：最新会话是否有译文、是否已有摘要文件、摘要内容。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryFile {
    /// 是否存在非空的 .summary.<lang>.md
    exists: bool,
    /// 摘要内容（不存在则空串）
    text: String,
    /// 是否有可提炼的译文记录
    has_transcript: bool,
}

/// 诊断日志：release 下 windows_subsystem="windows"，eprintln 看不到，故写文件。
/// 追加到 app 配置目录/debug.log，带时间戳；失败静默忽略不影响主流程。
fn debug_log(app: &AppHandle, line: &str) {
    use std::io::Write;
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("debug.log"))
    {
        let _ = writeln!(f, "[{}] {}", ts, line);
    }
}

#[tauri::command]
fn start_translation(
    app: AppHandle,
    state: State<AppState>,
    config: RuntimeConfig,
) -> Result<(), String> {
    // 诊断日志：记录实际收到的关键配置（不记 API key），用于区分识别/翻译问题
    debug_log(
        &app,
        &format!(
            "start_translation: source={} asr={} engine={} srcLang={} tgtLang={} denoise={} silero={}",
            config.source,
            config.asr_engine,
            config.translation_engine,
            config.source_lang,
            config.target_lang,
            config.denoise,
            config.silero_vad
        ),
    );
    // 1. 设备自检（友好报错）
    capture::probe(&config.source).map_err(|e| format!("音频设备不可用: {}", e))?;

    // 1.5 模型按需下载：缺失则让前端先触发下载
    if !asr::model_present(&app, &config.asr_engine) {
        return Err("MODEL_MISSING".to_string());
    }

    // 2. 加载/复用进程内识别器（引擎或源语言变更时重建）
    let recognizer = {
        let mut guard = state.recognizer.lock().unwrap();
        let desired = asr::fingerprint_for(&config.asr_engine, &config.source_lang, &config.target_lang);
        let need_reload = match guard.as_ref() {
            Some(a) => a.fingerprint() != desired,
            None => true,
        };
        if need_reload {
            *guard = None;
            let a = asr::load(&app, &config.asr_engine, &config.source_lang, &config.target_lang)
                .map_err(|e| e.to_string())?;
            *guard = Some(a);
        }
        guard.as_ref().unwrap().clone()
    };

    // 3. 停掉可能在跑的旧流水线
    if let Some(h) = state.pipeline.lock().unwrap().take() {
        h.stop();
    }

    // 4. 启动新流水线（VAD 选择由 config.silero_vad 决定；Silero 模型已内嵌在 crate 中）
    let handle = pipeline::start(app, recognizer, config);
    *state.pipeline.lock().unwrap() = Some(handle);
    Ok(())
}

#[tauri::command]
fn stop_translation(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    if let Some(h) = state.pipeline.lock().unwrap().take() {
        h.stop();
    }
    // 无论谁触发停止（主窗 / 悬浮窗），都向所有窗口补发权威 idle，保证两窗联动。
    pipeline::emit_idle(&app);
    Ok(())
}

/// 运行中热切翻译目标语言：不重启流水线、不打断采集/识别。
/// 仅翻译线程感知；在途/已译段保持原语言，此后新段用新语言。空闲(无流水线)时静默忽略。
#[tauri::command]
fn set_target_lang(state: State<AppState>, target_lang: String) {
    if let Some(h) = state.pipeline.lock().unwrap().as_ref() {
        h.set_target_lang(target_lang);
    }
}

/// 指定 ASR 引擎的模型是否已下载到数据目录。
#[tauri::command]
fn model_exists(app: AppHandle, engine: String) -> bool {
    asr::model_present(&app, &engine)
}

/// 按需下载指定 ASR 引擎的模型（进度走 "model-progress" 事件）。
#[tauri::command]
fn download_model(app: AppHandle, engine: String) -> Result<(), String> {
    asr::download(&app, &engine).map_err(|e| e.to_string())
}

/// 开/关悬浮字幕窗（conf 定义的独立窗口，加载 overlay.html）。
/// 返回开启后的状态：true=已开，false=已关。
#[tauri::command]
fn toggle_overlay(app: AppHandle) -> Result<bool, String> {
    let w = match app.get_webview_window("overlay") {
        Some(w) => w,
        None => {
            debug_log(&app, "toggle_overlay: 找不到 overlay 窗口（conf 未创建）");
            return Err("找不到悬浮窗".to_string());
        }
    };
    let visible = w.is_visible().unwrap_or(false);
    debug_log(&app, &format!("toggle_overlay: 当前可见={}", visible));
    if visible {
        w.hide().map_err(|e| e.to_string())?;
        debug_log(&app, "toggle_overlay: 已 hide");
        Ok(false)
    } else {
        let r = w.show();
        debug_log(&app, &format!("toggle_overlay: show 结果={:?}", r));
        r.map_err(|e| e.to_string())?;
        let _ = w.set_focus();
        Ok(true)
    }
}

/// 设置持久化文件路径：app 配置目录下 settings.json。
fn settings_file(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

/// 读取已保存的设置 JSON（不存在/损坏返回 None，由前端用默认值兜底）。
#[tauri::command]
fn load_settings(app: AppHandle) -> Option<String> {
    let path = settings_file(&app).ok()?;
    std::fs::read_to_string(path).ok()
}

/// 同步写盘保存设置 JSON。走文件而非 localStorage，强杀/重装/强制退出都不丢最后一次改动。
#[tauri::command]
fn save_settings(app: AppHandle, json: String) -> Result<(), String> {
    let path = settings_file(&app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

/// 提炼本次会话译文的重点（仅 LLM 引擎）。读最新会话译文文件 → 流式提炼 →
/// emit("summary") 逐步回填，完成后写 <stem>.summary.<lang>.md。立即返回，结果走事件不阻塞 UI。
#[tauri::command]
fn summarize_session(app: AppHandle, config: RuntimeConfig) -> Result<(), String> {
    let engine = config.translation_engine.as_str();
    if engine != "openai" && engine != "ollama" {
        return Err("重点提炼需 LLM 引擎（OpenAI 兼容 / Ollama）".to_string());
    }
    let path = transcript::latest_transcript(&app).ok_or("没有可提炼的译文记录")?;
    let segs = transcript::read_session(&path).map_err(|e| e.to_string())?;
    // 摘要只用「有译文」的段：翻译失败的段译文为空，落盘保留但不喂给提炼 LLM
    let content = segs
        .iter()
        .filter(|s| !s.translated.trim().is_empty())
        .map(|s| format!("[原] {}\n[译] {}", s.original.trim(), s.translated.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    if content.trim().is_empty() {
        return Err("本次会话还没有可提炼的译文内容".to_string());
    }
    let summary_path = transcript::summary_path_for(&path, &config.target_lang);
    let app2 = app.clone();
    debug_log(
        &app,
        &format!(
            "summarize_session: start engine={} tgtLang={} chars={}",
            engine,
            config.target_lang,
            content.chars().count()
        ),
    );
    std::thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(180))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = app2.emit(
                    "summary",
                    SummaryEvent {
                        text: format!("[提炼失败] {}", e),
                        pending: false,
                    },
                );
                return;
            }
        };
        // 节流 ~100ms 一帧，全量累计文本，前端整块替换
        let mut last_emit: Option<std::time::Instant> = None;
        let on_chunk = |acc: &str| {
            let due = last_emit
                .map_or(true, |t| t.elapsed() >= std::time::Duration::from_millis(100));
            if due {
                last_emit = Some(std::time::Instant::now());
                let _ = app2.emit(
                    "summary",
                    SummaryEvent {
                        text: acc.to_string(),
                        pending: true,
                    },
                );
            }
        };
        let on_stage = |s: &str| debug_log(&app2, &format!("summarize: {}", s));
        let final_text = match translate::summarize(&client, &config, &content, on_chunk, on_stage) {
            Ok(s) => {
                let _ = std::fs::write(&summary_path, &s);
                debug_log(&app2, &format!("summarize_session: ok chars={}", s.chars().count()));
                s
            }
            Err(e) => {
                debug_log(&app2, &format!("summarize_session: failed: {}", e));
                format!("[提炼失败] {}", e)
            }
        };
        let _ = app2.emit(
            "summary",
            SummaryEvent {
                text: final_text,
                pending: false,
            },
        );
    });
    Ok(())
}

/// 读取最新会话、对应目标语言的摘要文件（首次提炼用）：有摘要则直接返回内容，避免重复调 LLM。
#[tauri::command]
fn load_summary(app: AppHandle, target_lang: String) -> Result<SummaryFile, String> {
    let Some(path) = transcript::latest_transcript(&app) else {
        return Ok(SummaryFile {
            exists: false,
            text: String::new(),
            has_transcript: false,
        });
    };
    let summary_path = transcript::summary_path_for(&path, &target_lang);
    if let Ok(text) = std::fs::read_to_string(&summary_path) {
        if !text.trim().is_empty() {
            return Ok(SummaryFile {
                exists: true,
                text,
                has_transcript: true,
            });
        }
    }
    Ok(SummaryFile {
        exists: false,
        text: String::new(),
        has_transcript: true,
    })
}

/// 保存手动编辑后的摘要内容，写回最新会话对应目标语言的 <stem>.summary.<lang>.md。
#[tauri::command]
fn save_summary(app: AppHandle, content: String, target_lang: String) -> Result<(), String> {
    let path = transcript::latest_transcript(&app).ok_or("没有可保存的会话记录")?;
    let summary_path = transcript::summary_path_for(&path, &target_lang);
    std::fs::write(&summary_path, content).map_err(|e| e.to_string())
}

/// 重译单段（前端「重译此句」）：复用 translate()，但**不带多轮上下文**（单段独立请求，
/// 质量略逊于流水线内重译，但无需依赖 pipeline 存活，停止后也可重译）。引擎/key 取当前 settings。
#[tauri::command]
async fn retranslate(original: String, config: RuntimeConfig) -> Result<String, String> {
    if config.translation_engine == "none" {
        return Err("纯字幕模式无需翻译".to_string());
    }
    if original.trim().is_empty() {
        return Err("原文为空".to_string());
    }
    // HTTP 阻塞调用放到 blocking 线程池，别卡住 async runtime
    tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        translate::translate(&client, &config, &original, &[], |_| {})
            .map_err(|e| format!("{:#}", e))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 用系统文件管理器打开 transcripts 目录（存译文与摘要文件）。
#[tauri::command]
fn open_summary_dir(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = transcript::transcripts_dir(&app).ok_or("无译文目录")?;
    app.opener()
        .open_path(dir.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// 列出历史会话（供「历史会话」面板），按时间倒序。
#[tauri::command]
fn list_sessions(app: AppHandle) -> Vec<transcript::SessionMeta> {
    transcript::list_sessions(&app)
}

/// 读取某历史会话的全部段落（只读回看）。校验 path 落在 transcripts 目录内，防目录穿越。
#[tauri::command]
fn load_session(app: AppHandle, path: String) -> Result<Vec<transcript::Segment>, String> {
    let dir = transcript::transcripts_dir(&app).ok_or("无译文目录")?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    let target = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !target.starts_with(&dir) {
        return Err("非法的会话路径".to_string());
    }
    transcript::read_session(&target).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_translation,
            stop_translation,
            set_target_lang,
            toggle_overlay,
            model_exists,
            download_model,
            load_settings,
            save_settings,
            summarize_session,
            open_summary_dir,
            load_summary,
            save_summary,
            retranslate,
            list_sessions,
            load_session
        ])
        // 启动时清理过期译文文件：读 settings.json 取保留天数/是否同删摘要（缺省 10/false）
        .setup(|app| {
            let handle = app.handle().clone();
            let (days, sum_days) = settings_file(&handle)
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    (
                        v.get("transcriptKeepDays")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(10),
                        v.get("summaryKeepDays")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                    )
                })
                .unwrap_or((10, 0));
            transcript::cleanup(&handle, days, sum_days);
            // Windows WebView2 透明窗口默认会刷白底，conf 的 transparent:true 不一定生效；
            // 显式把 overlay 窗口背景设为全透明，确保鼠标移开时只剩描边文字、背景透出桌面。
            if let Some(ov) = handle.get_webview_window("overlay") {
                let _ = ov.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
            }
            Ok(())
        })
        // 主界面 X = 整个 app 干净退出（隐藏的 overlay 窗口会把进程拖在后台，必须显式 exit）
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { .. } = event {
                    let app = window.app_handle();
                    if let Some(state) = app.try_state::<AppState>() {
                        if let Some(h) = state.pipeline.lock().unwrap().take() {
                            h.stop();
                        }
                        // 释放进程内识别器（无子进程，drop 仅回收模型内存）
                        drop(state.recognizer.lock().unwrap().take());
                    }
                    app.exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
