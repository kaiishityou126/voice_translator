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
}

impl RuntimeConfig {
    /// 给 LLM 提示词用的目标语言英文名
    pub fn target_lang_name(&self) -> &'static str {
        match self.target_lang.as_str() {
            "en" => "English",
            "ja" => "Japanese",
            _ => "Simplified Chinese",
        }
    }
}
