//! 语音切段器：把 16kHz 单声道流按「语音/静音」边界切成语音段。
//! 两种后端（`Segmenter` 枚举分发）：
//! - Silero（首选）：直接用 sherpa-onnx 自带 VAD 的**原生分段**，在自然停顿处输出完整短句，
//!   不做固定时长硬切，避免把句首/句尾的词截断（SenseVoice 每段独立识别，最怕半个词）。
//! - Energy（兜底）：零依赖能量门限 + 自定义 preroll/静音超时切段，仅在 Silero 模型缺失时使用。

use std::collections::VecDeque;
use std::path::Path;

const SAMPLE_RATE: usize = 16_000;

/// 段的切因：Pause=停顿切（句界，可立即定稿）；Max=顶到时长上限强切（半句，需续接）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CutReason {
    Pause,
    Max,
}

/// 按 ASR 引擎区分的切段时长上限（秒）。两个引擎对段长的偏好相反：
/// - SenseVoice：非流式 CTC，段越长越易把连续快语退化成同音乱码 → 短段（实时优先）；
/// - Qwen3-ASR：自回归 LLM 解码器，需完整句子上下文压低幻觉/模板泄漏，可接受延迟 → 长段。
#[derive(Clone, Copy)]
pub struct SegLimits {
    /// Silero 原生分段：单段硬上限
    pub silero_max_s: f32,
    /// Silero 句末停顿阈值：静音达此才断句切段
    pub silero_min_silence_s: f32,
    /// 能量兜底：软上限（超后遇换气短停即收尾）
    pub energy_soft_s: f32,
    /// 能量兜底：硬上限（兜底强切）
    pub energy_hard_s: f32,
}

impl SegLimits {
    /// 由 `config.asr_engine` 选档。未知引擎按 SenseVoice 处理。
    pub fn for_engine(asr_engine: &str) -> Self {
        match asr_engine {
            "qwen3Asr" => Self {
                silero_max_s: 10.0,
                silero_min_silence_s: 0.5,
                energy_soft_s: 6.0,
                energy_hard_s: 10.0,
            },
            _ => Self {
                silero_max_s: 6.0,
                silero_min_silence_s: 0.5,
                energy_soft_s: 4.0,
                energy_hard_s: 7.0,
            },
        }
    }
}

/// 切段后端：Silero 原生分段（准）/ Energy 自定义切段（兜底）。
/// 对外暴露统一的 `push`/`has_open`/`flush`，调用方无需关心用的是哪种。
pub enum Segmenter {
    Silero(SileroCore),
    Energy(EnergyCore),
}

impl Segmenter {
    /// Silero 原生分段。`model` 指向 silero_vad.onnx；失败返回 Err 供调用方回退能量门限。
    pub fn silero(model: &Path, threshold: f32, limits: SegLimits) -> anyhow::Result<Self> {
        Ok(Segmenter::Silero(SileroCore::new(model, threshold, limits)?))
    }

    /// 能量门限兜底切段。
    pub fn energy(threshold: f32, silence_ms: u64, limits: SegLimits) -> Self {
        Segmenter::Energy(EnergyCore::new(
            Box::new(EnergyVad { threshold }),
            silence_ms,
            limits,
        ))
    }

    /// 喂入任意长度 16k 单声道样本，完整切出的语音段追加到 `out`。
    /// 喂入任意长度的 16k 单声道样本，完整切出的语音段（带切因）追加到 `out`。
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<(Vec<f32>, CutReason)>) {
        match self {
            Segmenter::Silero(s) => s.push(samples, out),
            Segmenter::Energy(e) => e.push(samples, out),
        }
    }

    /// 是否有一段语音正在进行（尚未收尾）。
    pub fn has_open(&self) -> bool {
        match self {
            Segmenter::Silero(s) => s.has_open(),
            Segmenter::Energy(e) => e.has_open(),
        }
    }

    /// 强制收尾未完成的段（播放停止时防卡半句）。
    pub fn flush(&mut self, out: &mut Vec<(Vec<f32>, CutReason)>) {
        match self {
            Segmenter::Silero(s) => s.flush(out),
            Segmenter::Energy(e) => e.flush(out),
        }
    }
}

