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
    /// 说话人分离得到的段内说话人号（None=未分离/未开启）；前端渲染成「说话人N」标签。
    #[serde(skip_serializing_if = "Option::is_none")]
    speaker: Option<i32>,
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
    // 运行中可热切的说话人分离开关（入站识别线程每段读一次）。仅入站有效。
    diar_enabled: Arc<AtomicBool>,
}

impl PipelineHandle {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 运行中热切翻译目标语言：仅影响此后开始翻译的段落，在途/已译段保持原语言。
    pub fn set_target_lang(&self, lang: String) {
        *self.target_lang.write().unwrap() = lang;
    }

    /// 运行中热切说话人分离开关：仅影响此后识别的段落，不重启流水线。
    pub fn set_diarization(&self, enabled: bool) {
        self.diar_enabled.store(enabled, Ordering::Relaxed);
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
    // 说话人分离开关（可运行中热切）；仅入站生效。启动值取配置。
    let diar_enabled = Arc::new(AtomicBool::new(cfg.diarization_enabled));
    let (seg_tx, seg_rx) = mpsc::sync_channel::<(Vec<f32>, crate::segmenter::CutReason)>(6); // 采集 → 识别（有界，防积压）
    // 识别 → 翻译（有界）：元素带说话人号(分离拆出的行)，None=未分离。
    let (txt_tx, txt_rx) = mpsc::sync_channel::<(u64, String, Option<i32>)>(32);

    // ---- TTS 回灌线程（开启时）：译文 → Kokoro 合成 → 串行播放到 CABLE Input ----
    // 有界通道满则丢弃（说话速度超过合成播放速度时不堆积），保持实时。
    let tts_tx = if let Some(synth) = tts.clone() {
        // 队列元素带"入队时刻"，供消费端按等待时长自适应语速
        let (tx, rx) = mpsc::sync_channel::<(String, Instant)>(8);
        let stop_tts = stop.clone();
        let app_tts = app.clone();
        let dir_tts = dir.clone();
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
                        if let Err(e) = player.play(&pcm) {
                            emit_status(
                                &app_tts,
                                &stop_tts,
                                &dir_tts,
                                "error",
                                Some(format!("回灸播放失败（输出设备可能已拔/切换）: {:#}", e)),
                            );
                            break;
                        }
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
    let mut seg_limits = crate::segmenter::SegLimits::for_engine(&cfg.asr_engine);
    // 分段最大时长按引擎取（高级可调）；下限 3s 防无效值。
    let seg_max_ov = if cfg.asr_engine == "qwen3Asr" {
        cfg.seg_max_sec_qwen3
    } else {
        cfg.seg_max_sec_sensevoice
    };
    if seg_max_ov >= 3.0 {
        seg_limits.silero_max_s = seg_max_ov;
    }
    // 断句停顿（VAD 停顿=句界）：下限 200ms 保证 < preroll(200ms) 不矛盾。
    seg_limits.silero_min_silence_s = (cfg.seg_silence_ms.max(200) as f32) / 1000.0;
    // ASR 线程 timeout 兜底用：距上一段超“上限+1s”无新段时强制定稿。
    let seg_max_s = seg_limits.silero_max_s;
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
    // 说话人分离（仅入站）：开关热切读它；整句贴标签用声纹提取器（惰性加载、复用 CAM++）。
    let diar_flag = diar_enabled.clone();
    // 全局说话人库跨段归并阈值（可调）：整句声纹与说话人质心相似度 ≥ 此值即归为同一人。
    let spk_match = cfg.speaker_merge_threshold;
    // 近场音量门（可调）：段 RMS 低于此判为远场旁人/杂音，丢弃不识别。仅麦克风源生效（回灌出站 + 麦克风翻译），环回系统声响度天然波动大，不设门。
    let mic_gate_rms = cfg.mic_gate_rms;
    let mic_source = cfg.source == "microphone";
    // 句缓冲超长强制定稿时长（墙钟）：无停顿连续独白兜底。
    let sentence_max_samples = (cfg.sentence_max_s.max(3.0) * 16_000.0) as usize;
    thread::spawn(move || {
        emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);

        // 攒句定稿：VAD 段只是喂料，累积成完整句才是翻译/落盘/贴标签单元（见 docs/实时精确断句-设计方案.md）。
        // 全局说话人库（整句贴标签）：复用 CAM++ 声纹提取器 + 会话级 SpeakerRegistry，惰性加载。
        let mut spk_extractor: Option<crate::voiceprint::Extractor> = None;
        let mut spk_tried = false;
        // diarize-first 段内分离器（pyannote+CAM++）：定稿时检测块内是否多人、按人切音频。惰性加载一次。
        let mut spk_diarizer: Option<crate::diarize::Diarizer> = None;
        let mut diar_tried = false;
        let mut spk_registry = crate::diarize::SpeakerRegistry::new();

        // 发一条字幕原文（translated 空，pending 由调用方定，可带说话人号）
        let emit_orig = |id: u64, original: String, speaker: Option<i32>, pending: bool| {
            emit_subtitle(
                &app_asr,
                &stop_asr,
                SubtitleEvent {
                    id,
                    original,
                    translated: String::new(),
                    ts: now_ms(),
                    pending,
                    direction: dir_asr.clone(),
                    speaker,
                },
            );
        };
        // 送翻译（有界）；满则发「积压跳过」终态帧
        let send_tr = |id: u64, text: String, speaker: Option<i32>| {
            match txt_tx.try_send((id, text.clone(), speaker)) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    emit_subtitle(
                        &app_asr,
                        &stop_asr,
                        SubtitleEvent {
                            id,
                            original: text,
                            translated: "[翻译积压，已跳过]".to_string(),
                            ts: now_ms(),
                            pending: false,
                            direction: dir_asr.clone(),
                            speaker,
                        },
                    );
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        };
        // 重识别用的 ASR 句柄（Arc 克隆）：diarize-first 多人分支里，对切出的单人音频重跑识别。
        let asr_commit = asr_engine.clone();
        // 三档归号：对一段音频提声纹归全局号（<1.3s 未知；1.3~2.5s 只挂已知不新建；≥2.5s 可新建）。
        let assign_speaker = |audio: &[f32],
                              ex: &Option<crate::voiceprint::Extractor>,
                              reg: &mut crate::diarize::SpeakerRegistry|
         -> Option<i32> {
            const SHORT_SAMPLES: usize = 20_800; // 1.3s
            const RELIABLE_SAMPLES: usize = 40_000; // 2.5s
            if audio.len() < SHORT_SAMPLES {
                return None;
            }
            ex.as_ref().and_then(|e| e.embed(audio)).and_then(|emb| {
                if audio.len() >= RELIABLE_SAMPLES {
                    Some(reg.assign(&emb, spk_match))
                } else {
                    reg.match_only(&emb, spk_match)
                }
            })
        };
        // 出一行字幕（原文即时显示 + 送翻译；none 引擎只出原文终态）。
        let emit_line = |id: u64, text: String, speaker: Option<i32>| {
            if engine == "none" {
                emit_orig(id, text, speaker, false);
            } else {
                emit_orig(id, text.clone(), speaker, true);
                send_tr(id, text, speaker);
            }
        };
        // 定稿：把当前句缓冲作为一句提交。
        //   want_speaker + 分离器可用 + 块够长 → diarize-first：块内按人切音频，逐段重识别 + 归号 + 多行出。
        //   单人 / 不满足条件 → 整句一行、整句归号（三档）。
        // buf/audio/id 由调用方传入并在调用后自行重置；spk_extractor/diarizer/registry 以参数传入避免借用冲突。
        let commit = |text: &str,
                      audio: &[f32],
                      id: u64,
                      ex: &Option<crate::voiceprint::Extractor>,
                      diar: &Option<crate::diarize::Diarizer>,
                      reg: &mut crate::diarize::SpeakerRegistry| {
            let t = text.trim();
            if t.is_empty() {
                return;
            }
            let want_speaker = dir_asr == "in" && diar_flag.load(Ordering::Relaxed);
            // ---- diarize-first 段内分离（仅入站 + 分离开关 + 块够长）----
            const MIN_DIAR_SAMPLES: usize = 48_000; // 3s：太短的块不值当分离、易过分割
            const MIN_TURN_S: f32 = 0.8; // 短于此的轮次并入相邻，消灭碎片
            if want_speaker && audio.len() >= MIN_DIAR_SAMPLES {
                if let Some(d) = diar {
                    let turns =
                        crate::diarize::split_turns(&d.process(audio), audio.len(), MIN_TURN_S);
                    let distinct = {
                        let mut v: Vec<i32> = turns.iter().map(|x| x.2).collect();
                        v.sort_unstable();
                        v.dedup();
                        v.len()
                    };
                    if distinct >= 2 && turns.len() >= 2 {
                        // 尊重 pyannote 局部号：为每个局部号定一个全局号——用该局部号的全部音频（各轮拼接）
                        // 提一次声纹归号；同局部→同全局；不同局部强制不同全局（阈值 2.0 使 assign 必新建，
                        // cosine≤1<2 永不命中），不让 CAM++ 把 pyannote 已分开的人并回同一号。CAM++ 只用于跨块衔接。
                        const SHORT_SAMPLES: usize = 20_800; // 1.3s：短于此声纹不可信 → 标未知
                        let mut label_audio: std::collections::HashMap<i32, Vec<f32>> =
                            std::collections::HashMap::new();
                        for &(a, b, local) in &turns {
                            label_audio
                                .entry(local)
                                .or_default()
                                .extend_from_slice(&audio[a..b]);
                        }
                        let mut label_global: std::collections::HashMap<i32, Option<i32>> =
                            std::collections::HashMap::new();
                        let mut used: Vec<i32> = Vec::new();
                        for (local, laud) in &label_audio {
                            let g = if laud.len() < SHORT_SAMPLES {
                                None
                            } else if let Some(emb) = ex.as_ref().and_then(|e| e.embed(laud)) {
                                let mut g = reg.assign(&emb, spk_match);
                                if used.contains(&g) {
                                    g = reg.assign(&emb, 2.0); // 本块已占该号 → 强制新建
                                }
                                used.push(g);
                                Some(g)
                            } else {
                                None
                            };
                            label_global.insert(*local, g);
                        }
                        // 逐轮重识别 + 出行；说话人号查 label_global。首轮复用 id（替换已显示的 pending 行），其余新建 id。
                        let mut emitted = false;
                        for (a, b, local) in turns {
                            let sub = asr_commit.transcribe(&audio[a..b]).trim().to_string();
                            // 空/纯标点/幻觉的碎片跳过，不出行
                            if sub.is_empty()
                                || !sub.chars().any(|c| c.is_alphanumeric())
                                || asr::is_hallucination(&sub)
                                || asr::is_filler_hallucination(&sub)
                            {
                                continue;
                            }
                            let spk = label_global.get(&local).copied().flatten();
                            let line_id = if emitted {
                                NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1
                            } else {
                                id
                            };
                            emit_line(line_id, sub, spk);
                            emitted = true;
                        }
                        // 只要出过至少一行就算处理完；全被过滤才落到下面整句兜底，避免这句整块消失。
                        if emitted {
                            return;
                        }
                    }
                }
            }
            // ---- 单人 / 不分离：整句一行 ----
            let speaker = if want_speaker {
                assign_speaker(audio, ex, reg)
            } else {
                None
            };
            emit_line(id, t.to_string(), speaker);
        };

        // 声纹维度失配只告警一次（模型换代/旧 voiceprint.json）：避免每段刷屏。
        let mut vp_dim_warned = false;

        // ===== 攒句缓冲 =====
        let mut buf_text = String::new(); // 累积原文
        let mut buf_audio: Vec<f32> = Vec::new(); // 累积 16k 音频（仅贴标签时用）
        let mut buf_id: u64 = 0; // 当前句 id（活跃期间刷新原文，定稿回填译文）
        let mut buf_open = false; // 是否有活跃句
        let mut buf_samples: usize = 0; // 累积音频样本数（超长兜底按音频算，不受 ASR 处理速度影响）
        let mut last_voice = Instant::now(); // 上一段到达时刻（timeout 兜底用）
        // 断句下沉到 VAD：VAD 停顿(0.75s)切段即句界，并给段打 Pause/Max 标签。
        // ASR 据标签：Pause 切(停顿)立即定稿；Max 切(顶到上限、半句)继续攒，靠 sentence_max 硬兜底。

        loop {
            if stop_asr.load(Ordering::Relaxed) {
                break;
            }
            match seg_rx.recv_timeout(Duration::from_millis(250)) {
                Ok((seg, cut)) => {
                    // 段到达时刻（transcribe 前取，用于测真实段间停顿，避免 ASR 耗时污染间隔）
                    let recv_at = Instant::now();
                    // 近场音量门：麦克风源(回灌出站 + 麦克风翻译)按段 RMS 挡远场旁人/杂音；环回系统声响度天然波动大，不设门。
                    if mic_source {
                        let rms = if seg.is_empty() {
                            0.0
                        } else {
                            (seg.iter().map(|s| s * s).sum::<f32>() / seg.len() as f32).sqrt()
                        };
                        if rms < mic_gate_rms {
                            continue;
                        }
                    }
                    // 声纹门(仅回灌出站)：过了音量粗筛再比对登记声纹，非本人（旁人）直接丢。
                    // 未登记/未开启时 gate 为 None，自动降级只保留 RMS 门。
                    if dir_asr == "out" {
                        if let Some(g) = &gate {
                            match g.accepts(&seg) {
                                crate::voiceprint::GateDecision::Accept => {}
                                crate::voiceprint::GateDecision::Reject => continue,
                                crate::voiceprint::GateDecision::DimMismatch => {
                                    // 登记声纹维度与当前模型不符：无法比对，降级放行（宁放勿全拦），仅提示一次重新登记。
                                    if !vp_dim_warned {
                                        vp_dim_warned = true;
                                        emit_status(
                                            &app_asr,
                                            &stop_asr,
                                            &dir_asr,
                                            "error",
                                            Some("声纹维度与当前模型不匹配，出站门控已降级放行，请重新登记声纹".to_string()),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    emit_status(&app_asr, &stop_asr, &dir_asr, "recognizing", None);
                    let text = asr_engine.transcribe(&seg).trim().to_string();
                    emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);
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
                    // ===== 攒句 =====
                    let want_speaker = dir_asr == "in" && diar_flag.load(Ordering::Relaxed);
                    // 贴标签需要声纹提取器，惰性加载一次
                    if want_speaker && spk_extractor.is_none() && !spk_tried {
                        spk_tried = true;
                        match crate::voiceprint::Extractor::load(&app_asr, 1) {
                            Ok(e) => spk_extractor = Some(e),
                            Err(e) => eprintln!("[spk:in] 声纹提取器不可用，说话人不标注（{e:#}）"),
                        }
                    }
                    // 段内分离器惰性加载一次（缺 pyannote/CAM++ 则降级为不分离，只按整句归号）。
                    if want_speaker && spk_diarizer.is_none() && !diar_tried {
                        diar_tried = true;
                        // 聚类阈值 0.7（越大说话人越少，偏保守防单人块被过分割）。
                        match crate::diarize::Diarizer::load(&app_asr, 1, 0.7) {
                            Ok(d) => spk_diarizer = Some(d),
                            Err(e) => eprintln!("[diar:in] 段内分离器不可用，退回整句归号（{e:#}）"),
                        }
                    }
                    // 瑕疵修复：preroll 段间重叠会把上段末尾词再识别成极短碎渣（如「ます。」）。
                    // 极短段 且 其文本正是当前句末尾后缀 → 判重复碎渣，丢弃不追加。
                    const TINY_SAMPLES: usize = 6_400; // 0.4s
                    if buf_open && seg.len() < TINY_SAMPLES {
                        let strip =
                            |c: char| matches!(c, '。' | '．' | '.' | '…' | '、' | '，' | ',' | ' ');
                        let tail = buf_text.trim_end_matches(strip);
                        let cur = text.trim_end_matches(strip).trim();
                        if !cur.is_empty() && tail.ends_with(cur) {
                            last_voice = recv_at;
                            continue;
                        }
                    }
                    // 说话人切换定稿已移除：段级声纹波动大（同人相邻段余弦可差 ±20%），据此切句是噪声误切；
                    // 换人分句改由 timeout 停顿定稿承担，归号由定稿时整句 assign 完成（整句声纹更稳）。
                    last_voice = recv_at;
                    // 开新句 or 追加喂料段
                    if !buf_open {
                        buf_id = NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
                        buf_open = true;
                        buf_samples = 0;
                        buf_text.clear();
                        buf_audio.clear();
                    }
                    if !buf_text.is_empty() {
                        buf_text.push(' ');
                    }
                    buf_text.push_str(&text);
                    if want_speaker {
                        buf_audio.extend_from_slice(&seg);
                    }
                    // 原文即时显示（pending，说话人号待定稿）
                    emit_orig(buf_id, buf_text.clone(), None, true);
                    // 断句：VAD 已在停顿处切段并标好切因。
                    // Pause 切(停顿=句界)立即定稿；Max 切(顶到上限=半句)继续攒，靠 sentence_max 硬兜底。
                    let is_max = matches!(cut, crate::segmenter::CutReason::Max);
                    buf_samples += seg.len();
                    if !is_max || buf_samples >= sentence_max_samples {
                        commit(&buf_text, &buf_audio, buf_id, &spk_extractor, &spk_diarizer, &mut spk_registry);
                        buf_open = false;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // 定稿主路已由“收到停顿切段”触发（VAD 已确认停顿）。
                    // 此处仅做兜底：若句缓冲开着且距上一段超过“上限+1s”都无新段（说话人真停了、
                    // 且末段被误判为上限切），强制定稿，避免末句卡到 sentence_max。
                    // 阈值 > 上限保证不会在连续独白（段间隔≤上限）中途偼跑。
                    if buf_open {
                        let stopped = Instant::now().duration_since(last_voice);
                        if stopped >= Duration::from_secs_f32(seg_max_s + 1.0) {
                            commit(&buf_text, &buf_audio, buf_id, &spk_extractor, &spk_diarizer, &mut spk_registry);
                            buf_open = false;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // 停止前 flush 残留句
        if buf_open {
            commit(&buf_text, &buf_audio, buf_id, &spk_extractor, &spk_diarizer, &mut spk_registry);
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
                .timeout(Duration::from_secs(cfg.translate_timeout_s)) // 兜底救偶发长句：本地 CPU 慢模型长句可能 20~30s，给足时间再放弃
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
                    Ok((id, text, speaker)) => {
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
                        // LLM 路径逐字流式（translate.rs 首译即流式，疑似未翻才事后清上下文重译并覆盖）；
                        // Google 无流式 → 整句一次到达。此处节流 ~100ms 防事件风暴。
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
                                        speaker,
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
                            w.append(id, now_ms(), &text, if failed { "" } else { &translated }, speaker);
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
                                speaker,
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

    PipelineHandle {
        stop,
        target_lang: live_target,
        diar_enabled,
    }
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
                Ok(mono48) => {
                    if let Err(e) = player.play_48k(&mono48) {
                        emit_status(
                            &app_play,
                            &stop_play,
                            "out",
                            "error",
                            Some(format!("回灸播放失败（输出设备可能已拔/切换）: {:#}", e)),
                        );
                        break;
                    }
                }
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
        diar_enabled: Arc::new(AtomicBool::new(false)),
    }
}
