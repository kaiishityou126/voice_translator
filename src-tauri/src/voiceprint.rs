//! 说话人声纹：登记「你的声音」向量 + 出站(回灌)门控用余弦相似度挡掉旁人。
//!
//! 背景：Silero VAD 只判「像不像人声」不判「是不是你」，两个座位外的旁人说话照样是人声、
//! 会被出站流水线收进去误识别、灌 TTS。这里复用 sherpa-onnx 自带的 speaker-embedding
//! (CAM++ 中文声纹，进程内、复用同一份 onnxruntime，无第二份依赖)提取声纹向量：
//! - 登记：录你几秒语音 → 声纹存 app 配置目录/voiceprint.json；
//! - 门控：出站每段先提声纹与登记向量算余弦相似度，低于阈值判为旁人直接丢。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use tauri::{AppHandle, Manager};

/// 声纹模型资源路径（随安装包打包，见 tauri.conf.json bundle.resources）。
const MODEL_RES: &str = "sidecar/models/campplus_sv_zh_16k.onnx";
/// 登记声纹落盘文件名（存 app 配置目录）。
const VP_FILE: &str = "voiceprint.json";

/// 解析打包的声纹模型路径；缺失返回 None（门控/登记随即降级）。
pub fn model_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .resolve(MODEL_RES, tauri::path::BaseDirectory::Resource)
        .ok()
        .filter(|p| p.exists())
}

/// 声纹提取器（进程内，复用 sherpa 那份 onnxruntime）。内部类型已 Send+Sync。
pub struct Extractor {
    inner: SpeakerEmbeddingExtractor,
}

impl Extractor {
    /// 加载声纹模型。`num_threads` 为推理线程数（门控每段一次，取 1~2 即可）。
    pub fn load(app: &AppHandle, num_threads: i32) -> Result<Self> {
        let model = model_path(app).ok_or_else(|| anyhow!("声纹模型缺失（未随包解析到）"))?;
        let cfg = SpeakerEmbeddingExtractorConfig {
            model: Some(model.to_string_lossy().into_owned()),
            num_threads,
            debug: false,
            provider: Some("cpu".to_string()),
        };
        let inner = SpeakerEmbeddingExtractor::create(&cfg)
            .ok_or_else(|| anyhow!("声纹提取器初始化失败（模型损坏或加载错误）"))?;
        Ok(Self { inner })
    }

    /// 16k 单声道样本 → 声纹向量。样本过短/无法计算返回 None。
    pub fn embed(&self, samples_16k: &[f32]) -> Option<Vec<f32>> {
        let stream = self.inner.create_stream()?;
        stream.accept_waveform(16_000, samples_16k);
        stream.input_finished();
        if !self.inner.is_ready(&stream) {
            return None;
        }
        self.inner.compute(&stream)
    }
}

/// 两个等长向量的余弦相似度（∈[-1,1]）。长度不一致/零向量返回 -1（判为不匹配）。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return -1.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn vp_path(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| anyhow!("配置目录不可用: {e}"))?;
    std::fs::create_dir_all(&dir).ok();
    Ok(dir.join(VP_FILE))
}

/// 登记声纹落盘（原子写：先写 .part 再 rename，避免写一半崩溃损坏 voiceprint.json）。
pub fn save(app: &AppHandle, embedding: &[f32]) -> Result<()> {
    let path = vp_path(app)?;
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, serde_json::to_string(embedding)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// 读登记声纹；不存在/损坏/空返回 None。
pub fn load(app: &AppHandle) -> Option<Vec<f32>> {
    let path = vp_path(app).ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<f32>>(&raw)
        .ok()
        .filter(|v| !v.is_empty())
}

/// 是否已登记声纹。
pub fn exists(app: &AppHandle) -> bool {
    load(app).is_some()
}

/// 清除登记声纹。
pub fn clear(app: &AppHandle) -> Result<()> {
    let path = vp_path(app)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// 出站门控：提取器 + 登记向量 + 阈值。
pub struct Gate {
    extractor: Arc<Extractor>,
    enrolled: Vec<f32>,
    /// 余弦相似度阈值：段声纹与登记向量相似度 ≥ 此值才放行。
    pub threshold: f32,
}

impl Gate {
    pub fn new(extractor: Arc<Extractor>, enrolled: Vec<f32>, threshold: f32) -> Self {
        Self {
            extractor,
            enrolled,
            threshold,
        }
    }

    /// 判断该 16k 段是否是登记者。
    /// 提取不出声纹（段太短）时放行，不误杀——交给后续幻觉/套话熔断。
    /// 登记向量与实测声纹维度不一致（模型换代/旧 voiceprint.json）时无法比对：
    /// 返回 DimMismatch 让上层降级放行并提示重新登记，绝不静默把每段都判旁人丢掉。
    pub fn accepts(&self, seg_16k: &[f32]) -> GateDecision {
        match self.extractor.embed(seg_16k) {
            Some(emb) => {
                if emb.len() != self.enrolled.len() {
                    return GateDecision::DimMismatch;
                }
                if cosine(&emb, &self.enrolled) >= self.threshold {
                    GateDecision::Accept
                } else {
                    GateDecision::Reject
                }
            }
            None => GateDecision::Accept,
        }
    }
}

/// 出站声纹门控判定结果。
pub enum GateDecision {
    /// 放行：是登记者，或段太短无法判定（宁放勿杀）。
    Accept,
    /// 拒绝：声纹与登记者相似度低于阈值，判为旁人。
    Reject,
    /// 登记向量与实测声纹维度不一致（模型换代/旧登记文件）：无法比对，需降级放行 + 提示重新登记。
    DimMismatch,
}
