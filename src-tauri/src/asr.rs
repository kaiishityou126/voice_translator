//! 本地 ASR：sherpa-onnx，进程内离线识别（无 sidecar / HTTP）。
//! 两套引擎共用同一 `OfflineRecognizer` API，仅 model_config 子结构不同：
//!   - SenseVoice：CTC，快，无语言模型（默认）；
//!   - Qwen3-ASR：编码器 + LLM 解码器，准（专名/同音词），+延迟，需下载 ~940MB。
//! 采集到的 16k 单声道 f32 直接喂给识别器；标点与 ITN（数字规整）在模型内完成。
//! sherpa-onnx 静态链接，无运行期 DLL；OfflineRecognizer 为 Send + Sync，可跨线程只读共享。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use sherpa_onnx::{
    OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig, SpokenLanguageIdentification,
    SpokenLanguageIdentificationConfig, SpokenLanguageIdentificationWhisperConfig,
};
use tauri::{AppHandle, Emitter, Manager};

/// 引擎无关的进程内识别器抽象。两套引擎解码循环相同，仅加载配置不同。
pub trait Asr: Send + Sync {
    /// 识别一段 16k 单声道 f32 样本，返回去首尾空白后的文本（失败/空返回空串）。
    fn transcribe(&self, samples: &[f32]) -> String;
    /// 身份指纹（引擎 + 锁定的源语言）；与期望值不一致时上层重建识别器。
    fn fingerprint(&self) -> String;
    /// 新一轮流水线开始前调用：auto 模式据此重新检测语种（默认空实现，固定语种/Qwen3 无需）。
    fn reset_session(&self) {}
}

/// 计算「引擎 + 源语言（+ auto 时的目标语言）」的期望指纹，供上层判断是否需重建识别器（无需先加载模型）。
pub fn fingerprint_for(engine: &str, language: &str, target_lang: &str) -> String {
    let lang = if language.is_empty() { "auto" } else { language };
    match engine {
        // Qwen3 多语自动，源语言不参与加载 → 切语言不必重建
        "qwen3Asr" => "qwen3Asr".to_string(),
        // auto：目标语言决定「哪门语言走 auto 识别器」的路由，故并进指纹（换目标语言下次重建）
        _ if lang == "auto" => format!("senseVoice:auto:{target_lang}"),
        _ => format!("senseVoice:{lang}"),
    }
}

/// 按引擎判断模型是否已就绪。
pub fn model_present(app: &AppHandle, engine: &str) -> bool {
    match engine {
        "qwen3Asr" => qwen3_present(app),
        _ => sensevoice_present(app),
    }
}

/// 按引擎下载模型（进度走 "model-progress" 事件）。
pub fn download(app: &AppHandle, engine: &str) -> Result<()> {
    match engine {
        "qwen3Asr" => download_qwen3(app),
        _ => download_sensevoice(app),
    }
}

/// 按引擎加载识别器（首次加载：SenseVoice ~1~2s，Qwen3 ~5s）。
pub fn load(app: &AppHandle, engine: &str, language: &str, target_lang: &str) -> Result<Arc<dyn Asr>> {
    match engine {
        "qwen3Asr" => Ok(Arc::new(Qwen3Asr::load(app)?)),
        _ => {
            let lang = if language.is_empty() { "auto" } else { language };
            // source_lang=auto：base LID 逐段判语种 → 路由到对应固定识别器;目标语言走常驻 auto
            if lang == "auto" {
                Ok(Arc::new(AutoSenseVoice::load(app, target_lang)?))
            } else {
                Ok(Arc::new(SenseVoice::load(app, lang)?))
            }
        }
    }
}

/// SenseVoice 通用多语模型（zh/en/ja/ko/yue），int8 量化约 228MB（2024-07-17 版）。
/// 注意：不要用 2025-09-09 版——那是 WSYue 粤语微调版，日语/韩语会退化成纯汉字乱码。
const MODEL_FILE: &str = "model.int8.onnx";
const TOKENS_FILE: &str = "tokens.txt";
const HF_BASE: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/resolve/main";

