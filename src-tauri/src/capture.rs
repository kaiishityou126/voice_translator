//! 音频采集（按平台分发）：
//! - Windows：WASAPI 环回(系统音频) / 默认输入(麦克风)
//! - macOS：脚手架——ScreenCaptureKit(系统音频) / cpal(麦克风)，待在 Mac 上完成
//!
//! 平台无关的处理（降噪 → 48k→16k 抽取 → 切段 → 有界发送）抽成 `Processor` 共享，
//! 各平台只负责拿到「48k 单声道 f32」喂进来。

use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError};

use crate::denoise::{Decimator3, Denoiser};
use crate::segmenter::Segmenter;

const TARGET_RATE: usize = 16_000; // 喂给 SenseVoice
const CAPTURE_RATE: usize = 48_000; // 采集 + 降噪工作采样率

/// 平台无关处理：48k 单声道 → [降噪] → /3 抽取 → 切段 → 有界发送。
struct Processor {
    denoise: bool,
    denoiser: Denoiser,
    decimator: Decimator3,
    segmenter: Segmenter,
    clean48: Vec<f32>,
    mono16: Vec<f32>,
    segments: Vec<Vec<f32>>,
    vad_kind: &'static str, // "silero" / "energy"，用于上报实际生效的 VAD
}

impl Processor {
    /// `use_silero` = true 用 Silero VAD（模型缺失或初始化失败则回退能量门限），false 直接用能量门限。
    /// `silero_model` 为 silero_vad.onnx 的解析路径（资源），由调用方按平台解析后传入。
    fn new(
        denoise: bool,
        use_silero: bool,
        silero_model: Option<PathBuf>,
        energy_threshold: f32,
        silence_ms: u64,
    ) -> Self {
        let mut vad_kind = "energy";
        let segmenter = match (use_silero, silero_model) {
            (true, Some(model)) => {
                // Silero 阈值 0.5（标准值）：原生分段下这个阈值对停顿点的判定最稳。
                match Segmenter::silero(&model, 0.5) {
                    Ok(s) => {
                        vad_kind = "silero";
                        s
                    }
                    Err(e) => {
                        eprintln!("[vad] Silero 初始化失败，回退能量门限: {}", e);
                        Segmenter::energy(energy_threshold, silence_ms)
                    }
                }
            }
            _ => Segmenter::energy(energy_threshold, silence_ms),
        };
        Self {
            denoise,
            denoiser: Denoiser::new(),
            decimator: Decimator3::new(),
            segmenter,
            clean48: Vec::with_capacity(CAPTURE_RATE),
            mono16: Vec::with_capacity(TARGET_RATE),
            segments: Vec::new(),
            vad_kind,
        }
    }

    pub(super) fn vad_kind(&self) -> &'static str {
        self.vad_kind
    }

    /// 喂入 48k 单声道；切出的段经 seg_tx 发送。返回 false = 下游已断开，应停止采集。
    fn feed(&mut self, mono48: &[f32], seg_tx: &SyncSender<Vec<f32>>) -> bool {
        self.clean48.clear();
        if self.denoise {
            self.denoiser.process(mono48, &mut self.clean48);
        } else {
            self.clean48.extend_from_slice(mono48);
        }
        self.mono16.clear();
        self.decimator.process(&self.clean48, &mut self.mono16);
        self.segmenter.push(&self.mono16, &mut self.segments);
        self.drain(seg_tx)
    }

    /// 无新数据且有未收尾段时调用：强制收尾（播放停止防卡半句）。
    fn idle_flush(&mut self, seg_tx: &SyncSender<Vec<f32>>) -> bool {
        if self.segmenter.has_open() {
            self.segmenter.flush(&mut self.segments);
        }
        self.drain(seg_tx)
    }

    fn has_open(&self) -> bool {
        self.segmenter.has_open()
    }

    fn drain(&mut self, seg_tx: &SyncSender<Vec<f32>>) -> bool {
        for seg in self.segments.drain(..) {
            match seg_tx.try_send(seg) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    eprintln!("[capture] 处理积压，丢弃一段音频以保持实时");
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        true
    }
}