/// Silero 原生分段：把样本喂给 sherpa VAD，它在自然停顿处切好完整段，我们只负责取出队列。
pub struct SileroCore {
    vad: sherpa_onnx::VoiceActivityDetector,
    min_samples: usize,
    // 句首前补（preroll）：sherpa 的 Silero 绑定没暴露 speech_pad_ms，输出段从「概率越过阈值那帧」
    // 开始、不向前补，导致「不/碰/特」这类清辅音起头被切掉首音。这里自己实现 speech_pad_ms：
    // 维护最近输入的环形缓冲 + 累计样本计数，取段时按 seg.start() 把开口前 pad 个样本补回段头。
    ring: VecDeque<f32>, // 最近输入样本环形缓冲（与 vad 同步喂入）
    ring_cap: usize,     // 环形缓冲容量（样本数）
    fed: u64,            // 已喂入 vad 的累计样本数，与 seg.start() 同基准
    pad: usize,          // 句首前补样本数
    last_end: u64,       // 上一段已发出段的绝对结尾下标；补头不得越过它，消 Max 接缝 ~200ms 重复
    // 连续无停顿说话的硬切兜底：open_samples 数当前连续语音时长，超 max_open_samples(= silero_max_s×16k)
    // 就 flush 硬切一段(半句/Max)并紧跟 reset 清 sherpa 内部计数器，防 circular-buffer Invalid n。
    open_samples: usize,
    max_open_samples: usize,
}

impl SileroCore {
    fn new(model: &Path, threshold: f32, limits: SegLimits) -> anyhow::Result<Self> {
        let mut config = sherpa_onnx::VadModelConfig::default();
        config.silero_vad = sherpa_onnx::SileroVadModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            threshold,
            // 句末停顿阈值：连续静音达此才认为一句结束并切段（高级可调，默认 0.5s）。
            // 换气类短停被容忍，不会把一句话切碎或切在词中间。
            min_silence_duration: limits.silero_min_silence_s,
            // 过滤短于 0.25s 的噪点，避免咖哒声起一段空段。
            min_speech_duration: 0.25,
            window_size: 512, // Silero v5 @16k 固定窗
            // 单段硬上限按引擎区分：SenseVoice 短段(6s)防长段退化；Qwen3 长段(10s)给足上下文。
            max_speech_duration: limits.silero_max_s,
        };
        config.sample_rate = 16_000;
        config.num_threads = 1;
        let vad = sherpa_onnx::VoiceActivityDetector::create(&config, 30.0).ok_or_else(|| {
            anyhow::anyhow!("Silero VAD 初始化失败（模型缺失或加载错误）: {}", model.display())
        })?;
        // preroll = 200ms：小于 min_silence_duration(0.5s)，保证补头窗口必落在句末静音里，
        // 既补得回句首、又绝不会够到上一句尾音或复制本段已有样本。
        let pad = SAMPLE_RATE * 200 / 1000;
        // 环形缓冲要够长：取段发生在「段末 + 句末静音」之后，故 seg.start() 距当前已喂入样本可达
        // 「单段最长 + 静音 + pad」。按引擎最长段 + 2s 余量留足，保证 start-pad 仍在缓冲内。
        let ring_cap = (limits.silero_max_s.ceil() as usize + 2) * SAMPLE_RATE;
        Ok(Self {
            vad,
            min_samples: SAMPLE_RATE * 100 / 1000, // 100ms 以下丢弃
            ring: VecDeque::with_capacity(ring_cap + SAMPLE_RATE),
            ring_cap,
            fed: 0,
            pad,
            last_end: 0,
            open_samples: 0,
            max_open_samples: (limits.silero_max_s * SAMPLE_RATE as f32) as usize,
        })
    }

    fn push(&mut self, samples: &[f32], out: &mut Vec<(Vec<f32>, CutReason)>) {
        // sherpa VAD 内部按 window_size 缓冲处理，可直接喂任意长度。
        self.vad.accept_waveform(samples);
        // 同步进环形缓冲，仅保留最近 ring_cap 个样本，供取段时补句首。
        self.ring.extend(samples.iter().copied());
        while self.ring.len() > self.ring_cap {
            self.ring.pop_front();
        }
        self.fed += samples.len() as u64;
        // 自然停顿(min_silence)切出的段 = 句界(Pause)，立即定稿。
        self.drain(out, CutReason::Pause);
        // 连续无停顿：自己数时长，超 silero_max_s 就 flush 硬切一段(半句/Max)——段长与延迟都有上限。
        if self.vad.detected() {
            self.open_samples += samples.len();
            if self.open_samples >= self.max_open_samples {
                self.vad.flush();
                self.drain(out, CutReason::Max);
                // flush 中途打断会腐化 sherpa 的 CircularBuffer 计数器(负数 Invalid n)；紧跟 reset
                // 洗回启动干净态。sherpa 内部下标归零，我们的绝对下标(fed/ring/last_end)同步归零对齐。
                self.vad.reset();
                self.open_samples = 0;
                self.fed = 0;
                self.ring.clear();
                self.last_end = 0;
            }
        } else {
            self.open_samples = 0;
        }
    }

    fn drain(&mut self, out: &mut Vec<(Vec<f32>, CutReason)>, cut: CutReason) {
        while let Some(seg) = self.vad.front() {
            let samples = seg.samples();
            if samples.len() >= self.min_samples {
                let start = seg.start().max(0) as u64;
                // 环形缓冲里最早样本的绝对下标。
                let ring_base = self.fed.saturating_sub(self.ring.len() as u64);
                // 期望补头区间 [pad_start, start)，与段本身 [start, start+n) 相邻不重叠。
                // 但绝不越过上一段已发出的结尾 last_end：连续说话被 Max 强切时两段无静音间隔，
                // start-pad 会落在上一段语音尾，用 last_end 夹住可消接缝重复；有停顿的 Pause 切
                // 因间隔 > pad，last_end 不影响其补头。
                let pad_start = start.saturating_sub(self.pad as u64).max(self.last_end);
                let mut seg_out = Vec::with_capacity(self.pad + samples.len());
                if pad_start < start && pad_start >= ring_base {
                    let from = (pad_start - ring_base) as usize;
                    let to = (start - ring_base) as usize;
                    if to <= self.ring.len() {
                        seg_out.extend(self.ring.iter().copied().skip(from).take(to - from));
                    }
                }
                seg_out.extend_from_slice(samples);
                // 记录本段绝对结尾，供下一段补头夹逼（去接缝重复）。
                self.last_end = start + samples.len() as u64;
                out.push((seg_out, cut));
            }
            drop(seg);
            self.vad.pop();
        }
    }

    fn has_open(&self) -> bool {
        self.vad.detected()
    }

    fn flush(&mut self, out: &mut Vec<(Vec<f32>, CutReason)>) {
        // 强制把内部缓冲里残留的语音收尾成段，再取出。收尾残段当句界（Pause）立即定稿。
        self.vad.flush();
        self.drain(out, CutReason::Pause);
    }
}

