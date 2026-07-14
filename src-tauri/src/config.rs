use serde::Deserialize;

fn default_source_lang() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

fn default_translation_engine() -> String {
    "google".to_string()
}

fn default_asr_engine() -> String {
    "senseVoice".to_string()
}

fn default_summary_max_context() -> usize {
    10_000
}

fn default_tts_target_lang() -> String {
    "ja".to_string()
}

fn default_relay_my_lang() -> String {
    "zh".to_string()
}

fn default_voiceprint_threshold() -> f32 {
    0.5
}

fn default_speaker_merge_threshold() -> f32 {
    0.55
}

fn default_seg_silence_ms() -> u64 {
    500
}

fn default_seg_max_sv() -> f32 {
    6.0
}

fn default_seg_max_qwen3() -> f32 {
    10.0
}

fn default_sentence_max_s() -> f32 {
    10.0
}

fn default_mic_gate_rms() -> f32 {
    0.02
}

fn default_translate_timeout_s() -> u64 {
    30
}

/// LLM 模型库中的一条（云端 OpenAI 兼容 或 本地 Ollama）。翻译与提炼按 id 从库里选用。
/// 前端还带 label / capTranslation / capSummary 等「仅 UI」字段（显示名、下拉过滤用），后端 serde 自动忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProfile {
    pub id: String,
    /// true = 本地 Ollama（无需 key）/ false = 云端（需 key）
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
}