// ======================== Windows：WASAPI ========================
#[cfg(target_os = "windows")]
mod platform {
    use super::{Processor, CAPTURE_RATE};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::SyncSender;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use tauri::{AppHandle, Emitter, Manager};
    use wasapi::*;

    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

    /// 在当前线程初始化 COM；容忍 RPC_E_CHANGED_MODE（已被初始化为别的套间模式）。
    fn init_com() -> Result<()> {
        let hr = initialize_mta();
        if hr.is_err() && hr.0 != RPC_E_CHANGED_MODE {
            hr.ok()?;
        }
        Ok(())
    }

    pub fn capture_thread(
        source: &str,
        denoise: bool,
        use_silero: bool,
        energy_threshold: f32,
        silence_ms: u64,
        stop: Arc<AtomicBool>,
        seg_tx: SyncSender<Vec<f32>>,
        app: AppHandle,
    ) -> Result<()> {
        init_com()?;
        let enumerator = DeviceEnumerator::new()?;
        // loopback 抓「正在播放的设备」→ Render；麦克风 → Capture
        let device = match source {
            "microphone" => enumerator.get_default_device(&Direction::Capture)?,
            _ => enumerator.get_default_device(&Direction::Render)?,
        };
        let mut audio_client = device.get_iaudioclient()?;

        // 请求 48kHz 双声道 f32；autoconvert 让 WASAPI 重采样到 48k
        let desired_format = WaveFormat::new(32, 32, &SampleType::Float, CAPTURE_RATE, 2, None);
        let blockalign = desired_format.get_blockalign() as usize; // 2ch*4B=8
        let (_def_time, min_time) = audio_client.get_device_period()?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: min_time,
        };
        // 设备是 Render + stream 方向 Capture = loopback
        audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;
        let h_event = audio_client.set_get_eventhandle()?;
        let capture_client = audio_client.get_audiocaptureclient()?;
        audio_client.start_stream()?;

        let mut byte_queue: VecDeque<u8> = VecDeque::with_capacity(blockalign * CAPTURE_RATE);
        // Silero 模型作为应用资源打包；dev/安装态都经资源目录解析。解析不到则回退能量门限。
        let silero_model = if use_silero {
            app.path()
                .resolve(
                    "sidecar/models/silero_vad.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .ok()
                .filter(|p| p.exists())
        } else {
            None
        };
        let mut proc = Processor::new(denoise, use_silero, silero_model, energy_threshold, silence_ms);
        // 上报实际生效的 VAD（release/安装态看不到日志，靠它在 UI 里观测）
        let _ = app.emit("engine-info", serde_json::json!({ "vad": proc.vad_kind() }));
        let mut mono48 = Vec::<f32>::with_capacity(CAPTURE_RATE);
        let mut last_data = Instant::now();
        let idle_flush = Duration::from_millis(silence_ms.max(300));

        while !stop.load(Ordering::Relaxed) {
            let frames = byte_queue.len() / blockalign;
            if frames > 0 {
                last_data = Instant::now();
                mono48.clear();
                for _ in 0..frames {
                    let l = pop_f32(&mut byte_queue);
                    let r = pop_f32(&mut byte_queue);
                    mono48.push((l + r) * 0.5);
                }
                if !proc.feed(&mono48, &seg_tx) {
                    audio_client.stop_stream().ok();
                    return Ok(());
                }
            } else if proc.has_open() && last_data.elapsed() >= idle_flush {
                if !proc.idle_flush(&seg_tx) {
                    audio_client.stop_stream().ok();
                    return Ok(());
                }
                last_data = Instant::now();
            }

            let before = byte_queue.len();
            let info = capture_client.read_from_device_to_deque(&mut byte_queue)?;
            if info.flags.silent {
                // 静音缓冲数据未定义，清零以免污染 VAD/识别
                for b in byte_queue.iter_mut().skip(before) {
                    *b = 0;
                }
            }
            let _ = h_event.wait_for_event(200); // 超时仅为及时响应 stop
        }
        audio_client.stop_stream().ok();
        Ok(())
    }

