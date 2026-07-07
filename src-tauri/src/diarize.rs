//! 说话人分离（实验版：仅加载 + 埋点，不拆段、不改字幕）。
//!
//! 复用 sherpa-onnx 的 `OfflineSpeakerDiarization`：
//!   pyannote-segmentation-3.0（随安装包打包，见 tauri.conf.json bundle.resources）做局部重叠感知分段，
//!   + CAM++（复用声纹那份 campplus_sv_zh_16k.onnx）提说话人嵌入，
//!   + FastClustering（num_clusters=-1 自动定人数）聚类。
//! 段模型缺失时 `load` 返回 Err，调用方静默降级为「不分离」。

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};
use tauri::{AppHandle, Manager};

/// pyannote 分段模型打包资源相对路径（随安装包，见 tauri.conf.json bundle.resources）。
const PYANNOTE_RES: &str = "sidecar/models/pyannote_segmentation_3.onnx";
/// 旧版手动下载子路径（app 数据目录/models 下），作打包资源缺失时的回退，兼容开发期手放。
const PYANNOTE_SUBPATH: &str = "sherpa-onnx-pyannote-segmentation-3-0/model.onnx";

/// 解析 pyannote 分段模型路径：优先打包资源，回退 app 数据目录旧路径；都无返回 None（分离降级）。
fn pyannote_model_path(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve(PYANNOTE_RES, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    let legacy = app
        .path()
        .app_data_dir()
        .ok()?
        .join("models")
        .join(PYANNOTE_SUBPATH);
    legacy.exists().then_some(legacy)
}

/// 分离器（进程内，复用 sherpa 那份 onnxruntime）。
pub struct Diarizer {
    inner: OfflineSpeakerDiarization,
}

impl Diarizer {
    /// 加载分离器。pyannote 或 CAM++ 模型缺失/初始化失败则返回 Err（调用方降级）。
    /// `num_threads` 为分段+嵌入推理线程数（取 1，避免抢占 ASR）；`threshold` 为聚类阈值（越大说话人越少）。
    pub fn load(app: &AppHandle, num_threads: i32, threshold: f32) -> Result<Self> {
        let seg = pyannote_model_path(app)
            .ok_or_else(|| anyhow!("pyannote 分段模型缺失（models/{PYANNOTE_SUBPATH}）"))?;
        let emb = crate::voiceprint::model_path(app).ok_or_else(|| anyhow!("CAM++ 声纹模型缺失"))?;
        let cfg = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(seg.to_string_lossy().into_owned()),
                },
                num_threads,
                debug: false,
                provider: Some("cpu".to_string()),
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(emb.to_string_lossy().into_owned()),
                num_threads,
                debug: false,
                provider: Some("cpu".to_string()),
            },
            clustering: FastClusteringConfig {
                num_clusters: -1, // 自动定人数（按 threshold 聚类）
                // 阈值越大→说话人越少（抑制过分割）；由上层传入（默认 0.7）。
                threshold,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        let inner = OfflineSpeakerDiarization::create(&cfg)
            .ok_or_else(|| anyhow!("diarization 初始化失败（模型损坏或不兼容）"))?;
        Ok(Self { inner })
    }

    /// 对一段 16k 单声道跑分离，返回按开始时间排序的 (start_s, end_s, speaker) 列表。
    pub fn process(&self, samples_16k: &[f32]) -> Vec<(f32, f32, i32)> {
        match self.inner.process(samples_16k) {
            Some(res) => res
                .sort_by_start_time()
                .into_iter()
                .map(|s| (s.start, s.end, s.speaker))
                .collect(),
            None => Vec::new(),
        }
    }
}

/// 特殊标记 token（`<|zh|>` / `<|NEUTRAL|>` 等）：归属/拼接时跳过。
fn is_special(t: &str) -> bool {
    t.starts_with("<|") && t.ends_with("|>")
}

/// 把一个 token 追加到行文本：BPE 词首符 `▁`(U+2581) 还原成前置空格。
fn detok_push(s: &mut String, t: &str) {
    if let Some(rest) = t.strip_prefix('\u{2581}') {
        s.push(' ');
        s.push_str(rest);
    } else {
        s.push_str(t);
    }
}