/// 帧级语音判定器。各实现规定自己的帧长（样本数）。仅供能量门限兜底使用。
pub trait Vad {
    fn frame_size(&self) -> usize;
    fn is_speech(&mut self, frame: &[f32]) -> bool;
}

/// 能量门限 VAD：零依赖兜底。
pub struct EnergyVad {
    pub threshold: f32,
}

impl Vad for EnergyVad {
    fn frame_size(&self) -> usize {
        320 // 20ms @16k
    }
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        let sum: f32 = frame.iter().map(|s| s * s).sum();
        (sum / frame.len() as f32).sqrt() > self.threshold
    }
}

pub struct EnergyCore {
    vad: Box<dyn Vad>,
    frame: usize,
    // 双阈值断句：区分「换气短停」与「句末长停」，避免把一句话拦腰切两段。
    silence_long: usize,       // 句末长停：连续静音达此阈值必收尾（真正的句子结束）
    silence_short: usize,      // 换气短停：仅当本段已超软上限时，遇此长度静音才收尾（防超长）
    min_speech_samples: usize, // 小于此长度视为噪点丢弃
    soft_max_samples: usize,   // 软上限：超过后允许在短停处收尾，优先在停顿处断句
    max_samples: usize,        // 单段硬上限，控制延迟（兜底强制切）
    preroll_cap: usize,        // 语音起始前置缓冲，避免吞句首

    leftover: Vec<f32>,
    frame_buf: Vec<f32>,
    preroll: VecDeque<f32>,
    cur: Vec<f32>,
    in_speech: bool,
    silence_run: usize,
}

