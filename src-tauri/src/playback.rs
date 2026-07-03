//! TTS 音频回灌：把 Kokoro 合成的 24kHz 单声道 PCM render 到指定输出设备（如 VB-Cable 的 CABLE Input）。
//! 与采集对称（WASAPI 共享事件模式）：请求 48k 双声道 f32 + autoconvert，把 24k→48k 线性重采样、单声道复制成双声道写入。
//! 一句没播完不开下一句（调用方串行）→ 不重叠。设备按 friendly name 匹配，缺省用默认输出。

const KOKORO_RATE: usize = 24_000;

#[cfg(target_os = "windows")]
pub fn list_output_devices() -> Vec<String> {
    platform::list_output_devices().unwrap_or_default()
}

#[cfg(target_os = "macos")]
pub fn list_output_devices() -> Vec<String> {
    platform::list_output_devices().unwrap_or_default()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn list_output_devices() -> Vec<String> {
    Vec::new()
}

/// 自动找虚拟声卡的输入端(Windows: 名含 "cable input"；macOS: 名含 "blackhole")，找不到返回 None。
pub fn find_cable_input() -> Option<String> {
    // 各平台虚拟声卡命名不同：VB-Cable(Windows) / BlackHole(macOS)
    #[cfg(target_os = "macos")]
    let needle = "blackhole";
    #[cfg(not(target_os = "macos"))]
    let needle = "cable input";
    list_output_devices()
        .into_iter()
        .find(|n| n.to_lowercase().contains(needle))
}

/// 是否装了虚拟声卡（供前端判断是否提示装 VB-Cable）。
pub fn cable_present() -> bool {
    find_cable_input().is_some()
}

#[cfg(target_os = "windows")]
pub use platform::Player;

#[cfg(target_os = "macos")]
pub use platform::Player;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub struct Player;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
impl Player {
    pub fn open(_device: &str) -> anyhow::Result<Self> {
        anyhow::bail!("当前平台未实现 TTS 回灌播放")
    }
    pub fn play(&mut self, _samples24k: &[f32]) {}
    pub fn play_48k(&mut self, _mono48: &[f32]) {}
}

#[cfg(target_os = "windows")]
mod platform {
    use super::KOKORO_RATE;
    use std::collections::VecDeque;

    use anyhow::Result;
    use wasapi::*;

    const DEVICE_RATE: usize = 48_000;
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

    fn init_com() -> Result<()> {
        let hr = initialize_mta();
        if hr.is_err() && hr.0 != RPC_E_CHANGED_MODE {
            hr.ok()?;
        }
        Ok(())
    }

    /// 枚举所有活动的输出（Render）设备友好名，供前端下拉选 CABLE Input。
    pub fn list_output_devices() -> Result<Vec<String>> {
        init_com()?;
        let enumerator = DeviceEnumerator::new()?;
        let coll = enumerator.get_device_collection(&Direction::Render)?;
        let n = coll.get_nbr_devices()?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            if let Ok(d) = coll.get_device_at_index(i) {
                if let Ok(name) = d.get_friendlyname() {
                    out.push(name);
                }
            }
        }
        Ok(out)
    }

    /// 持久打开一个输出设备，串行写入 TTS 音频。空 device 用默认输出。
    pub struct Player {
        audio_client: AudioClient,
        render: AudioRenderClient,
        h_event: Handle,
        buffer_frames: usize,
        // 24k→48k 线性重采样状态
        prev: f32,
        pos: f64,
        primed: bool,
    }

    impl Player {
        pub fn open(device_name: &str) -> Result<Self> {
            init_com()?;
            let enumerator = DeviceEnumerator::new()?;
            let device = if device_name.is_empty() {
                enumerator.get_default_device(&Direction::Render)?
            } else {
                let coll = enumerator.get_device_collection(&Direction::Render)?;
                coll.get_device_with_name(device_name)
                    .or_else(|_| enumerator.get_default_device(&Direction::Render))?
            };
            let mut audio_client = device.get_iaudioclient()?;
            let format = WaveFormat::new(32, 32, &SampleType::Float, DEVICE_RATE, 2, None);
            let (_def, min_time) = audio_client.get_device_period()?;
            let mode = StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: min_time,
            };
            audio_client.initialize_client(&format, &Direction::Render, &mode)?;
            let h_event = audio_client.set_get_eventhandle()?;
            let render = audio_client.get_audiorenderclient()?;
            let buffer_frames = audio_client.get_buffer_size()? as usize;
            audio_client.start_stream()?;
            Ok(Self {
                audio_client,
                render,
                h_event,
                buffer_frames,
                prev: 0.0,
                pos: 0.0,
                primed: false,
            })
        }

        /// 阻塞播放一整段 24k 单声道（TTS）：24k→48k 升采样后写设备（调用方串行 → 不重叠）。
        pub fn play(&mut self, samples24k: &[f32]) {
            if samples24k.is_empty() {
                return;
            }
            // 重采样到 48k 单声道
            let mut mono48: Vec<f32> = Vec::with_capacity(samples24k.len() * 2 + 4);
            let ratio = KOKORO_RATE as f64 / DEVICE_RATE as f64; // <1，升采样
            for &cur in samples24k {
                if !self.primed {
                    self.prev = cur;
                    self.primed = true;
                }
                while self.pos < 1.0 {
                    mono48.push(self.prev + (cur - self.prev) * self.pos as f32);
                    self.pos += ratio;
                }
                self.pos -= 1.0;
                self.prev = cur;
            }
            self.write_mono48(&mono48);
        }

        /// 播放一段已是 48k 单声道的 PCM（原声直通）：不重采样，直接单声道→双声道写设备。
        pub fn play_48k(&mut self, mono48: &[f32]) {
            if !mono48.is_empty() {
                self.write_mono48(mono48);
            }
        }

        /// 单声道 48k → 双声道交错 → 阻塞写满设备缓冲。
        fn write_mono48(&mut self, mono48: &[f32]) {
            let mut bytes: VecDeque<u8> = VecDeque::with_capacity(mono48.len() * 8);
            for &s in mono48 {
                let b = s.to_le_bytes();
                bytes.extend(b);
                bytes.extend(b);
            }
            while bytes.len() >= 8 {
                let space = self
                    .audio_client
                    .get_available_space_in_frames()
                    .unwrap_or(self.buffer_frames as u32) as usize;
                let want = (bytes.len() / 8).min(space);
                if want > 0 {
                    let _ = self.render.write_to_device_from_deque(want, &mut bytes, None);
                }
                let _ = self.h_event.wait_for_event(200);
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            let _ = self.audio_client.stop_stream();
        }
    }
}

