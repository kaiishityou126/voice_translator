//! 流水线编排（两级并行）：
//!   采集线程 → 切段 →(seg)→ 识别线程 → 原文(pending)+(txt)→ 翻译线程 → 回填译文
//! 识别(本地)与翻译(远程)在不同线程，段 N+1 的识别可与段 N 的翻译并行，降低延迟。
//! 原文一识别出来就先推给前端显示，译文随后回填（同一 id）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    ts: u128,
    pending: bool,
    /// "in"=入站(听对方译中文) / "out"=出站(你说话译出语回灌)
    direction: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusEvent {
    state: String,
    detail: Option<String>,
    /// 事件来自哪条流水线："in"=入站(听对方) / "out"=出站(回灌) / "all"=全局(停止时的权威 idle)
    direction: String,
}

/// 全局字幕 id：单调递增，跨流水线重启不重置。前端靠 id 做原文→译文回填，
/// 若每次重启从 1 计数会与上一轮残留条目撞号，导致主窗口回填错位。
static NEXT_SUBTITLE_ID: AtomicU64 = AtomicU64::new(0);

/// out(麦克风/回灌)路径近场音量门阈值(段 RMS)。Silero VAD 只判「像不像人声」不判响度，
/// 两个座位外的旁人说话照样是人声 → 会被收进出站流水线误识别、灌 TTS。这里用响度粗筛：
/// 近场(你对着自己麦说话)响度远大于远场旁人，段 RMS 低于此阈值直接丢，不送 ASR。
/// 只作用于 out；in(系统音频环回)是干净数字信号，不设门。阈值靠 [gate:out] 埋点观察后收紧。
const MIC_GATE_RMS: f32 = 0.02;

fn emit_status(app: &AppHandle, stop: &AtomicBool, dir: &str, state: &str, detail: Option<String>) {
    if stop.load(Ordering::SeqCst) {
        return; // 本流水线已停，丢弃迟到事件
    }
    let _ = app.emit(
        "status",
        StatusEvent {
            state: state.to_string(),
            detail,
            direction: dir.to_string(),
        },
    );
}

/// 停止后由命令层直接补发一条权威 idle：绕过代号守卫。
/// stop() 会先把代号 +1，导致翻译线程退出时发的终态 idle 被 emit_status 守卫吞掉，
/// 那条 idle 又是同步所有窗口（主窗+悬浮窗）的唯一信号，故必须在此显式无条件补发。
pub fn emit_idle(app: &AppHandle) {
    let _ = app.emit(
        "status",
        StatusEvent {
            state: "idle".to_string(),
            detail: None,
            direction: "all".to_string(),
        },
    );
}