impl EnergyCore {
    fn new(vad: Box<dyn Vad>, silence_ms: u64, limits: SegLimits) -> Self {
        let frame = vad.frame_size();
        // silence_ms 作为句末长停阈值；换气短停固定取其约一半（最少 400ms），仅在超长时生效。
        let silence_long = (silence_ms as usize * SAMPLE_RATE) / 1000;
        let silence_short = (silence_ms.max(800) as usize * SAMPLE_RATE) / 2000; // ≈ silence_ms/2
        Self {
            vad,
            frame,
            silence_long,
            silence_short,
            min_speech_samples: SAMPLE_RATE * 300 / 1000, // 300ms
            // 软/硬上限按 ASR 引擎区分：SenseVoice 4s/7s（实时、防长段退化），Qwen3 6s/10s（给足上下文）。
            // 软上限超过后允许在换气短停处收尾，让多数段在自然停顿断句；硬上限仅兜底防超长。
            soft_max_samples: (SAMPLE_RATE as f32 * limits.energy_soft_s) as usize,
            max_samples: (SAMPLE_RATE as f32 * limits.energy_hard_s) as usize,
            preroll_cap: SAMPLE_RATE * 300 / 1000,        // 300ms
            leftover: Vec::with_capacity(frame * 2),
            frame_buf: Vec::with_capacity(frame),
            preroll: VecDeque::with_capacity(SAMPLE_RATE * 300 / 1000 + frame),
            cur: Vec::new(),
            in_speech: false,
            silence_run: 0,
        }
    }

    /// 喂入任意长度的 16k 单声道样本，完整切出的语音段（带切因）追加到 `out`。
    fn push(&mut self, samples: &[f32], out: &mut Vec<(Vec<f32>, CutReason)>) {
        self.leftover.extend_from_slice(samples);
        let frame = self.frame;
        // 取出 frame_buf 为局部变量，避免「借用 self.leftover」与「&mut self.vad」冲突
        let mut buf = std::mem::take(&mut self.frame_buf);
        let mut i = 0;
        while i + frame <= self.leftover.len() {
            buf.clear();
            buf.extend_from_slice(&self.leftover[i..i + frame]);
            let voiced = self.vad.is_speech(&buf);
            self.handle(voiced, &buf, out);
            i += frame;
        }
        self.frame_buf = buf;
        if i > 0 {
            self.leftover.drain(0..i);
        }
    }

    fn handle(&mut self, voiced: bool, frame: &[f32], out: &mut Vec<(Vec<f32>, CutReason)>) {
        if !self.in_speech {
            for &s in frame {
                self.preroll.push_back(s);
            }
            while self.preroll.len() > self.preroll_cap {
                self.preroll.pop_front();
            }
            if voiced {
                self.in_speech = true;
                self.silence_run = 0;
                // preroll 末尾就是当前帧（上面刚 push 进去），drain 已含之，不能再单独追加当前帧，
                // 否则段首这一帧被写两遍（起音处 20ms 重复波形，害首音节识别）。
                self.cur.extend(self.preroll.drain(..));
            }
        } else {
            self.cur.extend_from_slice(frame);
            if voiced {
                self.silence_run = 0;
            } else {
                self.silence_run += frame.len();
                // 长停 = 句末，正常收尾（换气类短停 < 此阈值，会被容忍，不切）。
                if self.silence_run >= self.silence_long {
                    self.finalize(out, CutReason::Pause);
                    return;
                }
                // 短停 = 可能换气：仅当本段已超软上限时才在此收尾，
                // 避免长句拖到硬上限被拦腰斩断（断点处的词缺右侧上下文，最易识别错）。
                if self.silence_run >= self.silence_short && self.cur.len() >= self.soft_max_samples {
                    self.finalize(out, CutReason::Pause);
                    return;
                }
            }
            if self.cur.len() >= self.max_samples {
                self.finalize(out, CutReason::Max);
            }
        }
    }

    fn finalize(&mut self, out: &mut Vec<(Vec<f32>, CutReason)>, cut: CutReason) {
        let keep = self.cur.len().saturating_sub(self.silence_run);
        if keep >= self.min_speech_samples {
            out.push((self.cur[..keep].to_vec(), cut));
        }
        self.cur.clear();
        self.preroll.clear();
        self.in_speech = false;
        self.silence_run = 0;
    }

    pub fn has_open(&self) -> bool {
        self.in_speech
    }

    pub fn flush(&mut self, out: &mut Vec<(Vec<f32>, CutReason)>) {
        if self.in_speech {
            self.finalize(out, CutReason::Pause);
        }
    }
}
