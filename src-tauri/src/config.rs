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

fn default_ollama_base_url() -> String {
    "http://localhost:11434/v1".to_string()
}

fn default_ollama_model() -> String {
    "qwen2.5".to_string()
}

fn default_asr_engine() -> String {
    "senseVoice".to_string()
}

fn default_summary_max_context() -> usize {
    48_000
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

    /// 翻译引擎："openai" / "ollama"(本地) / "google"(免费非官方) / "none"(纯字幕)
    #[serde(default = "default_translation_engine")]
    pub translation_engine: String,

    // OpenAI 兼容接口
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    // Ollama（本地 OpenAI 兼容，无需 key）
    #[serde(default = "default_ollama_base_url")]
    pub ollama_base_url: String,
    #[serde(default = "default_ollama_model")]
    pub ollama_model: String,

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