/// Whisper-base 语种识别(LID)模型：仅 encoder/decoder int8 两个文件(合计约 160MB)。
/// 与 SenseVoice 同一套 sherpa onnxruntime，不引入新依赖、不撞符号。source_lang="auto" 时开场判别一次语种。
/// base 比 tiny 判别更稳(尤其短段、ja/ko/zh/en 混说);粤语 yue 仍弱属长尾。
const LID_ENCODER: &str = "base-encoder.int8.onnx";
const LID_DECODER: &str = "base-decoder.int8.onnx";
const LID_BASE: &str = "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/main";

/// 各文件近似大小（字节），仅用于「合并下载进度」按合计字节算百分比，避免主模型 100% 后再下 LID 造成回跳。
const MODEL_SIZE: u64 = 228 * 1024 * 1024;
const LID_ENCODER_SIZE: u64 = 29 * 1024 * 1024;
const LID_DECODER_SIZE: u64 = 131 * 1024 * 1024;

/// Qwen3-ASR-0.6B-int8：解压后的目录名 / GitHub releases 单一 tar.bz2（约 940MB）。
const QWEN3_DIR: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";
const QWEN3_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2";

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
fn sensevoice_present(app: &AppHandle) -> bool {
    models_dir(app)
        .map(|d| {
            d.join(MODEL_FILE).exists()
                && d.join(TOKENS_FILE).exists()
                // auto 语种检测默认开启 → LID 两个小文件也算 SenseVoice 套件的一部分
                && d.join(LID_ENCODER).exists()
                && d.join(LID_DECODER).exists()
        })
        .unwrap_or(false)
}

/// 下载 SenseVoice 模型到数据目录；大文件进度通过 "model-progress" 事件上报 {downloaded,total,pct}。
fn download_sensevoice(app: &AppHandle) -> Result<()> {
    let dir = models_dir(app)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()?;
    // 词表很小，不上报进度
    download_file(&client, app, TOKENS_FILE, &dir.join(TOKENS_FILE), false, 0, 0)?;
    // 主模型(228MB) + base LID 双文件(160MB) 合并成一条总进度：按合计字节算百分比，
    // 平滑 0→100，避免主模型 100% 后再从 0 下 LID 造成回跳。已存在的文件跳过、不计入合计。
    let batch: [(String, PathBuf, u64); 3] = [
        (format!("{HF_BASE}/{MODEL_FILE}"), dir.join(MODEL_FILE), MODEL_SIZE),
        (format!("{LID_BASE}/{LID_ENCODER}"), dir.join(LID_ENCODER), LID_ENCODER_SIZE),
        (format!("{LID_BASE}/{LID_DECODER}"), dir.join(LID_DECODER), LID_DECODER_SIZE),
    ];
    let grand_total: u64 = batch
        .iter()
        .filter(|(_, dest, _)| !dest.exists())
        .map(|(_, _, size)| *size)
        .sum();
    let mut offset = 0u64;
    for (url, dest, size) in &batch {
        if dest.exists() {
            continue;
        }
        download_url(&client, app, url, dest, true, offset, grand_total)?;
        offset += size;
    }
    Ok(())
}

/// Qwen3 解压目录：app 数据目录/models/<QWEN3_DIR>。
fn qwen3_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(models_dir(app)?.join(QWEN3_DIR))
}

/// Qwen3 四个模型组件是否都已就绪（tokenizer 是目录）。
fn qwen3_present(app: &AppHandle) -> bool {
    let Ok(d) = qwen3_dir(app) else {
        return false;
    };
    d.join("conv_frontend.onnx").exists()
        && d.join("encoder.int8.onnx").exists()
        && d.join("decoder.int8.onnx").exists()
        && d.join("tokenizer").is_dir()
}

/// 下载 Qwen3 模型(单一 tar.bz2,约 940MB)并用系统 tar(Win10+ 自带 bsdtar,含 bz2lib)解压。
/// 下载进度走 "model-progress"；解压期间发一条 pct=100 表示进入解压阶段。
fn download_qwen3(app: &AppHandle) -> Result<()> {
    let dir = models_dir(app)?;
    let tarball = dir.join("qwen3-asr.tar.bz2");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()?;
    // 下整包(report=true 上报进度);download_to 直接下到指定 URL/路径
    download_url(&client, app, QWEN3_URL, &tarball, true, 0, 0)?;

    // 进入解压阶段：前端可据此切到「解压中」提示
    let _ = app.emit(
        "model-progress",
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
        return Err(anyhow!("tar 解压 Qwen3 模型失败（压缩包损坏？）"));
    }
    let _ = std::fs::remove_file(&tarball);
    if !qwen3_present(app) {
        return Err(anyhow!("Qwen3 模型解压后文件不完整"));
    }
    Ok(())
}

