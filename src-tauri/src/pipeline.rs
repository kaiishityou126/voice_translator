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
    let (seg_tx, seg_rx) = mpsc::sync_channel::<Vec<f32>>(6); // 采集 → 识别（有界，防积压）
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
    // 断句停顿（高级可调）；下限 200ms 保证 < preroll(200ms) 不矛盾。
    seg_limits.silero_min_silence_s = (cfg.seg_silence_ms.max(200) as f32) / 1000.0;
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
    // 说话人分离（仅入站）：开关热切读它；阈值启动时定；Diarizer 首次开启时在识别线程内惰性加载。
    let diar_flag = diar_enabled.clone();
    let diar_threshold = cfg.diarization_threshold;
    // 全局说话人库跨段归并阈值（可调）：段声纹与说话人质心相似度 ≥ 此值即归为同一人。
    let spk_match = cfg.speaker_merge_threshold;
    // 引擎名（判是否支持分离：Qwen3 无 token 时间戳，排除）
    let asr_engine_name = cfg.asr_engine.clone();
    thread::spawn(move || {
        emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);

        // VAD 双阈值已保证每段≈一句完整话，识别结果直接成条（SenseVoice 自带标点/ITN）。
        // 惰性加载的说话人分离器（仅入站、开关开启首次用到才加载；模型缺失/失败则保持 None 降级不拆）
        let mut diarizer: Option<crate::diarize::Diarizer> = None;
        // 是否已尝试加载分离器（失败也置 true，避免每段重试刷屏）
        let mut diar_tried = false;
        // 全局说话人库（A 在线增量）：跨段把同一个人归到稳定编号。
        // 复用声纹那份 CAM++ 提向量；提取器惰性加载（与分离器同期），库随会话存活、停止即弃。
        let mut spk_extractor: Option<crate::voiceprint::Extractor> = None;
        let mut spk_tried = false;
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
        // 普通（不分离）单段：纯字幕模式原文即终态；否则原文先显示再送翻译。
        let flush_one = |text: String| {
            let id = NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
            if engine == "none" {
                emit_orig(id, text, None, false);
            } else {
                emit_orig(id, text.clone(), None, true);
                send_tr(id, text, None);
            }
        };

        // 声纹维度失配只告警一次（模型换代/旧 voiceprint.json）：避免每段刷屏。
        let mut vp_dim_warned = false;

        loop {
            if stop_asr.load(Ordering::Relaxed) {
                break;
            }
            match seg_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(seg) => {
                    // 近场音量门(仅 out)：算段 RMS，低于阈值判为远场旁人/环境，丢弃不识别。in 路径不设门。
                    if dir_asr == "out" {
                        let rms = if seg.is_empty() {
                            0.0
                        } else {
                            (seg.iter().map(|s| s * s).sum::<f32>() / seg.len() as f32).sqrt()
                        };
                        if rms < MIC_GATE_RMS {
                            continue;
                        }
                        // 声纹门：过了音量粗筛再比对登记声纹，非本人（旁人）直接丢。
                        // 未登记/未开启时 gate 为 None，自动降级只保留 RMS 门。
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
                    // 入站且分离开启 → 取识别细节（含 token 时间戳）以便按说话人切文本；否则只取文本。
                    // Qwen3 是 LLM 解码、不产 token 时间戳，无法分离，直接排除——省去 transcribe_detailed 的额外开销。
                    let want_diar = dir_asr == "in"
                        && diar_flag.load(Ordering::Relaxed)
                        && asr_engine_name != "qwen3Asr";
                    let detail = if want_diar {
                        asr_engine.transcribe_detailed(&seg)
                    } else {
                        asr::AsrDetail {
                            text: asr_engine.transcribe(&seg),
                            tokens: Vec::new(),
                            timestamps: None,
                        }
                    };
                    emit_status(&app_asr, &stop_asr, &dir_asr, "listening", None);
                    let text = detail.text.trim().to_string();
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
                    // 说话人分离路径（入站 + 开关 + 有非空 token 时间戳）：同步跑 diar + 提声纹并入
                    // 全局说话人库，据结果整段翻译或按（全局稳定编号的）说话人拆成多行（门控单次翻译）。
                    // 无时间戳的引擎(如 Qwen3 返回空时间戳)不进此路径，也不加载分离模型。
                    let has_ts = detail.timestamps.as_ref().map_or(false, |t| !t.is_empty());
                    if want_diar && has_ts {
                        if diarizer.is_none() && !diar_tried {
                            diar_tried = true;
                            match crate::diarize::Diarizer::load(&app_asr, 1, diar_threshold) {
                                Ok(d) => {
                                    eprintln!("[diar:in] 分离器已加载（threshold={:.2}）", diar_threshold);
                                    diarizer = Some(d);
                                }
                                Err(e) => eprintln!("[diar:in] 未启用（{e:#}）"),
                            }
                        }
                        if let (Some(d), Some(ts)) = (diarizer.as_ref(), detail.timestamps.as_ref()) {
                            // 惰性加载声纹提取器（全局说话人库要用它把每段/每行的声纹并入库）
                            if spk_extractor.is_none() && !spk_tried {
                                spk_tried = true;
                                match crate::voiceprint::Extractor::load(&app_asr, 1) {
                                    Ok(e) => {
                                        eprintln!("[diar:in] 全局说话人库已启用（跨段声纹归并）");
                                        spk_extractor = Some(e);
                                    }
                                    Err(e) => eprintln!(
                                        "[diar:in] 声纹提取器不可用，退化为仅段内分离（{e:#}）"
                                    ),
                                }
                            }
                            // 同步跑 diar（几十毫秒）拿子段
                            let parts = d.process(&seg);
                            let mut spk: Vec<i32> = parts.iter().map(|p| p.2).collect();
                            spk.sort_unstable();
                            spk.dedup();
                            // 切 [a,b) 段样本提声纹 → 并入全局库，返回全局编号；提不出返回 None
                            const MIN_EMB: usize = 12_800; // 0.8s：短于此声纹不稳，只挂已有、绝不新建
                            let seg_ref = &seg;
                            let ex_ref = &spk_extractor;
                            let mut assign_g = |a: usize, b: usize| -> Option<i32> {
                                let ex = ex_ref.as_ref()?;
                                let a = a.min(seg_ref.len());
                                let b = b.min(seg_ref.len());
                                if b <= a {
                                    return None;
                                }
                                let emb = ex.embed(&seg_ref[a..b])?;
                                if b - a < MIN_EMB {
                                    // 短段：只往已知说话人挂，挂不上标未知，不新建虚假编号
                                    spk_registry.match_only(&emb, spk_match)
                                } else {
                                    Some(spk_registry.assign(&emb, spk_match))
                                }
                            };
                            // 段内多人才逐行归属；否则整段一个人
                            let (labeled, split): (Vec<(Option<i32>, String)>, bool) =
                                if spk.len() >= 2 {
                                    let runs =
                                        crate::diarize::attribute(&detail.tokens, ts, &parts);
                                    let mut local2g: std::collections::HashMap<i32, i32> =
                                        std::collections::HashMap::new();
                                    let mut v: Vec<(Option<i32>, String)> =
                                        Vec::with_capacity(runs.len());
                                    for (local, line, t0, t1) in &runs {
                                        let a = (t0 * 16_000.0) as usize;
                                        let b = (t1 * 16_000.0) as usize + 1;
                                        // 提不出声纹（行太短）时，复用本段同一局部说话人已定的全局号，防漂
                                        let g = assign_g(a, b)
                                            .or_else(|| local2g.get(local).copied());
                                        if let Some(g) = g {
                                            local2g.entry(*local).or_insert(g);
                                        }
                                        v.push((g, line.clone()));
                                    }
                                    let distinct = v
                                        .iter()
                                        .filter_map(|x| x.0)
                                        .collect::<std::collections::HashSet<_>>()
                                        .len();
                                    let split = v.len() >= 2 && distinct >= 2;
                                    (v, split)
                                } else {
                                    (Vec::new(), false)
                                };
                            if split {
                                // 多说话人：逐行发（同步执行→id 连续、顺序正确），speaker 为全局稳定编号
                                for (g, line) in &labeled {
                                    let lid =
                                        NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
                                    if engine == "none" {
                                        emit_orig(lid, line.clone(), *g, false);
                                    } else {
                                        emit_orig(lid, line.clone(), *g, true);
                                        send_tr(lid, line.clone(), *g);
                                    }
                                }
                            } else {
                                // 单人 / 拆分塌缩：整段一个说话人。已算过则复用，否则整段提一次声纹。
                                let g = labeled
                                    .iter()
                                    .find_map(|x| x.0)
                                    .or_else(|| assign_g(0, seg.len()));
                                let id = NEXT_SUBTITLE_ID.fetch_add(1, Ordering::Relaxed) + 1;
                                if engine == "none" {
                                    emit_orig(id, text.clone(), g, false);
                                } else {
                                    emit_orig(id, text.clone(), g, true);
                                    send_tr(id, text.clone(), g);
                                }
                            }
                            continue;
                        }
                        // diar 不可用（加载失败/无时间戳）→ 落普通路径
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
