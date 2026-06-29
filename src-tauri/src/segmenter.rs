//! 语音切段器：把 16kHz 单声道流按「语音/静音」边界切成语音段。
//! 两种后端（`Segmenter` 枚举分发）：
//! - Silero（首选）：直接用 sherpa-onnx 自带 VAD 的**原生分段**，在自然停顿处输出完整短句，
//!   不做固定时长硬切，避免把句首/句尾的词截断（SenseVoice 每段独立识别，最怕半个词）。
//! - Energy（兜底）：零依赖能量门限 + 自定义 preroll/静音超时切段，仅在 Silero 模型缺失时使用。

use std::collections::VecDeque;
use std::path::Path;

const SAMPLE_RATE: usize = 16_000;

/// 按 ASR 引擎区分的切段时长上限（秒）。两个引擎对段长的偏好相反：
/// - SenseVoice：非流式 CTC，段越长越易把连续快语退化成同音乱码 → 短段（实时优先）；
/// - Qwen3-ASR：自回归 LLM 解码器，需完整句子上下文压低幻觉/模板泄漏，可接受延迟 → 长段。
#[derive(Clone, Copy)]
pub struct SegLimits {
    pub silero_max_s: f32,  // Silero 原生分段：单段硬上限
    pub energy_soft_s: f32, // 能量兜底：软上限（超后遇换气短停即收尾）
    pub energy_hard_s: f32, // 能量兜底：硬上限（兜底强切）
}

impl SegLimits {
    /// 由 `config.asr_engine` 选档。未知引擎按 SenseVoice 处理。
    pub fn for_engine(asr_engine: &str) -> Self {
        match asr_engine {
            "qwen3Asr" => Self {
                silero_max_s: 10.0,
                energy_soft_s: 6.0,
                energy_hard_s: 10.0,
            },
            _ => Self {
                silero_max_s: 6.0,
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
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<f32>>) {
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
    pub fn flush(&mut self, out: &mut Vec<Vec<f32>>) {
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
}

impl SileroCore {
    fn new(model: &Path, threshold: f32, limits: SegLimits) -> anyhow::Result<Self> {
        let mut config = sherpa_onnx::VadModelConfig::default();
        config.silero_vad = sherpa_onnx::SileroVadModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            threshold,
            // 句末停顿阈值：连续静音达 0.5s 才认为一句结束并切段。
            // 换气类短停（<0.5s）被容忍，不会把一句话切碎或切在词中间。
            min_silence_duration: 0.5,
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
        Ok(Self {
            vad,
            min_samples: SAMPLE_RATE * 100 / 1000, // 100ms 以下丢弃
        })
    }

    fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<f32>>) {
        // sherpa VAD 内部按 window_size 缓冲处理，可直接喂任意长度。
        self.vad.accept_waveform(samples);
        self.drain(out);
    }

    fn drain(&mut self, out: &mut Vec<Vec<f32>>) {
        while let Some(seg) = self.vad.front() {
            let samples = seg.samples();
            if samples.len() >= self.min_samples {
                out.push(samples.to_vec());
            }
            drop(seg);
            self.vad.pop();
        }
    }

    fn has_open(&self) -> bool {
        self.vad.detected()
    }

    fn flush(&mut self, out: &mut Vec<Vec<f32>>) {
        // 强制把内部缓冲里残留的语音收尾成段，再取出。
        self.vad.flush();
        self.drain(out);
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

    /// 喂入任意长度的 16k 单声道样本，完整切出的语音段追加到 `out`。
    fn push(&mut self, samples: &[f32], out: &mut Vec<Vec<f32>>) {
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

    fn handle(&mut self, voiced: bool, frame: &[f32], out: &mut Vec<Vec<f32>>) {
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
                self.cur.extend(self.preroll.drain(..));
                self.cur.extend_from_slice(frame);
            }
        } else {
            self.cur.extend_from_slice(frame);
            if voiced {
                self.silence_run = 0;
            } else {
                self.silence_run += frame.len();
                // 长停 = 句末，正常收尾（换气类短停 < 此阈值，会被容忍，不切）。
                if self.silence_run >= self.silence_long {
                    self.finalize(out);
                    return;
                }
                // 短停 = 可能换气：仅当本段已超软上限时才在此收尾，
                // 避免长句拖到硬上限被拦腰斩断（断点处的词缺右侧上下文，最易识别错）。
                if self.silence_run >= self.silence_short && self.cur.len() >= self.soft_max_samples {
                    self.finalize(out);
                    return;
                }
            }
            if self.cur.len() >= self.max_samples {
                self.finalize(out);
            }
        }
    }

    fn finalize(&mut self, out: &mut Vec<Vec<f32>>) {
        let keep = self.cur.len().saturating_sub(self.silence_run);
        if keep >= self.min_speech_samples {
            out.push(self.cur[..keep].to_vec());
        }
        self.cur.clear();
        self.preroll.clear();
        self.in_speech = false;
        self.silence_run = 0;
    }

    pub fn has_open(&self) -> bool {
        self.in_speech
    }

    pub fn flush(&mut self, out: &mut Vec<Vec<f32>>) {
        if self.in_speech {
            self.finalize(out);
        }
    }
}