/// 前端 start_translation 传入的运行时配置。字段名与前端 camelCase 对齐。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfig {
    /// "loopback" = 系统音频环回；"microphone" = 麦克风
    pub source: String,
    /// 是否启用降噪（RNNoise，48k）
    #[serde(default = "default_true")]
    pub denoise: bool,
    /// 是否用 Silero VAD（sherpa-onnx 自带，按自然停顿切段）；失败回退能量门限
    #[serde(default = "default_true")]
    pub silero_vad: bool,
    /// 源语言（说话人语言）："auto" / "zh" / "en" / "ja" / "ko" / "yue"，传给 SenseVoice 的 language
    #[serde(default = "default_source_lang")]
    pub source_lang: String,
    /// 目标语言："zh" / "en" / "ja"
    pub target_lang: String,

    /// ASR 引擎："senseVoice"(默认、快、CTC) / "qwen3Asr"(精准、LLM 解码、+延迟、需下载 ~940MB)
    #[serde(default = "default_asr_engine")]
    pub asr_engine: String,

    /// 翻译引擎："llm"（大模型，用 llm_profiles + translation_llm_id）/ "google"（免费非官方）/ "none"（纯字幕）
    #[serde(default = "default_translation_engine")]
    pub translation_engine: String,

    /// LLM 模型库（云端 OpenAI 兼容 / 本地 Ollama 统一管理）；翻译与提炼按 id 选用。
    #[serde(default)]
    pub llm_profiles: Vec<LlmProfile>,
    /// 翻译用模型 id（指向 llm_profiles 某条）；engine=="llm" 时生效。
    #[serde(default)]
    pub translation_llm_id: String,
    /// 提炼重点用模型 id（指向 llm_profiles）；空则回退到翻译用模型。
    #[serde(default)]
    pub summary_llm_id: String,

    // VAD 可选调参（不传走默认）
    #[serde(default)]
    pub energy_threshold: Option<f32>,
    #[serde(default)]
    pub silence_ms: Option<u64>,

    /// 重点提炼单次最大上下文（≈token，CJK 约等于字数）：记录超此预算时自动分块 map-reduce 提炼
    #[serde(default = "default_summary_max_context")]
    pub summary_max_context: usize,

    /// 翻译语音回灌：开启后把译文用 Kokoro 合成并播放到 outputDevice（CABLE Input）
    #[serde(default)]
    pub tts_enabled: bool,
    /// TTS 输出设备友好名（如 "CABLE Input (VB-Audio Virtual Cable)"）；空=系统默认输出
    #[serde(default)]
    pub output_device: String,
    /// TTS 音色（Kokoro speaker id，越界自动夹到合法范围）
    #[serde(default)]
    pub tts_voice: i32,
    /// 回灌(出站)目标语言：你说的话译成它再合成给对方听，默认日语
    #[serde(default = "default_tts_target_lang")]
    pub tts_target_lang: String,
    /// 回灌·你的语言（你说&你读共用）：出站识别源 + 入站翻译目标。具体语种、永不 auto。
    /// SenseVoice 下你说须∈zh/en/ja/ko/yue；Qwen3 下可为任一翻译语言(你说走自动识别)。
    #[serde(default = "default_relay_my_lang")]
    pub relay_my_lang: String,
    /// 回灌·对方说的语言（入站识别源）："auto"=自动识别(多语会议默认) / 具体语种。
    /// Qwen3 恒 auto(不能单独传语种)；SenseVoice 可 auto 或 zh/en/ja/ko/yue。
    #[serde(default = "default_source_lang")]
    pub relay_inbound_source_lang: String,
    /// 回灌·原声直通：true=对方听你的原声(mic→CABLE，跳过识别/翻译/TTS) / false=译文。
    #[serde(default)]
    pub relay_passthrough: bool,
    /// 是否把本条流水线译文写存档（入站写，出站不写，避免两条同写撞文件）
    #[serde(default = "default_true")]
    pub write_transcript: bool,

    /// 说话人门控（仅出站/回灌）：开启后出站每段先比对登记声纹，非本人直接丢，挡掉旁人。
    /// 需先登记声纹（voiceprint.json）；未登记时此开关无效自动降级。
    #[serde(default)]
    pub voiceprint_gate: bool,
    /// 声纹相似度阈值（余弦，∈[-1,1]）：段声纹与登记向量相似度 ≥ 此值才放行。默认 0.5。
    #[serde(default = "default_voiceprint_threshold")]
    pub voiceprint_threshold: f32,

    /// 说话人分离（仅入站）：开启后把一段里不同说话人的话按 token 时间戳拆成多行并标注。实验性，默认关，可运行中热切。
    #[serde(default)]
    pub diarization_enabled: bool,
    /// 全局说话人库跨段归并阈值（余弦）：段声纹与某说话人质心相似度 ≥ 此值即归为同一人。
    /// 越大→归并越严（同一人易裂成多号），越小→归并越松（易把两人归成一号）。默认 0.55。
    #[serde(default = "default_speaker_merge_threshold")]
    pub speaker_merge_threshold: f32,

    /// 分段最大时长·SenseVoice（秒）：连续说话超此值强制切段。默认 6。
    #[serde(default = "default_seg_max_sv")]
    pub seg_max_sec_sensevoice: f32,
    /// 分段最大时长·Qwen3（秒）：默认 10（需更长上下文）。
    #[serde(default = "default_seg_max_qwen3")]
    pub seg_max_sec_qwen3: f32,
    /// VAD 停顿（句界，毫秒）：连续静音达此 VAD 切一句（停顿切）。默认 500。
    #[serde(default = "default_seg_silence_ms")]
    pub seg_silence_ms: u64,
    /// 句缓冲超长强制定稿（秒）：无停顿连续说到此时长，强制切句。默认 25。
    #[serde(default = "default_sentence_max_s")]
    pub sentence_max_s: f32,
    /// 出站近场音量门（段 RMS）：仅出站，段响度低于此判为远场旁人/环境，丢弃不识别。默认 0.02。
    /// 不同麦克风/环境响度差异大，可按需微调（调大更严挡旁人、调小更宽松）。
    #[serde(default = "default_mic_gate_rms")]
    pub mic_gate_rms: f32,
    /// 翻译单请求 HTTP 超时（秒）：慢的本地大模型长句可能耗时较久，超此判失败。默认 30。
    #[serde(default = "default_translate_timeout_s")]
    pub translate_timeout_s: u64,
}

impl RuntimeConfig {
    /// 给 LLM 提示词用的目标语言英文名
    pub fn target_lang_name(&self) -> &'static str {
        match self.target_lang.as_str() {
            "en" => "English",
            "ja" => "Japanese",
            "ko" => "Korean",
            "yue" => "Cantonese",
            "fr" => "French",
            "es" => "Spanish",
            "de" => "German",
            "ru" => "Russian",
            "hi" => "Hindi",
            "it" => "Italian",
            "pt" => "Portuguese",
            _ => "Simplified Chinese",
        }
    }

    /// Kokoro espeak G2P 语种提示码（按目标语言）
    pub fn tts_lang(&self) -> &'static str {
        match self.target_lang.as_str() {
            "en" => "en", "ja" => "ja", "ko" => "ko", "fr" => "fr",
            "es" => "es", "de" => "de", "ru" => "ru", "yue" => "yue",
            // Kokoro 葡语音色是巴西葡语；espeak 用 "pt"(欧葡)口音略偏但可懂，如需更准可换 "pt-br"
            "hi" => "hi", "it" => "it", "pt" => "pt",
            _ => "zh",
        }
    }
}