// ======================== macOS：cpal 输出（回灌到 BlackHole 等虚拟声卡）========================
// 与 Windows 对称：请求设备默认输出配置（CoreAudio 原生 f32），把 24k/48k 单声道重采样到
// 设备采样率、复制成设备声道数，经共享环形缓冲喂给 cpal 回调。
// cpal 无 WASAPI 的 autoconvert，故采样率转换由本模块自己做（线性插值，与采集侧一致）。
// play/play_48k 阻塞到缓冲抽干才返回 → 保持“一句没播完不开下一句”的串行契约（调用方串行）。
#[cfg(target_os = "macos")]
mod platform {
    use super::KOKORO_RATE;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::{anyhow, bail, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// 枚举所有输出设备友好名，供前端下拉选 BlackHole。
    pub fn list_output_devices() -> Result<Vec<String>> {
        let host = cpal::default_host();
        let mut out = Vec::new();
        for d in host.output_devices()? {
            if let Ok(name) = d.name() {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// 持久打开一个输出设备，串行写入 TTS/原声音频。空 device 用默认输出。
    /// cpal 回调仅从 `buf` 弹出交错样本；重采样与交错在 play/play_48k 里完成（回调保持轻量）。
    pub struct Player {
        _stream: cpal::Stream, // 保活；drop 即停流
        buf: Arc<Mutex<VecDeque<f32>>>,
        device_rate: u32,
        channels: usize,
        // 线性重采样状态（按输入采样率惰性初始化；换率则重置）
        rs_src_rate: u32,
        rs_ratio: f64,
        rs_pos: f64,
        rs_prev: f32,
        rs_primed: bool,
    }

    impl Player {
        pub fn open(device_name: &str) -> Result<Self> {
            let host = cpal::default_host();
            let device = if device_name.is_empty() {
                host.default_output_device()
                    .ok_or_else(|| anyhow!("未找到默认输出设备"))?
            } else {
                let mut found = None;
                for d in host.output_devices()? {
                    if d.name().map(|n| n == device_name).unwrap_or(false) {
                        found = Some(d);
                        break;
                    }
                }
                match found {
                    Some(d) => d,
                    None => host
                        .default_output_device()
                        .ok_or_else(|| anyhow!("未找到输出设备: {device_name}"))?,
                }
            };

            let supported = device.default_output_config()?;
            // CoreAudio 输出原生 f32；非 f32 设备极少见，直接给出清晰报错。
            if supported.sample_format() != cpal::SampleFormat::F32 {
                bail!("输出设备采样格式非 f32（{:?}），暂不支持", supported.sample_format());
            }
            let config: cpal::StreamConfig = supported.config();
            let device_rate = config.sample_rate.0;
            let channels = config.channels as usize;

            let buf: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
            let cb_buf = buf.clone();
            let err_fn = |e| eprintln!("[playback] cpal 输出流错误: {}", e);
            let stream = device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut q = cb_buf.lock().unwrap();
                    for slot in data.iter_mut() {
                        *slot = q.pop_front().unwrap_or(0.0); // 欠载填静音
                    }
                },
                err_fn,
                None,
            )?;
            stream.play()?;

            Ok(Self {
                _stream: stream,
                buf,
                device_rate,
                channels,
                rs_src_rate: 0,
                rs_ratio: 0.0,
                rs_pos: 0.0,
                rs_prev: 0.0,
                rs_primed: false,
            })
        }

        /// 阻塞播放一整段 24k 单声道（TTS）：重采样到设备率 + 交错到设备声道 → 写入 → 等抽干。
        pub fn play(&mut self, samples24k: &[f32]) {
            if samples24k.is_empty() {
                return;
            }
            self.enqueue(samples24k, KOKORO_RATE as u32);
            self.drain();
        }

        /// 播放一段已是 48k 单声道的 PCM（原声直通）：同上，输入率按 48k。
        pub fn play_48k(&mut self, mono48: &[f32]) {
            if mono48.is_empty() {
                return;
            }
            self.enqueue(mono48, 48_000);
            self.drain();
        }

        /// 单声道输入 → 重采样到设备率 → 交错成设备声道 → 压入共享缓冲。
        fn enqueue(&mut self, input: &[f32], src_rate: u32) {
            // 输入采样率变化时重置重采样状态（同一 Player 生命周期内通常只用一种率）。
            if self.rs_src_rate != src_rate {
                self.rs_src_rate = src_rate;
                self.rs_ratio = src_rate as f64 / self.device_rate as f64;
                self.rs_pos = 0.0;
                self.rs_prev = 0.0;
                self.rs_primed = false;
            }
            let mut mono_dev: Vec<f32> = Vec::with_capacity(
                (input.len() as f64 / self.rs_ratio).ceil() as usize + 4,
            );
            if src_rate == self.device_rate {
                mono_dev.extend_from_slice(input);
            } else {
                for &cur in input {
                    if !self.rs_primed {
                        self.rs_prev = cur;
                        self.rs_primed = true;
                    }
                    while self.rs_pos < 1.0 {
                        mono_dev.push(self.rs_prev + (cur - self.rs_prev) * self.rs_pos as f32);
                        self.rs_pos += self.rs_ratio;
                    }
                    self.rs_pos -= 1.0;
                    self.rs_prev = cur;
                }
            }
            let mut q = self.buf.lock().unwrap();
            for &s in &mono_dev {
                for _ in 0..self.channels {
                    q.push_back(s);
                }
            }
        }

        /// 阻塞到共享缓冲被回调抽干（近空）→ 保证串行不重叠。
        fn drain(&self) {
            loop {
                {
                    let q = self.buf.lock().unwrap();
                    if q.len() < self.channels {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}