/// 下载单个 HF 文件到 dest（按 HF_BASE/name 拼 URL）。report=true 时上报进度。
fn download_file(
    client: &reqwest::blocking::Client,
    app: &AppHandle,
    name: &str,
    dest: &Path,
    report: bool,
    offset: u64,
    grand_total: u64,
) -> Result<()> {
    let url = format!("{}/{}", HF_BASE, name);
    download_url(client, app, &url, dest, report, offset, grand_total)
}

/// 按完整 URL 下载到 dest（先写 .part 临时文件，完成后原子改名，避免半截文件被当成完整模型）。
/// report=true 时上报进度。grand_total>0 时按「合计字节」算进度（offset=之前文件已下字节），
/// 用于多文件合并成一条总进度、避免回跳；grand_total=0 时按本文件自身 Content-Length 算（未知则按 4MB tick）。
fn download_url(
    client: &reqwest::blocking::Client,
    app: &AppHandle,
    url: &str,
    dest: &Path,
    report: bool,
    offset: u64,
    grand_total: u64,
) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let mut resp = client.get(url).send()?.error_for_status()?;
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
            // 合并进度模式：用合计字节;否则用本文件自身进度
            let (rep_downloaded, rep_total) = if grand_total > 0 {
                (offset + downloaded, grand_total)
            } else {
                (downloaded, total)
            };
            let pct = if rep_total > 0 {
                rep_downloaded * 100 / rep_total
            } else {
                0
            };
            // total 未知时（如 GitHub CDN 不返回 Content-Length），按每下满 4MB 触发一次，
            // 让前端显示「已下载 NNN MB」而非卡在 0%。
            let tick = if rep_total > 0 {
                pct
            } else {
                downloaded / (4 * 1024 * 1024)
            };
            if tick != last_pct {
                last_pct = tick;
                let _ = app.emit(
                    "model-progress",
                    serde_json::json!({ "downloaded": rep_downloaded, "total": rep_total, "pct": pct }),
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
}

impl Asr for SenseVoice {
    fn transcribe(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    fn fingerprint(&self) -> String {
        format!("senseVoice:{}", self.language)
    }
}

/// source_lang="auto" 的 SenseVoice 包装：多语言会议场景，逐段 base LID 判语种 → 路由。
/// - 目标语言(通常中文)走常驻 `auto` 识别器：SenseVoice 中文最强，auto 自判即准，省一个识别器;
/// - 其余外语各走「固定语种」识别器，按需现建、放进 LRU 池(封顶 POOL_CAP，超了淘汰最久未用);
/// - 段太短/判不出时沿用上一段语种(避免短段乱跳)，再不行用 auto 兜底。
/// 不再「开场锁定整场」，因此同一场会议里中/日/英可逐段切换。
pub struct AutoSenseVoice {
    app: AppHandle,
    lid: SpokenLanguageIdentification,
    /// 目标语言代码(zh/en/ja…)；某段判出的语种 == 它时走 auto，省一个识别器。
    target: String,
    /// 常驻 auto 识别器：兼「目标语言」与「判不出时兜底」。仅 ASR 线程访问，Mutex 无竞争。
    auto: Mutex<SenseVoice>,
    /// 外语固定识别器池，LRU：尾部=最近使用，超 POOL_CAP 从头部淘汰。
    pool: Mutex<Vec<SenseVoice>>,
    /// 上一段判定的语种，用于短段/判失败时继承。
    last_lang: Mutex<String>,
}

/// 外语固定识别器封顶数(auto 不计入)；峰值常驻 = auto + POOL_CAP 个，约 (1+POOL_CAP)×228MB。
const POOL_CAP: usize = 2;

impl AutoSenseVoice {
    pub fn load(app: &AppHandle, target_lang: &str) -> Result<Self> {
        let dir = models_dir(app)?;
        let config = SpokenLanguageIdentificationConfig {
            whisper: SpokenLanguageIdentificationWhisperConfig {
                encoder: Some(dir.join(LID_ENCODER).to_string_lossy().into_owned()),
                decoder: Some(dir.join(LID_DECODER).to_string_lossy().into_owned()),
                tail_paddings: 0,
            },
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };
        let lid = SpokenLanguageIdentification::create(&config)
            .ok_or_else(|| anyhow!("创建 Whisper 语种识别器失败（模型文件损坏？）"))?;
        // 常驻 auto：兼目标语言识别 + 判不出时兜底
        let auto = SenseVoice::load(app, "auto")?;
        let target = if target_lang.is_empty() {
            "zh".to_string()
        } else {
            target_lang.to_string()
        };
        Ok(Self {
            app: app.clone(),
            lid,
            target,
            auto: Mutex::new(auto),
            pool: Mutex::new(Vec::new()),
            last_lang: Mutex::new(String::new()),
        })
    }

    /// 取/建该语种的固定识别器并跑识别；维护 LRU(用过的移到尾部，超量淘汰头部)。
    /// 现建一个识别器约 1~2s，仅该语种首次出现时发生。建失败则回退 auto。
    fn transcribe_fixed(&self, lang: &str, samples: &[f32]) -> String {
        let mut pool = self.pool.lock().unwrap();
        if let Some(idx) = pool.iter().position(|sv| sv.language == lang) {
            let sv = pool.remove(idx); // 取出
            let out = sv.transcribe(samples);
            pool.push(sv); // 回到尾部=最近使用
            return out;
        }
        // 池中没有：现建
        match SenseVoice::load(&self.app, lang) {
            Ok(sv) => {
                let out = sv.transcribe(samples);
                pool.push(sv);
                if pool.len() > POOL_CAP {
                    pool.remove(0); // 淘汰最久未用(头部)
                }
                let _ = self
                    .app
                    .emit("language-detected", serde_json::json!({ "lang": lang }));
                out
            }
            Err(e) => {
                eprintln!("[asr] 固定识别器加载失败({lang})，回退 auto: {e}");
                self.auto.lock().unwrap().transcribe(samples)
            }
        }
    }

    /// 用 LID 判这段音频的语种，映射到 SenseVoice 支持的 5 种；其余（含判别失败/段太短）返回 "auto"。
    fn detect_lang(&self, samples: &[f32]) -> String {
        // 段太短(<1.5s)时 LID 极不可靠（一声"嗯"会被判成 en/zh 导致误锁整场）：直接放弃，用兜底识别器，等够长的段再判。
        if samples.len() < 16_000 * 3 / 2 {
            return "auto".to_string();
        }
        let stream = self.lid.create_stream();
        stream.accept_waveform(16_000, samples);
        let raw = self.lid.compute(&stream).map(|r| r.lang).unwrap_or_default();
        // 诊断：打印本段时长与 LID 原始输出，定位判错/判空
        eprintln!(
            "[lid] dur={:.2}s raw=\"{}\"",
            samples.len() as f32 / 16_000.0,
            raw
        );
        match raw.as_str() {
            "zh" | "en" | "ja" | "ko" | "yue" => raw,
            _ => "auto".to_string(),
        }
    }
}

impl Asr for AutoSenseVoice {
    fn transcribe(&self, samples: &[f32]) -> String {
        // 逐段判语种 → 路由（不锁定，整场可在中/日/英间切换）
        let mut lang = self.detect_lang(samples);
        // 判不出(短段/失败)：沿用上一段语种，避免短段乱跳；无上一段则保持 "auto" 兜底
        if lang == "auto" {
            let last = self.last_lang.lock().unwrap().clone();
            if !last.is_empty() {
                lang = last;
            }
        } else {
            *self.last_lang.lock().unwrap() = lang.clone();
        }

        // 路由：目标语言 或 判不出 → 常驻 auto；其余外语 → 固定识别器池
        let out = if lang == "auto" || lang == self.target {
            self.auto.lock().unwrap().transcribe(samples)
        } else {
            self.transcribe_fixed(&lang, samples)
        };
        eprintln!(
            "[lid] route={} dur={:.2}s text=\"{}\"",
            lang,
            samples.len() as f32 / 16_000.0,
            out
        );
        out
    }

    fn fingerprint(&self) -> String {
        format!("senseVoice:auto:{}", self.target)
    }

    fn reset_session(&self) {
        // 新一轮开始：清空外语识别器池与上一段语种(释放外语识别器内存)，保留常驻 auto
        self.pool.lock().unwrap().clear();
        self.last_lang.lock().unwrap().clear();
    }
}

/// 进程内 Qwen3-ASR 识别器（编码器 + LLM 解码器，多语自动）。
pub struct Qwen3Asr {
    recognizer: OfflineRecognizer,
}

impl Qwen3Asr {
    /// 加载 Qwen3 四组件创建识别器（num_threads=6 实测最优）。
    pub fn load(app: &AppHandle) -> Result<Self> {
        let dir = qwen3_dir(app)?;
        let s = |p: PathBuf| p.to_string_lossy().into_owned();

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
            conv_frontend: Some(s(dir.join("conv_frontend.onnx"))),
            encoder: Some(s(dir.join("encoder.int8.onnx"))),
            decoder: Some(s(dir.join("decoder.int8.onnx"))),
            tokenizer: Some(s(dir.join("tokenizer"))), // 注意：是目录
            max_total_len: 512,
            max_new_tokens: 512,
            temperature: 1e-6,
            top_p: 0.8,
            seed: 42,
            hotwords: None,
            ..Default::default()
        };
        config.model_config.num_threads = 6;

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("创建 Qwen3-ASR 识别器失败（模型文件损坏？）"))?;
        Ok(Self {
            recognizer,
        })
    }
}