fn emit_subtitle(app: &AppHandle, stop: &AtomicBool, ev: SubtitleEvent) {
    if stop.load(Ordering::SeqCst) {
        return; // 本流水线已停，丢弃迟到事件
    }
    let _ = app.emit("subtitle", ev);
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 回灌语速自适应:按"这句译文在 TTS 队列里等了多久"给合成语速——
/// 等得越久=越落后 → 越快追赶,常态(基本不等)保持 1.0 自然。封顶 1.25(再快伤听感)。
/// 思路对齐实时同传的"滞后反馈":用实际时间滞后当信号,神经 TTS 变速不变调。
fn speed_for_lag(waited: Duration) -> f32 {
    const START_S: f32 = 1.5; // 低于此不提速(串行播放的正常等待)
    const CAP: f32 = 1.25; // 语速上限
    const SLOPE: f32 = 0.1; // 每多等 1s 提速 0.1
    let extra = waited.as_secs_f32() - START_S;
    if extra <= 0.0 {
        1.0
    } else {
        (1.0 + extra * SLOPE).min(CAP)
    }
}

pub struct PipelineHandle {
    stop: Arc<AtomicBool>,
    // 运行中可热切的目标语言（翻译 worker 每段读一次）。与采集/识别无关，故无需重启流水线。
    target_lang: Arc<RwLock<String>>,
}

impl PipelineHandle {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 运行中热切翻译目标语言：仅影响此后开始翻译的段落，在途/已译段保持原语言。
    pub fn set_target_lang(&self, lang: String) {
        *self.target_lang.write().unwrap() = lang;
    }
}

/// 启动流水线。`recognizer` 是已加载好的进程内识别器（SenseVoice / Qwen3，只读共享）。
/// `tts` 为已加载的 Kokoro 合成器（开启回灌时由命令层加载，否则 None）。
/// `gate` 为说话人声纹门（仅出站传 Some，入站恒 None）：非登记者的段丢弃。
pub fn start(
    app: AppHandle,
    recognizer: Arc<dyn asr::Asr>,
    tts: Option<Arc<crate::tts::Tts>>,
    gate: Option<crate::voiceprint::Gate>,
    direction: &str,
    cfg: RuntimeConfig,
) -> PipelineHandle {
    // auto 模式下解锁语种检测，让本轮重新判别（避免沿用上一轮锁定的语种）
    recognizer.reset_session();
    let dir = direction.to_string(); // "in"/"out" 标记，前端区分字幕来源
    let stop = Arc::new(AtomicBool::new(false));
    let (seg_tx, seg_rx) = mpsc::sync_channel::<Vec<f32>>(6); // 采集 → 识别（有界，防积压）
    let (txt_tx, txt_rx) = mpsc::sync_channel::<(u64, String)>(32); // 识别 → 翻译（有界）

    // ---- TTS 回灌线程（开启时）：译文 → Kokoro 合成 → 串行播放到 CABLE Input ----
    // 有界通道满则丢弃（说话速度超过合成播放速度时不堆积），保持实时。
    let tts_tx = if let Some(synth) = tts.clone() {
        // 队列元素带"入队时刻"，供消费端按等待时长自适应语速
        let (tx, rx) = mpsc::sync_channel::<(String, Instant)>(8);
        let stop_tts = stop.clone();
        // 空 output_device 时自动找 CABLE Input，找不到用默认输出
        let device = if cfg.output_device.is_empty() {
            crate::playback::find_cable_input().unwrap_or_default()
        } else {
            cfg.output_device.clone()
        };
        let sid = cfg.tts_voice;
        thread::spawn(move || {
            let mut player = match crate::playback::Player::open(&device) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[tts] 打开输出设备失败: {:#}", e);
                    return;
                }
            };
            loop {
                if stop_tts.load(Ordering::Relaxed) {
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok((text, enqueued)) => {
                        // 这句在队列里等了多久 → 越久越落后 → 自适应提速追赶(常态 1.0)
                        let waited = enqueued.elapsed();
                        let speed = speed_for_lag(waited);
                        if speed > 1.0 {
                            eprintln!("[tts] 积压 {:.1}s → 语速 {:.2}", waited.as_secs_f32(), speed);
                        }
                        // 崩溃取证埋点：Kokoro/espeak 若在合成内抛外部异常会直接 abort。
                        // 先把即将合成的文本(带 {:?} 转义，暴露控制字符/乱码/超长)打到 stderr(无缓冲)并 flush，
                        // abort 后这行即最后线索。定位完删除。
                        eprintln!(
                            "[tts] →synth len={} sid={} speed={:.2} text={:?}",
                            text.chars().count(),
                            sid,
                            speed,
                            text
                        );
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                        let pcm = synth.synth(&text, sid, speed);
                        eprintln!("[tts] ←synth 样本={}", pcm.len());
                        player.play(&pcm);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Some(tx)
    } else {
        None
    };

    // ---- 采集线程 ----
    let stop_cap = stop.clone();
    let stop_cap_err = stop.clone();
    let app_cap = app.clone();
    let dir_cap = dir.clone();
    let source = cfg.source.clone();
    let denoise = cfg.denoise;
    let use_silero = cfg.silero_vad;
    let threshold = cfg.energy_threshold.unwrap_or(0.01);
    // 句末长停阈值：~1s 才判定一句说完。换气类短停（<1s）被容忍，不切，
    // 避免一句话被拦腰切两段；段内超长时由 segmenter 的短停阈值+软上限兜底收尾。
    let silence_ms = cfg.silence_ms.unwrap_or(1000);
    // 切段时长上限按 ASR 引擎区分：SenseVoice 短段实时，Qwen3 长段给足上下文。
    let seg_limits = crate::segmenter::SegLimits::for_engine(&cfg.asr_engine);
    thread::spawn(move || {
        if let Err(e) = capture::capture_thread(
            &source,
            denoise,
            use_silero,
            threshold,
            silence_ms,
            seg_limits,
            stop_cap,
            seg_tx,
            app_cap.clone(),
        ) {
            emit_status(&app_cap, &stop_cap_err, &dir_cap, "error", Some(format!("采集失败: {}", e)));
        }
    });

    // ---- 识别线程（本地 SenseVoice，进程内）----
    let stop_asr = stop.clone();
    let app_asr = app.clone();
    let engine = cfg.translation_engine.clone();
    let asr_engine = recognizer;
    let dir_asr = dir.clone();
    thread::spawn(move || {
        emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);

        // VAD 双阈值已保证每段≈一句完整话，识别结果直接成条（SenseVoice 自带标点/ITN）。
        // 把一段识别文本分配 id、推送前端、并按引擎决定是否送翻译。
        let flush_one = |text: String| {
            let id = NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
            // 纯字幕模式：原文即终态，不进翻译线程
            if engine == "none" {
                emit_subtitle(
                    &app_asr,
                    &stop_asr,
                    SubtitleEvent {
                        id,
                        original: text,
                        translated: String::new(),
                        ts: now_ms(),
                        pending: false,
                        direction: dir_asr.clone(),
                    },
                );
                return;
            }
            // 原文先推给前端（译文待回填）
            emit_subtitle(
                &app_asr,
                &stop_asr,
                SubtitleEvent {
                    id,
                    original: text.clone(),
                    translated: String::new(),
                    ts: now_ms(),
                    pending: true,
                    direction: dir_asr.clone(),
                },
            );
            // 交给翻译线程（有界）；满了说明翻译跟不上，标记跳过避免卡“翻译中…”
            match txt_tx.try_send((id, text)) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full((tid, otext))) => {
                    emit_subtitle(
                        &app_asr,
                        &stop_asr,
                        SubtitleEvent {
                            id: tid,
                            original: otext,
                            translated: "[翻译积压，已跳过]".to_string(),
                            ts: now_ms(),
                            pending: false,
                            direction: dir_asr.clone(),
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
                    // 近场音量门(仅 out)：算段 RMS，低于阈值判为远场旁人/环境，丢弃不识别。
                    // 埋点打出每段 RMS(定阈值用，定完可保留或调静)。in 路径不设门。
                    if dir_asr == "out" {
                        let rms = if seg.is_empty() {
                            0.0
                        } else {
                            (seg.iter().map(|s| s * s).sum::<f32>() / seg.len() as f32).sqrt()
                        };
                        if rms < MIC_GATE_RMS {
                            eprintln!("[gate:out] 丢弃远场段 rms={:.4} < {:.4} (样本={})", rms, MIC_GATE_RMS, seg.len());
                            continue;
                        }
                        eprintln!("[gate:out] 放行 rms={:.4} (样本={})", rms, seg.len());
                        // 声纹门：过了音量粗筛再比对登记声纹，非本人（旁人）直接丢。
                        // 未登记/未开启时 gate 为 None，自动降级只保留 RMS 门。
                        if let Some(g) = &gate {
                            let (ok, score) = g.accepts(&seg);
                            if !ok {
                                eprintln!("[voiceprint:out] 丢弃非本人段 score={:.3} < {:.3}", score, g.threshold);
                                continue;
                            }
                            eprintln!("[voiceprint:out] 放行 score={:.3}", score);
                        }
                    }
                    emit_status(&app_asr, &stop_asr, &dir_asr, "recognizing", None);
                    // 崩溃取证埋点：sherpa 若在 transcribe 内抛外部异常会直接 abort。
                    // 先把「即将识别」的段长打到 stderr(无缓冲)并 flush；识别完再打识别文本(带 {:?} 转义)。
                    // abort 后若停在 →transcribe 说明崩在 ASR，停在 →synth 说明崩在 TTS。定位完删除。
                    eprintln!(
                        "[asr:{}] →transcribe 样本={} (~{:.2}s)",
                        dir_asr,
                        seg.len(),
                        seg.len() as f32 / 16_000.0
                    );
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    let text = asr_engine.transcribe(&seg);
                    eprintln!("[asr:{}] ←transcribe text={:?}", dir_asr, text);
                    emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    // 纯标点/符号段（噪音误触发后 ASR 只吐个「.」「。」之类）直接丢弃：
                    // 整段没有任何字母/汉字/假名/谚文（is_alphanumeric 覆盖 CJK/Hangul/Kana）即判定。
                    if !text.chars().any(|c| c.is_alphanumeric()) {
                        eprintln!("[asr] 丢弃纯标点/符号段：{}", text);
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
                    // Qwen3-ASR 对话模板泄漏（噪声/音乐段吐出 <asr_text>/system 等骨架 token）直接丢弃
                    if asr::is_template_leak(&text) {
                        eprintln!("[asr] 丢弃模板泄漏段：{}", text);
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
    // 目标语言独立成可变共享槽（其余配置仍只读）；set_target_lang 写它，worker 每段读它。
    let live_target = Arc::new(RwLock::new(cfg.target_lang.clone()));
    let cfg = Arc::new(cfg); // 多 worker 共享只读配置
    let txt_rx = Arc::new(Mutex::new(txt_rx)); // 单生产者多消费者：worker 抢锁 try_recv
    // 已完成翻译的最近历史 (id, 原文, 译文)，作为 LLM 上下文；并发下按完成序近似，实时场景足够
    let history = Arc::new(Mutex::new(VecDeque::<(u64, String, String)>::new()));
    // 本会话译文落盘写入器（多 worker 共享，成功译文逐段追加）；纯字幕/出站不写
    let session_writer = if cfg.write_transcript {
        crate::transcript::transcripts_dir(&app).map(|d| {
            let stem = crate::transcript::session_stem_now();
            Arc::new(crate::transcript::SessionWriter::new(d.join(format!("{stem}.jsonl"))))
        })
    } else {
        None
    };

    for _ in 0..NUM_TRANSLATORS {
        let stop_tr = stop.clone();
        let app_tr = app.clone();
        let dir_tr = dir.clone();
        let cfg = cfg.clone();
        let live_target = live_target.clone();
        let rx = txt_rx.clone();
        let hist = history.clone();
        let writer = session_writer.clone();
        let tts_tx = tts_tx.clone();
        thread::spawn(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30)) // 兜底救偶发长句：本地 CPU 慢模型长句可能 20~30s，给足时间再放弃
                .build()
                .expect("http client");
            loop {
                if stop_tr.load(Ordering::Relaxed) {
                    break;
                }
                // 抢锁拿一个任务后立即释放锁（try_recv 不阻塞，保证多 worker 真并发）
                // 抢锁阻塞等一个任务(最多 250ms 便于查 stop)；拿到后立即出作用域释放锁，再处理。
                // 锁只在「等待/接收」时持有，HTTP 翻译在锁外执行 → 多 worker 仍真并发；
                // 同一时刻只有一个 worker 在 recv 上等待，另一个阻塞在 lock() 上(非自旋、不耗 CPU)，
                // 一旦来消息持锁者立即取走并放锁，下一个 worker 接力等待。
                let item = { rx.lock().unwrap().recv_timeout(Duration::from_millis(250)) };
                match item {
                    Ok((id, text)) => {
                        emit_status(&app_tr, &stop_tr, &dir_tr, "translating", None);
                        let ctx: Vec<(String, String)> = {
                            let h = hist.lock().unwrap();
                            let n = h.len();
                            h.iter()
                                .skip(n.saturating_sub(CONTEXT_USE))
                                .map(|(_, o, t)| (o.clone(), t.clone()))
                                .collect()
                        };
                        // 译文回调：把「截至目前的全量累计译文」按同一 id 回填（pending=true），前端整条替换。
                        // 注：当前 LLM 路径为整句翻完一次性回填（translate.rs 先憋住判定语种再决定是否重译），
                        // Google 本就无流式 → 实际多为整句到达；此处保留节流 ~100ms 仅防极端事件风暴。
                        let mut last_emit: Option<std::time::Instant> = None;
                        let on_chunk = |acc: &str| {
                            let due = last_emit
                                .map_or(true, |t| t.elapsed() >= Duration::from_millis(100));
                            if due {
                                last_emit = Some(std::time::Instant::now());
                                emit_subtitle(
                                    &app_tr,
                                    &stop_tr,
                                    SubtitleEvent {
                                        id,
                                        original: text.clone(),
                                        translated: acc.to_string(),
                                        ts: now_ms(),
                                        pending: true,
                                        direction: dir_tr.clone(),
                                    },
                                );
                            }
                        };
                        // 热切目标语言：读共享槽，与启动值不同时克隆配置覆盖该字段后再翻译。
                        let want_lang = { live_target.read().unwrap().clone() };
                        let eff = if want_lang == cfg.target_lang {
                            None
                        } else {
                            let mut c = (*cfg).clone();
                            c.target_lang = want_lang;
                            Some(c)
                        };
                        let use_cfg: &RuntimeConfig = eff.as_ref().unwrap_or(&cfg);
                        let translated = match translate::translate(&client, use_cfg, &text, &ctx, on_chunk) {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("[translate] 失败 engine={} err={:#}", cfg.translation_engine, e);
                                format!("[翻译失败] {}", e)
                            }
                        };
                        let failed = translated.starts_with("[翻译失败]");
                        // 成功译文才进上下文历史，避免把失败标记当上下文喂回模型
                        if !failed {
                            let mut h = hist.lock().unwrap();
                            h.push_back((id, text.clone(), translated.clone()));
                            while h.len() > CONTEXT_KEEP {
                                h.pop_front();
                            }
                        }
                        // 回灌：成功译文送 TTS 线程合成播放；满则丢（合成跟不上语速时不堆积）
                        if !failed {
                            if let Some(tx) = &tts_tx {
                                let _ = tx.try_send((translated.clone(), Instant::now()));
                            }
                        }
                        // 落盘：成功写译文；失败写空译文（保留已识别原文，提炼时跳过空译文）。
                        // 不把冗长的 429/报错全文写进存档，保持文件干净、不污染摘要输入。
                        if let Some(w) = &writer {
                            w.append(id, now_ms(), &text, if failed { "" } else { &translated });
                        }
                        // 终态帧：无条件发 pending=false 收尾（即使失败，也把最终态/失败标记定下来）
                        emit_subtitle(
                            &app_tr,
                            &stop_tr,
                            SubtitleEvent {
                                id,
                                original: text,
                                translated,
                                ts: now_ms(),
                                pending: false,
                                direction: dir_tr.clone(),
                            },
                        );
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            emit_status(&app_tr, &stop_tr, &dir_tr, "idle", None);
        });
    }

    PipelineHandle { stop, target_lang: live_target }
}

/// 原声直通流水线（回灌·原声）：麦克风原始音频 → 直接播到 CABLE，不识别/翻译/合成。
pub fn start_passthrough(app: AppHandle, cfg: RuntimeConfig) -> PipelineHandle {
    let stop = Arc::new(AtomicBool::new(false));
    // 空 output_device 时自动找 CABLE Input
    let device = if cfg.output_device.is_empty() {
        crate::playback::find_cable_input().unwrap_or_default()
    } else {
        cfg.output_device.clone()
    };
    // 麦克风 48k 单声道帧（有界，满则丢弃保持实时）
    let (frame_tx, frame_rx) = mpsc::sync_channel::<Vec<f32>>(16);

    // 采集线程：麦克风原始 48k 单声道 → frame_tx
    let stop_cap = stop.clone();
    let app_cap = app.clone();
    let stop_cap_err = stop.clone();
    thread::spawn(move || {
        if let Err(e) = capture::raw_mic_thread(stop_cap, frame_tx, app_cap.clone()) {
            emit_status(&app_cap, &stop_cap_err, "out", "error", Some(format!("原声采集失败: {}", e)));
        }
    });

    // 播放线程：frame_rx → CABLE（直接 48k 写，不重采样）
    let stop_play = stop.clone();
    let app_play = app.clone();
    thread::spawn(move || {
        let mut player = match crate::playback::Player::open(&device) {
            Ok(p) => p,
            Err(e) => {
                emit_status(&app_play, &stop_play, "out", "error", Some(format!("打开输出设备失败: {:#}", e)));
                return;
            }
        };
        emit_status(&app_play, &stop_play, "out", "listening", None);
        loop {
            if stop_play.load(Ordering::Relaxed) {
                break;
            }
            match frame_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(mono48) => player.play_48k(&mono48),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        emit_status(&app_play, &stop_play, "out", "idle", None);
    });

    // 原声无目标语言，占位一个空槽以复用 PipelineHandle
    PipelineHandle {
        stop,
        target_lang: Arc::new(RwLock::new(String::new())),
    }
}
