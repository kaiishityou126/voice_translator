//! 本地 TTS：sherpa-onnx Kokoro v1.0 多语，进程内离线合成（与 ASR 复用同一份静态 sherpa，不引第二份 onnxruntime）。
//! 译文文本 → 24kHz 单声道 f32；按 sid 选音色（speaker），length_scale 控语速。
//! 模型 ~310MB 按需下载到数据目录，沿用 asr.rs 的 tar.bz2 + 系统 tar 解压机制，不打进安装包。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};
use tauri::{AppHandle, Emitter, Manager};

/// Kokoro v1.0 多语：解压后的目录名 / GitHub releases 单一 tar.bz2（约 310MB）。
const KOKORO_DIR: &str = "kokoro-multi-lang-v1_0";
const KOKORO_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2";

/// 模型存放目录：app 数据目录/models（与 ASR 共用）。
fn models_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| anyhow!("无法解析数据目录: {e}"))?
        .join("models");
    std::fs::create_dir_all(&dir).ok();
    Ok(dir)
}

fn kokoro_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(models_dir(app)?.join(KOKORO_DIR))
}

/// Kokoro 关键组件是否就绪（模型 + 音色 + 词表 + espeak 数据）。
pub fn model_present(app: &AppHandle) -> bool {
    let Ok(d) = kokoro_dir(app) else {
        return false;
    };
    d.join("model.onnx").exists()
        && d.join("voices.bin").exists()
        && d.join("tokens.txt").exists()
        && d.join("espeak-ng-data").is_dir()
}

/// 下载 Kokoro（单一 tar.bz2，约 310MB）并用系统 tar 解压。进度走 "tts-progress" 事件。
pub fn download(app: &AppHandle) -> Result<()> {
    use std::io::{Read, Write};
    let dir = models_dir(app)?;
    let tarball = dir.join("kokoro.tar.bz2");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;
    let mut resp = client.get(KOKORO_URL).send()?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let tmp = tarball.with_extension("part");
    let mut out = std::fs::File::create(&tmp)?;
    let mut downloaded: u64 = 0;
    let mut last_tick: u64 = u64::MAX;
    let mut buf = [0u8; 65536];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        downloaded += n as u64;
        let pct = if total > 0 { downloaded * 100 / total } else { 0 };
        let tick = if total > 0 { pct } else { downloaded / (4 * 1024 * 1024) };
        if tick != last_tick {
            last_tick = tick;
            let _ = app.emit(
                "tts-progress",
                serde_json::json!({ "downloaded": downloaded, "total": total, "pct": pct }),
            );
        }
    }
    drop(out);
    std::fs::rename(&tmp, &tarball)?;

    let _ = app.emit(
        "tts-progress",
        serde_json::json!({ "downloaded": 0, "total": 0, "pct": 100, "stage": "extract" }),
    );
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tarball)
        .arg("-C")
        .arg(&dir)
        .status()
        .map_err(|e| anyhow!("调用系统 tar 解压失败: {e}（需 Windows 10+ 自带 tar）"))?;
    if !status.success() {
        return Err(anyhow!("tar 解压 Kokoro 模型失败（压缩包损坏？）"));
    }
    let _ = std::fs::remove_file(&tarball);
    if !model_present(app) {
        return Err(anyhow!("Kokoro 模型解压后文件不完整"));
    }
    Ok(())
}

/// 进程内 Kokoro 合成器（多 worker 串行播放，单实例只读共享）。
pub struct Tts {
    engine: OfflineTts,
    speakers: i32,
}

unsafe impl Send for Tts {}
unsafe impl Sync for Tts {}

impl Tts {
    /// 加载 Kokoro v1.0 多语，`lang` 给 espeak G2P 语种提示（如 "zh"/"en"/"ja"）。
    /// `num_threads` 由上层按核数分配（回灌时与出站 ASR 一起压缩防超订）。
    pub fn load(app: &AppHandle, lang: &str, num_threads: i32) -> Result<Arc<Self>> {
        let d = kokoro_dir(app)?;
        let lexicon = [d.join("lexicon-us-en.txt"), d.join("lexicon-zh.txt")]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(",");
        let kokoro = OfflineTtsKokoroModelConfig {
            model: Some(d.join("model.onnx").to_string_lossy().into_owned()),
            voices: Some(d.join("voices.bin").to_string_lossy().into_owned()),
            tokens: Some(d.join("tokens.txt").to_string_lossy().into_owned()),
            data_dir: Some(d.join("espeak-ng-data").to_string_lossy().into_owned()),
            dict_dir: Some(d.join("dict").to_string_lossy().into_owned()),
            lexicon: Some(lexicon),
            lang: Some(lang.to_string()),
            length_scale: 1.0,
        };
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro,
                num_threads,
                ..Default::default()
            },
            max_num_sentences: 1,
            ..Default::default()
        };
        let engine = OfflineTts::create(&config)
            .ok_or_else(|| anyhow!("创建 Kokoro TTS 失败（模型文件损坏？）"))?;
        let speakers = engine.num_speakers();
        Ok(Arc::new(Self { engine, speakers }))
    }

    /// 合成一段文本为 24kHz 单声道 f32。sid 越界时夹到合法范围。
    pub fn synth(&self, text: &str, sid: i32, speed: f32) -> Vec<f32> {
        let sid = sid.clamp(0, (self.speakers - 1).max(0));
        let cfg = GenerationConfig {
            sid,
            speed: if speed > 0.0 { speed } else { 1.0 },
            ..Default::default()
        };
        self.engine
            .generate_with_config(text, &cfg, None::<fn(&[f32], f32) -> bool>)
            .map(|a| a.samples().to_vec())
            .unwrap_or_default()
    }
}