impl Asr for Qwen3Asr {
    fn transcribe(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    fn fingerprint(&self) -> String {
        "qwen3Asr".to_string()
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
    // 单元要重复 ≥4 次必然 p ≤ n/4，故上限取 n/2 已足够；不写死常数，任意长度复读都能覆盖。
    let max_p = n / 2;
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

/// 判断整段是否为 Qwen3-ASR 的「对话模板泄漏」。
/// Qwen3-ASR 是 LLM-decoder，内部把识别包装成对话；喂入噪声/音乐（无有效语音）时，
/// 解码器在无内容可吐时会退化成输出训练里先验最高的「模板骨架 token」（如 `<asr_text>`、`system`）。
/// 这些标记在正常中/日/英转写里永远不会出现，命中即丢，避免污染翻译。
pub fn is_template_leak(text: &str) -> bool {
    // 一、特殊标记 token：带尖括号，正常语音转写绝不含 → 子串包含即判定
    const MARKERS: &[&str] = &[
        "<asr_text>",
        "<asr_audio>",
        "<|im_start|>",
        "<|im_end|>",
        "<|endoftext|>",
    ];
    if MARKERS.iter().any(|m| text.contains(m)) {
        return true;
    }
    // 二、裸结构词：正常英文也可能出现 → 仅整段恰好等于时判定，零误伤
    let t = text.trim().trim_matches(|c: char| c.is_whitespace() || c == ':' || c == '：');
    const BARE: &[&str] = &["system", "assistant", "user", "language"];
    BARE.iter().any(|b| t.eq_ignore_ascii_case(b))
}

#[cfg(test)]
mod tests {
    use super::{is_filler_hallucination, is_hallucination, is_template_leak};

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

    #[test]
    fn template_leak_is_caught() {
        // 特殊标记：子串命中
        assert!(is_template_leak("<asr_text>system"));
        assert!(is_template_leak("foo<|im_start|>bar"));
        assert!(is_template_leak("<asr_audio>"));
        // 裸结构词：整段恰好等于
        assert!(is_template_leak("system"));
        assert!(is_template_leak(" assistant "));
        assert!(is_template_leak("language:"));
        // 正常句子里含这些词不应被误伤
        assert!(!is_template_leak("The system is working fine."));
        assert!(!is_template_leak("日本語の language を勉強しています。"));
    }
}