    #[inline]
    fn pop_f32(q: &mut VecDeque<u8>) -> f32 {
        let b0 = q.pop_front().unwrap_or(0);
        let b1 = q.pop_front().unwrap_or(0);
        let b2 = q.pop_front().unwrap_or(0);
        let b3 = q.pop_front().unwrap_or(0);
        f32::from_le_bytes([b0, b1, b2, b3])
    }

    pub fn probe(source: &str) -> Result<()> {
        init_com()?;
        let enumerator = DeviceEnumerator::new()?;
        let _device = match source {
            "microphone" => enumerator.get_default_device(&Direction::Capture)?,
            _ => enumerator.get_default_device(&Direction::Render)?,
        };
        Ok(())
    }
}

// ======================== macOS：麦克风(cpal) + 系统音频(ScreenCaptureKit) ========================
#[cfg(target_os = "macos")]
mod platform {
    use super::{Processor, CAPTURE_RATE};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, RecvTimeoutError, SyncSender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, bail, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use screencapturekit::prelude::{
        CMSampleBuffer, CMSampleBufferExt, SCStreamOutputTrait, SCStreamOutputType,
    };
    use tauri::{AppHandle, Emitter, Manager};

    struct Resampler {
        ratio: f64,
        pos: f64,
        prev: f32,
        primed: bool,
    }

    impl Resampler {
        fn new(src_rate: u32) -> Self {
            Self {
                ratio: src_rate as f64 / CAPTURE_RATE as f64,
                pos: 0.0,
                prev: 0.0,
                primed: false,
            }
        }

        fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
            for &cur in input {
                if !self.primed {
                    self.prev = cur;
                    self.primed = true;
                }
                while self.pos < 1.0 {
                    out.push(self.prev + (cur - self.prev) * self.pos as f32);
                    self.pos += self.ratio;
                }
                self.pos -= 1.0;
                self.prev = cur;
            }
        }
    }

    fn downmix_send(data: &[f32], channels: usize, tx: &std::sync::mpsc::Sender<Vec<f32>>) {
        let ch = channels.max(1);
        let mut mono = Vec::with_capacity(data.len() / ch + 1);
        for frame in data.chunks(ch) {
            let sum: f32 = frame.iter().copied().sum();
            mono.push(sum / ch as f32);
        }
        let _ = tx.send(mono);
    }

    pub fn capture_thread(
        source: &str,
        denoise: bool,
        use_silero: bool,
        energy_threshold: f32,
        silence_ms: u64,
        stop: Arc<AtomicBool>,
        seg_tx: SyncSender<Vec<f32>>,
        app: AppHandle,
    ) -> Result<()> {
        let silero_model = if use_silero {
            app.path()
                .resolve(
                    "sidecar/models/silero_vad.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .ok()
                .filter(|p| p.exists())
        } else {
            None
        };
        let mut proc =
            Processor::new(denoise, use_silero, silero_model, energy_threshold, silence_ms);
        let _ = app.emit("engine-info", serde_json::json!({ "vad": proc.vad_kind() }));

        match source {
            "microphone" => {
                let host = cpal::default_host();
                let device = host
                    .default_input_device()
                    .ok_or_else(|| anyhow!("未找到麦克风"))?;
                let config: cpal::StreamConfig = device.default_input_config()?.into();
                let src_rate = config.sample_rate.0;
                let channels = config.channels as usize;

                let (raw_tx, raw_rx) = channel::<Vec<f32>>();
                let err_fn = |e| eprintln!("[capture] cpal 输入流错误: {}", e);

                let stream = match config.channels {
                    1 => {
                        let tx = raw_tx.clone();
                        device.build_input_stream(
                            &config,
                            move |data: &[f32], _| {
                                let _ = tx.send(data.to_vec());
                            },
                            err_fn,
                            None,
                        )?
                    }
                    _ => {
                        let tx = raw_tx.clone();
                        device.build_input_stream(
                            &config,
                            move |data: &[f32], _| {
                                downmix_send(data, channels, &tx);
                            },
                            err_fn,
                            None,
                        )?
                    }
                };
                drop(raw_tx);
                stream.play()?;

                let mut resampler = Resampler::new(src_rate);
                let mut mono48 = Vec::<f32>::with_capacity(CAPTURE_RATE);
                let idle_flush = Duration::from_millis(silence_ms.max(300));
                let mut last_data = Instant::now();

                while !stop.load(Ordering::Relaxed) {
                    match raw_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(chunk) => {
                            last_data = Instant::now();
                            mono48.clear();
                            if src_rate == CAPTURE_RATE as u32 {
                                mono48.extend_from_slice(&chunk);
                            } else {
                                resampler.process(&chunk, &mut mono48);
                            }
                            if !proc.feed(&mono48, &seg_tx) {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if proc.has_open() && last_data.elapsed() >= idle_flush {
                                if !proc.idle_flush(&seg_tx) {
                                    break;
                                }
                                last_data = Instant::now();
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                Ok(())
            }
            "loopback" => {
                // 系统音频采集：ScreenCaptureKit（macOS 13.0+，需「屏幕录制」授权）。
                // 仅取音频，video 配置给最小尺寸以降低开销。
                use screencapturekit::prelude::*;

                let content = SCShareableContent::get().map_err(|e| {
                    anyhow!("无法获取可共享内容（可能未授权屏幕录制）: {:?}", e)
                })?;
                let display = content
                    .displays()
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("未找到显示器"))?;

                let filter = SCContentFilter::create()
                    .with_display(&display)
                    .with_excluding_windows(&[])
                    .build();

                // captures_audio=true 开系统音频；单声道 48k 与下游处理对齐。
                let config = SCStreamConfiguration::new()
                    .with_width(2)
                    .with_height(2)
                    .with_captures_audio(true)
                    .with_sample_rate(48000)
                    .with_channel_count(1);

                let (raw_tx, raw_rx) = channel::<Vec<f32>>();

                let mut stream = SCStream::new(&filter, &config);
                stream.add_output_handler(
                    AudioHandler {
                        tx: std::sync::Mutex::new(raw_tx),
                    },
                    SCStreamOutputType::Audio,
                );

                stream
                    .start_capture()
                    .map_err(|e| anyhow!("启动系统音频流失败: {:?}", e))?;

                let mut mono48 = Vec::<f32>::with_capacity(CAPTURE_RATE);
                let idle_flush = Duration::from_millis(silence_ms.max(300));
                let mut last_data = Instant::now();

                while !stop.load(Ordering::Relaxed) {
                    match raw_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(chunk) => {
                            last_data = Instant::now();
                            // SCK 已按请求输出 48k 单声道，直接透传。
                            mono48.clear();
                            mono48.extend_from_slice(&chunk);
                            if !proc.feed(&mono48, &seg_tx) {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if proc.has_open() && last_data.elapsed() >= idle_flush {
                                if !proc.idle_flush(&seg_tx) {
                                    break;
                                }
                                last_data = Instant::now();
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                stream.stop_capture().ok();
                Ok(())
            }
            _ => bail!("不支持的来源: {}", source),
        }
    }

    /// 系统音频回调：SCStream 要求 handler 为 Send + Sync。
    /// mpsc::Sender 非 Sync，用 Mutex 包一层。
    struct AudioHandler {
        tx: std::sync::Mutex<std::sync::mpsc::Sender<Vec<f32>>>,
    }

    impl SCStreamOutputTrait for AudioHandler {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
            if matches!(of_type, SCStreamOutputType::Audio) {
                let pcm = extract_audio_samples(&sample);
                if !pcm.is_empty() {
                    if let Ok(tx) = self.tx.lock() {
                        let _ = tx.send(pcm);
                    }
                }
            }
        }
    }

    /// 从音频 CMSampleBuffer 取出单声道 f32 PCM。
    /// 按 with_channel_count(1) 请求，取第一个 buffer 即单声道连续 f32（小端字节）。
    fn extract_audio_samples(sample: &CMSampleBuffer) -> Vec<f32> {
        let Some(list) = sample.audio_buffer_list() else {
            return Vec::new();
        };
        let Some(buf) = list.buffer(0) else {
            return Vec::new();
        };
        let bytes = buf.data();
        let n = bytes.len() / 4; // f32 = 4 字节
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let off = i * 4;
            out.push(f32::from_le_bytes([
                bytes[off],
                bytes[off + 1],
                bytes[off + 2],
                bytes[off + 3],
            ]));
        }
        out
    }

    pub fn probe(source: &str) -> Result<()> {
        match source {
            "microphone" => {
                let host = cpal::default_host();
                host.default_input_device()
                    .ok_or_else(|| anyhow!("未找到麦克风"))?;
                Ok(())
            }
            "loopback" => {
                // 首次调用会让系统在「隐私与安全性 → 屏幕录制」登记本应用并触发授权。
                use screencapturekit::prelude::SCShareableContent;
                if SCShareableContent::get().is_ok() {
                    Ok(())
                } else {
                    bail!("系统音频采集不可用：请在「系统设置 → 隐私与安全性 → 屏幕录制」中授权本应用，然后重启应用")
                }
            }
            _ => bail!("不支持的来源: {}", source),
        }
    }
}

// ======================== Linux：麦克风(cpal) + 系统音频(PipeWire/PulseAudio monitor) ========================
#[cfg(target_os = "linux")]
mod platform {
    use super::{Processor, CAPTURE_RATE};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, RecvTimeoutError, SyncSender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, bail, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use tauri::{AppHandle, Emitter, Manager};

    struct Resampler {
        ratio: f64,
        pos: f64,
        prev: f32,
        primed: bool,
    }

    impl Resampler {
        fn new(src_rate: u32) -> Self {
            Self {
                ratio: src_rate as f64 / CAPTURE_RATE as f64,
                pos: 0.0,
                prev: 0.0,
                primed: false,
            }
        }

        fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
            for &cur in input {
                if !self.primed {
                    self.prev = cur;
                    self.primed = true;
                }
                while self.pos < 1.0 {
                    out.push(self.prev + (cur - self.prev) * self.pos as f32);
                    self.pos += self.ratio;
                }
                self.pos -= 1.0;
                self.prev = cur;
            }
        }
    }

    fn downmix_send(data: &[f32], channels: usize, tx: &std::sync::mpsc::Sender<Vec<f32>>) {
        let ch = channels.max(1);
        let mut mono = Vec::with_capacity(data.len() / ch + 1);
        for frame in data.chunks(ch) {
            let sum: f32 = frame.iter().copied().sum();
            mono.push(sum / ch as f32);
        }
        let _ = tx.send(mono);
    }

    pub fn capture_thread(
        source: &str,
        denoise: bool,
        use_silero: bool,
        energy_threshold: f32,
        silence_ms: u64,
        stop: Arc<AtomicBool>,
        seg_tx: SyncSender<Vec<f32>>,
        app: AppHandle,
    ) -> Result<()> {
        let silero_model = if use_silero {
            app.path()
                .resolve(
                    "sidecar/models/silero_vad.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .ok()
                .filter(|p| p.exists())
        } else {
            None
        };
        let mut proc =
            Processor::new(denoise, use_silero, silero_model, energy_threshold, silence_ms);
        let _ = app.emit("engine-info", serde_json::json!({ "vad": proc.vad_kind() }));

        let host = cpal::default_host();
        let device = match source {
            "microphone" => {
                host.default_input_device()
                    .ok_or_else(|| anyhow!("未找到麦克风"))?
            }
            "loopback" => {
                // 系统音频 = 默认输出(sink)的 monitor 源。优先精确匹配
                // `<默认输出设备名>.monitor`，匹配不到再退回「任意含 monitor 的输入」。
                let devices: Vec<_> = host
                    .input_devices()
                    .map_err(|e| anyhow!("枚举设备失败: {}", e))?
                    .collect();
                let preferred = host
                    .default_output_device()
                    .and_then(|d| d.name().ok())
                    .map(|n| format!("{}.monitor", n));
                let pick = preferred
                    .as_ref()
                    .and_then(|want| {
                        devices
                            .iter()
                            .find(|d| d.name().map(|n| &n == want).unwrap_or(false))
                            .cloned()
                    })
                    .or_else(|| {
                        devices
                            .iter()
                            .find(|d| {
                                d.name()
                                    .map(|n| n.to_lowercase().contains("monitor"))
                                    .unwrap_or(false)
                            })
                            .cloned()
                    });
                pick.ok_or_else(|| {
                    let names: Vec<String> =
                        devices.iter().filter_map(|d| d.name().ok()).collect();
                    anyhow!(
                        "未找到系统音频(monitor)源。可用输入设备: [{}]。\
                         请确保 PulseAudio/PipeWire 正在运行，并已启用对应输出的 monitor。",
                        names.join(", ")
                    )
                })?
            }
            _ => bail!("不支持的来源: {}", source),
        };

        let supported = device.default_input_config()?;
        let sample_format = supported.sample_format();
        let src_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let config: cpal::StreamConfig = supported.into();

        let (raw_tx, raw_rx) = channel::<Vec<f32>>();
        let err_fn = |e| eprintln!("[capture] cpal 输入流错误: {}", e);

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let tx = raw_tx.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _| {
                        downmix_send(data, channels, &tx);
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let tx = raw_tx.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        let f: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                        downmix_send(&f, channels, &tx);
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let tx = raw_tx.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _| {
                        let f: Vec<f32> = data
                            .iter()
                            .map(|&s| (s as f32 - 32768.0) / 32768.0)
                            .collect();
                        downmix_send(&f, channels, &tx);
                    },
                    err_fn,
                    None,
                )?
            }
            other => bail!("不支持的采样格式: {:?}", other),
        };
        drop(raw_tx);
        stream.play()?;

        let mut resampler = Resampler::new(src_rate);
        let mut mono48 = Vec::<f32>::with_capacity(CAPTURE_RATE);
        let idle_flush = Duration::from_millis(silence_ms.max(300));
        let mut last_data = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            match raw_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => {
                    last_data = Instant::now();
                    mono48.clear();
                    if src_rate == CAPTURE_RATE as u32 {
                        mono48.extend_from_slice(&chunk);
                    } else {
                        resampler.process(&chunk, &mut mono48);
                    }
                    if !proc.feed(&mono48, &seg_tx) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if proc.has_open() && last_data.elapsed() >= idle_flush {
                        if !proc.idle_flush(&seg_tx) {
                            break;
                        }
                        last_data = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    }

    pub fn probe(source: &str) -> Result<()> {
        let host = cpal::default_host();
        match source {
            "microphone" => {
                host.default_input_device()
                    .ok_or_else(|| anyhow!("未找到麦克风"))?;
                Ok(())
            }
            "loopback" => {
                let devices: Vec<_> = host
                    .input_devices()
                    .map_err(|e| anyhow!("枚举设备失败: {}", e))?
                    .collect();
                let preferred = host
                    .default_output_device()
                    .and_then(|d| d.name().ok())
                    .map(|n| format!("{}.monitor", n));
                let found = preferred
                    .as_ref()
                    .map(|want| {
                        devices
                            .iter()
                            .any(|d| d.name().map(|n| &n == want).unwrap_or(false))
                    })
                    .unwrap_or(false)
                    || devices.iter().any(|d| {
                        d.name()
                            .map(|n| n.to_lowercase().contains("monitor"))
                            .unwrap_or(false)
                    });
                if found {
                    Ok(())
                } else {
                    let names: Vec<String> =
                        devices.iter().filter_map(|d| d.name().ok()).collect();
                    bail!(
                        "未找到系统音频(monitor)源。可用输入设备: [{}]。\
                         请确保 PulseAudio/PipeWire 正在运行，并已启用对应输出的 monitor。",
                        names.join(", ")
                    )
                }
            }
            _ => bail!("不支持的来源: {}", source),
        }
    }
}

// ======================== 其它平台：不支持采集（保证可编译）========================
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod platform {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::SyncSender;
    use std::sync::Arc;

    use anyhow::{bail, Result};

    pub fn capture_thread(
        source: &str,
        _denoise: bool,
        _use_silero: bool,
        _energy_threshold: f32,
        _silence_ms: u64,
        _stop: Arc<AtomicBool>,
        _seg_tx: SyncSender<Vec<f32>>,
        _app: tauri::AppHandle,
    ) -> Result<()> {
        let _ = source;
        bail!("当前平台不支持音频采集");
    }

    pub fn probe(_source: &str) -> Result<()> {
        bail!("当前平台不支持音频采集");
    }
}

pub use platform::{capture_thread, probe};
