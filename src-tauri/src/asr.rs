//! 本地 ASR：sherpa-onnx + SenseVoice，进程内离线识别（无 sidecar / HTTP）。
//! 采集到的 16k 单声道 f32 直接喂给识别器；标点与 ITN（数字规整）在模型内完成。
//! sherpa-onnx 静态链接，无运行期 DLL；OfflineRecognizer 为 Send + Sync，可跨线程只读共享。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};
use tauri::{AppHandle, Emitter, Manager};

/// SenseVoice 多语模型（zh/en/ja/ko/yue），int8 量化约 239MB。
const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";
const HF_BASE: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";

/// 模型存放目录：app 数据目录/models（首次运行按需下载到此，不打进安装包）。
fn models_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| anyhow!("无法解析数据目录: {e}"))?
        .join("models");
    std::fs::create_dir_all(&dir).ok();
    Ok(dir)
}

/// SenseVoice 模型与词表是否都已就绪。
pub fn model_present(app: &AppHandle) -> bool {
    models_dir(app)
        .map(|d| d.join(MODEL_FILE).exists() && d.join(TOKENS_FILE).exists())
        .unwrap_or(false)
}

/// 下载 SenseVoice 模型到数据目录；大文件进度通过 "model-progress" 事件上报 {downloaded,total,pct}。
pub fn download_model(app: &AppHandle) -> Result<()> {
    let dir = models_dir(app)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()?;
    // 词表很小，不上报进度
    download_file(&client, app, TOKENS_FILE, &dir.join(TOKENS_FILE), false)?;
    // 模型约 239MB，上报进度
    download_file(&client, app, MODEL_FILE, &dir.join(MODEL_FILE), true)?;
    Ok(())
}

/// 下载单个文件到 dest（断点用 .part 临时文件，完成后原子改名）。report=true 时上报进度。
fn download_file(
    client: &reqwest::blocking::Client,
    app: &AppHandle,
    name: &str,
    dest: &Path,
    report: bool,
) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let url = format!("{}/{}", HF_BASE, name);
    let mut resp = client.get(&url).send()?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let tmp = dest.with_extension("part");
    let mut out = std::fs::File::create(&tmp)?;
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = u64::MAX;
    let mut buf = [0u8; 65536];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        downloaded += n as u64;
        if report {
            let pct = if total > 0 { downloaded * 100 / total } else { 0 };
            if pct != last_pct {
                last_pct = pct;
                let _ = app.emit(
                    "model-progress",
                    serde_json::json!({ "downloaded": downloaded, "total": total, "pct": pct }),
                );
            }
        }
    }
    drop(out);
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// 进程内 SenseVoice 识别器。
pub struct SenseVoice {
    recognizer: OfflineRecognizer,
    /// 创建时锁定的源语言（"auto"/"zh"/"en"/"ja"…）；变更时需重建识别器。
    pub language: String,
}

impl SenseVoice {
    /// 加载模型创建识别器。`language` 为源语言；"auto" 让模型自检（短片段固定语种更准）。
    pub fn load(app: &AppHandle, language: &str) -> Result<Self> {
        let dir = models_dir(app)?;
        let model = dir.join(MODEL_FILE);
        let tokens = dir.join(TOKENS_FILE);
        let lang = if language.is_empty() { "auto" } else { language };

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            language: Some(lang.to_string()),
            use_itn: true,
        };
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        // 低功耗 CPU 默认单线程偏慢；用 4 线程吃满多核加速推理（这台无 GPU，纯 CPU）。
        config.model_config.num_threads = 4;

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("创建 SenseVoice 识别器失败（模型文件损坏？）"))?;
        Ok(Self {
            recognizer,
            language: language.to_string(),
        })
    }

    /// 识别一段 16k 单声道 f32 样本，返回去首尾空白后的文本（失败/空返回空串）。
    pub fn transcribe(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}

/// 判断单段输出是否为「重复幻觉」（无限复读同一短语）。
/// 音频被截断 / 数字未说完等不确定场景可能退化成复读机，熔断丢弃避免刷屏。
/// 中日文无空格，按「字符」而非「词」做重复检测：末尾存在长度 p 的单元连续重复 ≥4 次且覆盖 ≥24 字符。
pub fn is_hallucination(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return false;
    }
    let max_p = 20.min(n / 2);
    for p in 1..=max_p {
        let mut reps = 1;
        let mut k = n;
        while k >= 2 * p && chars[k - p..k] == chars[k - 2 * p..k - p] {
            reps += 1;
            k -= p;
        }
        if reps >= 4 && reps * p >= 24 {
            return true;
        }
    }
    false
}

/// 判断整段是否为静音/背景噪声上的「固定幻觉短语」（与音频内容无关的套话）。
/// 视频字幕语料训练的 ASR 模型在无语音段常吐这类套话，带正常标点逃得过重复判据，需按内容精准拦截。
/// 仅当整段「等于」或「主体就是」这些短语时才判定，避免误伤正常句子里偶含的词。
pub fn is_filler_hallucination(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| {
        c == '.' || c == '。' || c == '!' || c == '！' || c == '?' || c == '？' || c.is_whitespace()
    });
    const FILLERS: &[&str] = &[
        "Thank you",
        "Thank you for watching",
        "Thanks for watching",
        "you",
        "ご視聴ありがとうございました",
        "ご清聴ありがとうございました",
        "おやすみなさい",
        "チャンネル登録お願いします",
        "最後までご視聴いただきありがとうございます",
        "字幕",
        "字幕視聴者",
    ];
    FILLERS.iter().any(|f| t.eq_ignore_ascii_case(f))
}

#[cfg(test)]
mod tests {
    use super::{is_filler_hallucination, is_hallucination};

    #[test]
    fn normal_sentence_is_kept() {
        assert!(!is_hallucination(
            "気象庁は太平洋沿岸の広い地域に津波注意報を発表しています"
        ));
        assert!(!is_hallucination("はい"));
        assert!(!is_hallucination(""));
    }

    #[test]
    fn repetition_loop_is_caught() {
        let loop_text = "地震级别为多少，地震名称为マグニチュード，".repeat(20);
        assert!(is_hallucination(&loop_text));
        // 末尾周期性复读
        assert!(is_hallucination("マグニチュードマグニチュードマグニチュードマグニチュード"));
    }

    #[test]
    fn filler_phrases_are_caught() {
        assert!(is_filler_hallucination("Thank you."));
        assert!(is_filler_hallucination(" thank you "));
        assert!(is_filler_hallucination("ご視聴ありがとうございました"));
        // 正常句子里含 "you" 不应被误伤
        assert!(!is_filler_hallucination("Thank you for the tsunami warning today."));
        assert!(!is_filler_hallucination("気象庁は津波注意報を発表しています。"));
    }
}
