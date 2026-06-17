//! 降噪 + 重采样：
//! - Denoiser：nnnoiseless(RNNoise) 在 48kHz、480 样本/帧上做语音降噪
//! - Decimator3：48kHz → 16kHz 的 /3 抽取（带窗 sinc 低通抗混叠），喂给 SenseVoice
//! RNNoise 要求样本在 i16 数值范围（用 f32 存），所以进出各做一次 ±32768 缩放。

use nnnoiseless::DenoiseState;

const FRAME: usize = DenoiseState::FRAME_SIZE; // 480
const SCALE: f32 = 32768.0;

pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    inbuf: Vec<f32>, // 累积的 48k 样本（已缩放到 i16 范围）
    warmed: bool,    // 首帧输出按文档丢弃
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            inbuf: Vec::with_capacity(FRAME * 4),
            warmed: false,
        }
    }

    /// 输入/输出均为 48k 单声道 [-1,1]；按整帧产出去噪结果。
    pub fn process(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        for &s in samples {
            self.inbuf.push(s * SCALE);
        }
        let mut frame_out = [0f32; FRAME];
        let mut i = 0;
        while i + FRAME <= self.inbuf.len() {
            self.state.process_frame(&mut frame_out, &self.inbuf[i..i + FRAME]);
            if self.warmed {
                for &v in frame_out.iter() {
                    out.push(v / SCALE);
                }
            } else {
                self.warmed = true; // 丢弃首帧（warmup）
            }
            i += FRAME;
        }
        if i > 0 {
            self.inbuf.drain(0..i);
        }
    }
}

const NTAPS: usize = 33;

/// 48kHz → 16kHz 整数 /3 抽取器：每 3 个输入样本经低通 FIR 产 1 个输出样本。
pub struct Decimator3 {
    coefs: [f32; NTAPS],
    hist: [f32; NTAPS], // 环形历史
    head: usize,
    counter: usize, // 输入样本计数，用于每 3 取 1
}

impl Decimator3 {
    pub fn new() -> Self {
        // 带 Hamming 窗的 sinc 低通，截止 ~7.5kHz @48k（16k 的奈奎斯特 8k 以下）
        let fc = 7500.0_f32 / 48000.0;
        let m = (NTAPS - 1) as f32;
        let pi = std::f32::consts::PI;
        let mut coefs = [0f32; NTAPS];
        let mut sum = 0.0;
        for n in 0..NTAPS {
            let x = n as f32 - m / 2.0;
            let sinc = if x.abs() < 1e-6 {
                2.0 * fc
            } else {
                (2.0 * pi * fc * x).sin() / (pi * x)
            };
            let w = 0.54 - 0.46 * (2.0 * pi * n as f32 / m).cos(); // Hamming
            coefs[n] = sinc * w;
            sum += coefs[n];
        }
        for c in coefs.iter_mut() {
            *c /= sum; // 归一化，直流增益 = 1
        }
        Self {
            coefs,
            hist: [0f32; NTAPS],
            head: 0,
            counter: 0,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &s in input {
            self.hist[self.head] = s;
            self.head = (self.head + 1) % NTAPS;
            self.counter += 1;
            if self.counter == 3 {
                self.counter = 0;
                // 对称系数，卷积方向无所谓：从最旧样本(head 处)开始累加
                let mut acc = 0.0;
                let mut idx = self.head;
                for k in 0..NTAPS {
                    acc += self.coefs[k] * self.hist[idx];
                    idx = (idx + 1) % NTAPS;
                }
                out.push(acc);
            }
        }
    }
}