/// 找 t 时刻所属说话人：优先落在 [start,end] 内的区间（重叠取第一个）；都不在则取端点最近的区间。
fn speaker_at(t: f32, parts: &[(f32, f32, i32)]) -> Option<i32> {
    if parts.is_empty() {
        return None;
    }
    for &(s, e, spk) in parts {
        if t >= s && t <= e {
            return Some(spk);
        }
    }
    parts
        .iter()
        .min_by(|a, b| {
            let da = (t - a.0).abs().min((t - a.1).abs());
            let db = (t - b.0).abs().min((t - b.1).abs());
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|p| p.2)
}

/// 按 token 时间戳把整段文本归属到说话人，合成“连续同说话人”的多行（时间序）。
///
/// 逐 token 归属 + 迟滞平滑 + 最短行合并（不依赖标点，快语稀标点也能在真换人处切）：
///   1) 每个 token 按时间戳查（段内）说话人；
///   2) 迟滞：连续 ≥HYST_K 个 token 属新说话人才真正切换（单/双 token 抖动当噪声，防逐字横跳、防劈词）；
///   3) 成行后，短于 MIN_CHARS 的行并入相邻（消灭“。/半词”碎片，短插话并入邻居）。
/// 返回 Vec<(local_speaker, line_text, t0, t1)>：
///   local_speaker 为「段内」pyannote 编号（**未**跨段归一——跨段一致性交给 `SpeakerRegistry`）；
///   t0/t1 为该行首/末 token 时间戳（秒），供上层据此切音频提声纹、并入全局说话人库。
pub fn attribute(
    tokens: &[String],
    timestamps: &[f32],
    parts: &[(f32, f32, i32)],
) -> Vec<(i32, String, f32, f32)> {
    const HYST_K: usize = 3; // 迟滞：连续≥K个token属新说话人才切换
    const MIN_CHARS: usize = 4; // 最短行：短于此并入相邻

    let n = tokens.len().min(timestamps.len());
    // 1) 逐 token 说话人 + 时间戳（跳过特殊标记）
    let mut toks: Vec<(&str, i32, f32)> = Vec::new();
    for i in 0..n {
        if is_special(&tokens[i]) {
            continue;
        }
        let ts = timestamps[i];
        let spk = speaker_at(ts, parts).unwrap_or(0);
        toks.push((tokens[i].as_str(), spk, ts));
    }
    if toks.is_empty() {
        return Vec::new();
    }

    // 2) 迟滞平滑：候选说话人连续出现达 HYST_K 才切换，并回填最近 K 个 token 的标签
    let mut smoothed: Vec<i32> = Vec::with_capacity(toks.len());
    let mut cur = toks[0].1;
    let mut cand = cur;
    let mut cand_run = 0usize;
    for &(_, spk, _) in &toks {
        smoothed.push(cur);
        if spk == cur {
            cand_run = 0;
            cand = cur;
        } else if spk == cand {
            cand_run += 1;
        } else {
            cand = spk;
            cand_run = 1;
        }
        if cand_run >= HYST_K {
            cur = cand;
            let len = smoothed.len();
            for j in len.saturating_sub(HYST_K)..len {
                smoothed[j] = cur;
            }
            cand_run = 0;
        }
    }

    // 3) 成行（带首/末 token 时间戳）
    let mut runs: Vec<(i32, String, f32, f32)> = Vec::new();
    for (idx, (t, _, ts)) in toks.iter().enumerate() {
        let s = smoothed[idx];
        match runs.last_mut() {
            Some((rs, text, _t0, t1)) if *rs == s => {
                detok_push(text, t);
                *t1 = *ts;
            }
            _ => {
                let mut text = String::new();
                detok_push(&mut text, t);
                runs.push((s, text, *ts, *ts));
            }
        }
    }
    for r in runs.iter_mut() {
        r.1 = r.1.trim().to_string();
    }
    runs.retain(|r| !r.1.is_empty());

    // 4) 最短行合并：短于 MIN_CHARS 的并入相邻（时间范围一并吞并；短插话被吸收，不单独成行）
    let mut i = 0;
    while runs.len() > 1 && i < runs.len() {
        if runs[i].1.chars().count() < MIN_CHARS {
            if i > 0 {
                let (_, text, _t0, t1) = runs.remove(i);
                runs[i - 1].1.push_str(&text);
                runs[i - 1].3 = t1; // 末时间向后延伸
            } else {
                let (_, text, t0, _t1) = runs.remove(0);
                runs[0].1.insert_str(0, &text);
                runs[0].2 = t0; // 起时间向前延伸
            }
        } else {
            i += 1;
        }
    }
    // 段内编号不再归一化：局部 speaker 原样保留，全局稳定编号由 SpeakerRegistry 分配。
    runs
}

/// 会话级「全局说话人库」（A 在线增量）：把跨段的同一个人归到稳定编号。
///
/// 段内 pyannote 编号只在段内有效，换段就重置；本库对每段/每行提到的 CAM++ 声纹向量，
/// 与库内各说话人质心比余弦相似度：最像且 ≥ 阈值则归其编号并**增量精炼**该质心，
/// 否则登记为新说话人。会话开始 new、结束丢弃——编号在一次会话内跨段稳定。
pub struct SpeakerRegistry {
    /// 每个全局说话人的 (质心向量, 已并入段数)；下标即全局编号（0 起）。
    centroids: Vec<(Vec<f32>, u32)>,
}

impl SpeakerRegistry {
    pub fn new() -> Self {
        Self {
            centroids: Vec::new(),
        }
    }

    /// 把一个声纹向量归到全局说话人，返回其编号。
    /// 与各质心求余弦，最大者若 ≥ threshold 则归入并按段数增量平均更新质心；否则新建。
    pub fn assign(&mut self, emb: &[f32], threshold: f32) -> i32 {
        let mut best = -1i32;
        let mut best_score = threshold; // 必须 ≥ 阈值才算命中
        for (i, (c, _)) in self.centroids.iter().enumerate() {
            let s = crate::voiceprint::cosine(emb, c);
            if s >= best_score {
                best_score = s;
                best = i as i32;
            }
        }
        if best >= 0 {
            let (c, cnt) = &mut self.centroids[best as usize];
            let n = *cnt as f32;
            let m = c.len().min(emb.len());
            for k in 0..m {
                c[k] = (c[k] * n + emb[k]) / (n + 1.0);
            }
            *cnt += 1;
            best
        } else {
            self.centroids.push((emb.to_vec(), 1));
            (self.centroids.len() - 1) as i32
        }
    }

    /// 只在**已有**说话人里就近匹配（余弦 ≥ threshold），不新建、不更新质心。
    /// 供短段/碎片用：声纹不稳时能挂到已知说话人就挂，挂不上返回 None（标未知，不污染库、不裂出虚假新号）。
    pub fn match_only(&self, emb: &[f32], threshold: f32) -> Option<i32> {
        let mut best = -1i32;
        let mut best_score = threshold;
        for (i, (c, _)) in self.centroids.iter().enumerate() {
            let s = crate::voiceprint::cosine(emb, c);
            if s >= best_score {
                best_score = s;
                best = i as i32;
            }
        }
        (best >= 0).then_some(best)
    }
}
