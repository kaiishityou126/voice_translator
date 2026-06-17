//! 流水线编排（两级并行）：
//!   采集线程 → 切段 →(seg)→ 识别线程 → 原文(pending)+(txt)→ 翻译线程 → 回填译文
//! 识别(本地)与翻译(远程)在不同线程，段 N+1 的识别可与段 N 的翻译并行，降低延迟。
//! 原文一识别出来就先推给前端显示，译文随后回填（同一 id）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::RuntimeConfig;
use crate::{asr, capture, translate};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleEvent {
    id: u64,
    original: String,
    translated: String,
    source_lang: String,
    ts: u128,
    pending: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusEvent {
    state: String,
    detail: Option<String>,
}

/// 全局流水线代号：每次 start 自增。旧代流水线的线程退出时可能延迟发事件，
/// 用代号守卫丢弃旧代的迟到事件（尤其翻译 worker 退出时的 idle），避免污染新流水线。
static PIPELINE_GEN: AtomicU64 = AtomicU64::new(0);

/// 全局字幕 id：单调递增，跨流水线重启不重置。前端靠 id 做原文→译文回填，
/// 若每次重启从 1 计数会与上一轮残留条目撞号，导致主窗口回填错位。
static NEXT_SUBTITLE_ID: AtomicU64 = AtomicU64::new(0);

fn emit_status(app: &AppHandle, gen: u64, state: &str, detail: Option<String>) {
    if PIPELINE_GEN.load(Ordering::SeqCst) != gen {
        return; // 非当前代，丢弃
    }
    let _ = app.emit(
        "status",
        StatusEvent {
            state: state.to_string(),
            detail,
        },
    );
}

fn emit_subtitle(app: &AppHandle, gen: u64, ev: SubtitleEvent) {
    if PIPELINE_GEN.load(Ordering::SeqCst) != gen {
        return; // 非当前代，丢弃
    }
    let _ = app.emit("subtitle", ev);
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub struct PipelineHandle {
    stop: Arc<AtomicBool>,
}

impl PipelineHandle {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        // 代号自增：让本代线程在退出前 emit 的迟到事件（listening/recognizing）
        // 被 emit_status 的守卫丢弃，避免前端 running 被翻回 true（停止需点两次）。
        PIPELINE_GEN.fetch_add(1, Ordering::SeqCst);
    }
}

/// 启动流水线。`recognizer` 是已加载好的进程内 SenseVoice 识别器（只读共享）。
pub fn start(app: AppHandle, recognizer: Arc<asr::SenseVoice>, cfg: RuntimeConfig) -> PipelineHandle {
    // 本次流水线代号：自增后所有线程只发本代事件，旧代迟到事件被守卫丢弃
    let my_gen = PIPELINE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let stop = Arc::new(AtomicBool::new(false));
    let (seg_tx, seg_rx) = mpsc::sync_channel::<Vec<f32>>(6); // 采集 → 识别（有界，防积压）
    let (txt_tx, txt_rx) = mpsc::sync_channel::<(u64, String)>(32); // 识别 → 翻译（有界）

    // ---- 采集线程 ----
    let stop_cap = stop.clone();
    let app_cap = app.clone();
    let gen_cap = my_gen;
    let source = cfg.source.clone();
    let denoise = cfg.denoise;
    let use_silero = cfg.silero_vad;
    let threshold = cfg.energy_threshold.unwrap_or(0.01);
    // 句末长停阈值：~1s 才判定一句说完。换气类短停（<1s）被容忍，不切，
    // 避免一句话被拦腰切两段；段内超长时由 segmenter 的短停阈值+软上限兜底收尾。
    let silence_ms = cfg.silence_ms.unwrap_or(1000);
    thread::spawn(move || {
        if let Err(e) = capture::capture_thread(
            &source,
            denoise,
            use_silero,
            threshold,
            silence_ms,
            stop_cap,
            seg_tx,
            app_cap.clone(),
        ) {
            emit_status(&app_cap, gen_cap, "error", Some(format!("采集失败: {}", e)));
        }
    });

    // ---- 识别线程（本地 SenseVoice，进程内）----
    let stop_asr = stop.clone();
    let app_asr = app.clone();
    let gen_asr = my_gen;
    let engine = cfg.translation_engine.clone();
    let asr_engine = recognizer;
    thread::spawn(move || {
        emit_status(&app_asr, gen_asr, "listening", None);

        // VAD 双阈值已保证每段≈一句完整话，识别结果直接成条（SenseVoice 自带标点/ITN）。
        // 把一段识别文本分配 id、推送前端、并按引擎决定是否送翻译。
        let flush_one = |text: String| {
            let id = NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
            // 纯字幕模式：原文即终态，不进翻译线程
            if engine == "none" {
                emit_subtitle(
                    &app_asr,
                    gen_asr,
                    SubtitleEvent {
                        id,
                        original: text,
                        translated: String::new(),
                        source_lang: String::new(),
                        ts: now_ms(),
                        pending: false,
                    },
                );
                return;
            }
            // 原文先推给前端（译文待回填）
            emit_subtitle(
                &app_asr,
                gen_asr,
                SubtitleEvent {
                    id,
                    original: text.clone(),
                    translated: String::new(),
                    source_lang: String::new(),
                    ts: now_ms(),
                    pending: true,
                },
            );
            // 交给翻译线程（有界）；满了说明翻译跟不上，标记跳过避免卡“翻译中…”
            match txt_tx.try_send((id, text)) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full((tid, otext))) => {
                    emit_subtitle(
                        &app_asr,
                        gen_asr,
                        SubtitleEvent {
                            id: tid,
                            original: otext,
                            translated: "[翻译积压，已跳过]".to_string(),
                            source_lang: String::new(),
                            ts: now_ms(),
                            pending: false,
                        },
                    );
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        };

        loop {
            if stop_asr.load(Ordering::Relaxed) {
                break;
            }
            match seg_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(seg) => {
                    emit_status(&app_asr, gen_asr, "recognizing", None);
                    let text = asr_engine.transcribe(&seg);
                    emit_status(&app_asr, gen_asr, "listening", None);
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // 幻觉熔断：单段无限复读 / 异常超长，直接丢弃，不污染翻译
                    if asr::is_hallucination(&text) {
                        eprintln!("[asr] 丢弃疑似重复幻觉段（{} 字）", text.chars().count());
                        continue;
                    }
                    // 固定幻觉短语（静音/背景被识别成套话）直接丢弃
                    if asr::is_filler_hallucination(&text) {
                        eprintln!("[asr] 丢弃固定幻觉短语：{}", text);
                        continue;
                    }
                    flush_one(text);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // ---- 翻译线程（远程 LLM，多路并发）----
    // 译文按 id 回填，前端容忍乱序，故可并发提升吞吐，应对语速突发/单段慢请求。
    const NUM_TRANSLATORS: usize = 2; // 并发 worker 数
    const CONTEXT_KEEP: usize = 4; // 历史保留条数
    const CONTEXT_USE: usize = 2; // 喂给 LLM 的前文条数
    let cfg = Arc::new(cfg); // 多 worker 共享只读配置
    let txt_rx = Arc::new(Mutex::new(txt_rx)); // 单生产者多消费者：worker 抢锁 try_recv
    // 已完成翻译的最近历史 (id, 原文, 译文)，作为 LLM 上下文；并发下按完成序近似，实时场景足够
    let history = Arc::new(Mutex::new(VecDeque::<(u64, String, String)>::new()));
    // 本会话译文落盘写入器（多 worker 共享，成功译文逐段追加）；纯字幕模式无翻译故不会产生文件
    let session_writer = crate::transcript::transcripts_dir(&app).map(|d| {
        let stem = crate::transcript::session_stem_now();
        Arc::new(crate::transcript::SessionWriter::new(d.join(format!("{stem}.txt"))))
    });

    for _ in 0..NUM_TRANSLATORS {
        let stop_tr = stop.clone();
        let app_tr = app.clone();
        let gen_tr = my_gen;
        let cfg = cfg.clone();
        let rx = txt_rx.clone();
        let hist = history.clone();
        let writer = session_writer.clone();
        thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(15)) // 实时场景：超时即放弃该段，不阻塞队列
                .build()
                .expect("http client");
            loop {
                if stop_tr.load(Ordering::Relaxed) {
                    break;
                }
                // 抢锁拿一个任务后立即释放锁（try_recv 不阻塞，保证多 worker 真并发）
                let item = { rx.lock().unwrap().try_recv() };
                match item {
                    Ok((id, text)) => {
                        emit_status(&app_tr, gen_tr, "translating", None);
                        let ctx: Vec<(String, String)> = {
                            let h = hist.lock().unwrap();
                            let n = h.len();
                            h.iter()
                                .skip(n.saturating_sub(CONTEXT_USE))
                                .map(|(_, o, t)| (o.clone(), t.clone()))
                                .collect()
                        };
                        // 流式回调：每帧把「截至目前的全量累计译文」按同一 id 回填（pending=true），
                        // 前端整条替换即可逐字增长。节流 ~100ms 一帧，避免事件风暴。
                        let mut last_emit: Option<std::time::Instant> = None;
                        let on_chunk = |acc: &str| {
                            let due = last_emit
                                .map_or(true, |t| t.elapsed() >= Duration::from_millis(100));
                            if due {
                                last_emit = Some(std::time::Instant::now());
                                emit_subtitle(
                                    &app_tr,
                                    gen_tr,
                                    SubtitleEvent {
                                        id,
                                        original: text.clone(),
                                        translated: acc.to_string(),
                                        source_lang: String::new(),
                                        ts: now_ms(),
                                        pending: true,
                                    },
                                );
                            }
                        };
                        let translated = match translate::translate(&client, &cfg, &text, &ctx, on_chunk) {
                            Ok(t) => t,
                            Err(e) => format!("[翻译失败] {}", e),
                        };
                        // 只把成功译文写进历史，避免把 "[翻译失败]" 当上下文喂回模型
                        if !translated.starts_with("[翻译失败]") {
                            let mut h = hist.lock().unwrap();
                            h.push_back((id, text.clone(), translated.clone()));
                            while h.len() > CONTEXT_KEEP {
                                h.pop_front();
                            }
                            // 同步落盘本段双语（供事后提炼重点）；失败段不写，不污染摘要输入
                            if let Some(w) = &writer {
                                w.append(&text, &translated);
                            }
                        }
                        // 终态帧：无条件发 pending=false 收尾（即使失败，也把最终态/失败标记定下来）
                        emit_subtitle(
                            &app_tr,
                            gen_tr,
                            SubtitleEvent {
                                id,
                                original: text,
                                translated,
                                source_lang: String::new(),
                                ts: now_ms(),
                                pending: false,
                            },
                        );
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
            emit_status(&app_tr, gen_tr, "idle", None);
        });
    }

    PipelineHandle { stop }
}
