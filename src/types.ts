// 前后端共享的类型定义。字段用 camelCase，与 Rust 端 serde(rename_all="camelCase") 对齐。

export type SourceKind = "loopback" | "microphone";
export type TargetLang =
  | "zh" | "en" | "ja" | "ko" | "yue" | "fr" | "es" | "de" | "ru"
  | "hi" | "it" | "pt";
// 源语言：auto = 让 SenseVoice 自动检测；其余为固定语种（短片段固定语种比 auto 准）
export type SourceLang = "auto" | "zh" | "en" | "ja" | "ko" | "yue";
// 翻译引擎：openai 兼容 / 本地 Ollama / Google 免费(非官方) / none=纯字幕不翻译
export type TranslationEngine = "openai" | "ollama" | "google" | "none";
// ASR 引擎：senseVoice = 快(CTC,默认) / qwen3Asr = 精准(LLM 解码,+延迟,需下载 ~940MB)
export type AsrEngine = "senseVoice" | "qwen3Asr";
// 界面主题：light=浅色 / dark=深色 / system=跟随系统
export type ThemeMode = "light" | "dark" | "system";
// 字幕字号档位：sm=小 / md=中 / lg=大
export type SubtitleScale = "sm" | "md" | "lg";
// 字幕显示模式：bilingual=原文+译文 / transOnly=仅译文 / origOnly=仅原文
export type DisplayMode = "bilingual" | "transOnly" | "origOnly";

export interface Settings {
  source: SourceKind;
  denoise: boolean;
  sileroVad: boolean;
  sourceLang: SourceLang;
  targetLang: TargetLang;
  // ASR 识别引擎
  asrEngine: AsrEngine;
  // 翻译
  translationEngine: TranslationEngine;
  llmBaseUrl: string;
  llmApiKey: string;
  llmModel: string;
  ollamaBaseUrl: string;
  ollamaModel: string;
  // 译文文件保留天数（启动清理）与摘要文件保留天数（<=0 表示永久保留）
  transcriptKeepDays: number;
  summaryKeepDays: number;
  // 提炼重点：单次最大上下文（≈token，CJK 约等于字数），超过则自动分块提炼
  summaryMaxContext: number;
  // 界面显示偏好（纯前端，不影响后端流水线）
  theme: ThemeMode;
  subtitleScale: SubtitleScale;
  // 字幕显示模式（主界面 + 悬浮窗共用）
  displayMode: DisplayMode;
  // 悬浮窗预设：字号档位 + 背景不透明度(0~100，0=全透明描边风)
  overlayFontScale: SubtitleScale;
  overlayOpacity: number;
  // 翻译语音回灌：把译文用 Kokoro 合成并播放到 CABLE Input（自动检测虚拟声卡）
  ttsEnabled: boolean;
  // 回灌·你的语言（你说&你读共用，具体语种、永不 auto）
  relayMyLang: TargetLang;
  // 回灌·对方说的语言（入站识别源；auto=自动识别，多语会议默认）
  relayInboundSourceLang: SourceLang;
  // 回灌·原声直通（true=对方听你的原声，跳过翻译+TTS；false=译文）
  relayPassthrough: boolean;
  // 出站翻译目标语（你说话 → 对方听该语）
  ttsTargetLang: TargetLang;
  outputDevice: string;
  ttsVoice: number;
  // 说话人门控（同传出站）：只放行「你的声音」，挡掉旁人。需先登记声纹后开启才生效。
  voiceprintGate: boolean;
  // 声纹相似度阈值（0~1，越高越严）：段声纹与登记声纹相似度 ≥ 此值才放行。
  voiceprintThreshold: number;
}

// 主窗口 → 悬浮窗 下发的显示配置（悬浮窗是独立窗口、读不到 Settings，靠事件同步）
export interface OverlayConfig {
  displayMode: DisplayMode;
  overlayFontScale: SubtitleScale;
  overlayOpacity: number;
  // 输入模式展示：系统音频/麦克风；回灌模式时额外显示「原声」开关
  source: SourceKind;
  relayMode: boolean;
  relayPassthrough: boolean;
}

// 纯字幕引擎(none)没有译文，实际只能显示原文；统一在这里收敛
export function effectiveDisplayMode(s: Settings): DisplayMode {
  return s.translationEngine === "none" ? "origOnly" : s.displayMode;
}

export function pickOverlayConfig(s: Settings): OverlayConfig {
  return {
    displayMode: effectiveDisplayMode(s),
    overlayFontScale: s.overlayFontScale,
    overlayOpacity: s.overlayOpacity,
    source: s.source,
    relayMode: s.ttsEnabled,
    relayPassthrough: s.relayPassthrough,
  };
}

// Rust 端 emit("subtitle", ...) 的负载
// 同一 id 会先后到达两次：先出原文(pending=true, translated 空)，翻译完再回填(pending=false)
export interface SubtitleEvent {
  id: number;
  original: string;
  translated: string;
  ts: number;
  pending: boolean;
  // in=听对方(回灌文本字幕) / out=我说的话(合成给对方)
  direction: "in" | "out";
}

// load_session 命令返回的历史段落（与 SubtitleEvent 同构，无 pending）
export interface Segment {
  id: number;
  ts: number;
  original: string;
  translated: string;
  speaker: string | null;
}

// list_sessions 命令返回的历史会话条目元信息
export interface SessionMeta {
  stem: string;
  path: string;
  count: number;
  preview: string;
}

// Rust 端 emit("status", ...) 的负载
export interface StatusEvent {
  state: "idle" | "starting" | "listening" | "recognizing" | "translating" | "error";
  detail?: string;
  /// 事件来自哪条流水线："in"=入站(听对方) / "out"=出站(回灌) / "all"=全局。缺省按 "in" 处理。
  direction?: "in" | "out" | "all";
}

// Rust 端 emit("summary", ...) 的负载：pending=true 增量帧（全量累计），false 终态
export interface SummaryEvent {
  text: string;
  pending: boolean;
}

// load_summary 命令返回：最新会话是否有摘要文件 / 是否有可提炼译文
export interface SummaryFile {
  exists: boolean;
  text: string;
  hasTranscript: boolean;
}

// 翻译目标语言标签：服务顶栏三键切换 + 字幕页，求紧凑（en 用缩写 "EN"）。
// 与回灌「对方语言」是两码事，各写各的——见 RELAY_LANG_LABEL。
export const TARGET_LANG_LABEL: Record<TargetLang, string> = {
  zh: "中文",
  en: "EN",
  ja: "日本語",
  ko: "한국어",
  yue: "粤语",
  fr: "Français",
  es: "Español",
  de: "Deutsch",
  ru: "Русский",
  hi: "हिन्दी",
  it: "Italiano",
  pt: "Português",
};

// 回灌「对方语言」标签：TTS 合成给对方听的语言，独立业务，用完整语言名（en="English"），
// 与「源语言（说话人语言）」下拉的风格一致；不与顶栏 TARGET_LANG_LABEL 共用。
export const RELAY_LANG_LABEL: Record<TargetLang, string> = {
  zh: "中文",
  en: "English",
  ja: "日本語",
  ko: "한국어",
  yue: "粤语",
  fr: "Français",
  es: "Español",
  de: "Deutsch",
  ru: "Русский",
  hi: "हिन्दी",
  it: "Italiano",
  pt: "Português",
};

// 顶栏目标语言：高频三个做一键 segmented，其余收进「更多」下拉
export const TARGET_LANG_PRIMARY: TargetLang[] = ["zh", "en", "ja"];
export const TARGET_LANG_MORE: TargetLang[] = ["ko", "yue", "fr", "es", "de", "ru"];

export const SOURCE_LANG_LABEL: Record<SourceLang, string> = {
  auto: "自动检测",
  zh: "中文",
  en: "English",
  ja: "日本語",
  ko: "한국어",
  yue: "粤语",
};

export const ASR_ENGINE_LABEL: Record<AsrEngine, string> = {
  senseVoice: "SenseVoice（快）",
  qwen3Asr: "Qwen3（精准）",
};

export const TRANSLATION_ENGINE_LABEL: Record<TranslationEngine, string> = {
  openai: "OpenAI 兼容接口",
  ollama: "Ollama 免费（本地·部署）",
  google: "Google 免费（非官方）",
  none: "纯字幕（不翻译）",
};

export const THEME_LABEL: Record<ThemeMode, string> = {
  system: "跟随系统",
  light: "浅色",
  dark: "深色",
};

export const SUBTITLE_SCALE_LABEL: Record<SubtitleScale, string> = {
  sm: "小",
  md: "中",
  lg: "大",
};

export const DISPLAY_MODE_LABEL: Record<DisplayMode, string> = {
  bilingual: "原文 + 译文",
  transOnly: "仅译文",
  origOnly: "仅原文",
};

// 顶栏快捷切换用的短标签
export const DISPLAY_MODE_SHORT: Record<DisplayMode, string> = {
  bilingual: "双语",
  transOnly: "译文",
  origOnly: "原文",
};

// Kokoro v1.0 多语音色：官方 speaker id（0-52，共 53 个）→ 语种 + 中文说明。
// lang 决定它属于哪门语言，UI 按「对方语言」过滤，保证音色与合成语种始终匹配。
// id 依据 sherpa-onnx kokoro-multi-lang-v1_0 官方映射，Kokoro 无韩语音色。
export const TTS_VOICES: { id: number; lang: TargetLang; label: string }[] = [
  // 日语 (jf_*/jm_*)
  { id: 37, lang: "ja", label: "日语 · 女声 alpha" },
  { id: 38, lang: "ja", label: "日语 · 女声 gongitsune" },
  { id: 39, lang: "ja", label: "日语 · 女声 nezumi" },
  { id: 40, lang: "ja", label: "日语 · 女声 tebukuro" },
  { id: 41, lang: "ja", label: "日语 · 男声 kumo" },
  // 中文 (zf_*/zm_*)
  { id: 45, lang: "zh", label: "中文 · 女声 xiaobei" },
  { id: 46, lang: "zh", label: "中文 · 女声 xiaoni" },
  { id: 47, lang: "zh", label: "中文 · 女声 xiaoxiao" },
  { id: 48, lang: "zh", label: "中文 · 女声 xiaoyi" },
  { id: 49, lang: "zh", label: "中文 · 男声 yunjian" },
  { id: 50, lang: "zh", label: "中文 · 男声 yunxi" },
  { id: 51, lang: "zh", label: "中文 · 男声 yunxia" },
  { id: 52, lang: "zh", label: "中文 · 男声 yunyang" },
  // 英语 · 美式 (af_*/am_*)
  { id: 0, lang: "en", label: "美式英语 · 女声 alloy" },
  { id: 1, lang: "en", label: "美式英语 · 女声 aoede" },
  { id: 2, lang: "en", label: "美式英语 · 女声 bella" },
  { id: 3, lang: "en", label: "美式英语 · 女声 heart" },
  { id: 4, lang: "en", label: "美式英语 · 女声 jessica" },
  { id: 5, lang: "en", label: "美式英语 · 女声 kore" },
  { id: 6, lang: "en", label: "美式英语 · 女声 nicole" },
  { id: 7, lang: "en", label: "美式英语 · 女声 nova" },
  { id: 8, lang: "en", label: "美式英语 · 女声 river" },
  { id: 9, lang: "en", label: "美式英语 · 女声 sarah" },
  { id: 10, lang: "en", label: "美式英语 · 女声 sky" },
  { id: 11, lang: "en", label: "美式英语 · 男声 adam" },
  { id: 12, lang: "en", label: "美式英语 · 男声 echo" },
  { id: 13, lang: "en", label: "美式英语 · 男声 eric" },
  { id: 14, lang: "en", label: "美式英语 · 男声 fenrir" },
  { id: 15, lang: "en", label: "美式英语 · 男声 liam" },
  { id: 16, lang: "en", label: "美式英语 · 男声 michael" },
  { id: 17, lang: "en", label: "美式英语 · 男声 onyx" },
  { id: 18, lang: "en", label: "美式英语 · 男声 puck" },
  { id: 19, lang: "en", label: "美式英语 · 男声 santa" },
  // 英语 · 英式 (bf_*/bm_*)
  { id: 20, lang: "en", label: "英式英语 · 女声 alice" },
  { id: 21, lang: "en", label: "英式英语 · 女声 emma" },
  { id: 22, lang: "en", label: "英式英语 · 女声 isabella" },
  { id: 23, lang: "en", label: "英式英语 · 女声 lily" },
  { id: 24, lang: "en", label: "英式英语 · 男声 daniel" },
  { id: 25, lang: "en", label: "英式英语 · 男声 fable" },
  { id: 26, lang: "en", label: "英式英语 · 男声 george" },
  { id: 27, lang: "en", label: "英式英语 · 男声 lewis" },
  // 西班牙语 (ef_/em_)
  { id: 28, lang: "es", label: "西班牙语 · 女声 dora" },
  { id: 29, lang: "es", label: "西班牙语 · 男声 alex" },
  // 法语 (ff_)
  { id: 30, lang: "fr", label: "法语 · 女声 siwis" },
  // 意大利语 (if_/im_)
  { id: 35, lang: "it", label: "意大利语 · 女声 sara" },
  { id: 36, lang: "it", label: "意大利语 · 男声 nicola" },
  // 葡萄牙语·巴西 (pf_/pm_)
  { id: 42, lang: "pt", label: "葡萄牙语 · 女声 dora" },
  { id: 43, lang: "pt", label: "葡萄牙语 · 男声 alex" },
  { id: 44, lang: "pt", label: "葡萄牙语 · 男声 santa" },
  // 印地语 (hf_/hm_)
  { id: 31, lang: "hi", label: "印地语 · 女声 alpha" },
  { id: 32, lang: "hi", label: "印地语 · 女声 beta" },
  { id: 33, lang: "hi", label: "印地语 · 男声 omega" },
  { id: 34, lang: "hi", label: "印地语 · 男声 psi" },
];

/// 某目标语言的音色列表（UI 按对方语言过滤用）。
export function voicesForLang(lang: TargetLang): { id: number; lang: TargetLang; label: string }[] {
  return TTS_VOICES.filter((v) => v.lang === lang);
}

// ===== 回灌三套语言能力集 + 随识别引擎派生 =====
// 全部翻译目标语言（= TargetLang 全集，LLM 可输出）
export const ALL_TARGET_LANGS: TargetLang[] = [
  "zh", "en", "ja", "ko", "yue", "fr", "es", "de", "ru", "hi", "it", "pt",
];
// SenseVoice 识别的 5 种（也是合法 TargetLang 子集）
export const SENSEVOICE_LANGS: TargetLang[] = ["zh", "en", "ja", "ko", "yue"];
// Kokoro 有音色、可合成的 8 种（= 对方听可选集，无韩/粤/德/俄）
export const KOKORO_TTS_LANGS: TargetLang[] = ["zh", "en", "ja", "es", "fr", "it", "pt", "hi"];

// 你的语言可选（具体语种、无 auto）：SenseVoice→识别∩翻译=5；Qwen3→全部翻译语言 12
export function relayMyLangOptions(engine: AsrEngine): TargetLang[] {
  return engine === "qwen3Asr" ? ALL_TARGET_LANGS : SENSEVOICE_LANGS;
}
// 对方说（入站识别源）可选：SenseVoice→auto+5；Qwen3→仅 auto（不能单独传语种）
export function relayPeerSpeakOptions(engine: AsrEngine): SourceLang[] {
  return engine === "qwen3Asr" ? ["auto"] : ["auto", "zh", "en", "ja", "ko", "yue"];
}

export const DEFAULT_SETTINGS: Settings = {
  source: "loopback",
  denoise: true,
  sileroVad: true,
  sourceLang: "auto",
  targetLang: "zh",
  asrEngine: "senseVoice",
  // 首次启动默认 Google 免费(非官方,无需 key),开箱即用;用户改过后走 localStorage 存档
  translationEngine: "google",
  llmBaseUrl: "https://api.openai.com/v1",
  llmApiKey: "",
  llmModel: "gpt-4o-mini",
  ollamaBaseUrl: "http://localhost:11434/v1",
  ollamaModel: "qwen2.5",
  transcriptKeepDays: 10,
  summaryKeepDays: 0,
  summaryMaxContext: 10000,
  theme: "system",
  subtitleScale: "sm",
  displayMode: "bilingual",
  overlayFontScale: "md",
  overlayOpacity: 0,
  ttsEnabled: false,
  relayMyLang: "zh",
  relayInboundSourceLang: "auto",
  relayPassthrough: false,
  ttsTargetLang: "ja",
  outputDevice: "",
  ttsVoice: 41,
  voiceprintGate: false,
  voiceprintThreshold: 0.5,
};

import { invoke } from "@tauri-apps/api/core";

const STORAGE_KEY = "voice-translator-settings";

// 持久化走 Rust 写真实文件(同步落盘),不受 WebView localStorage 异步刷盘影响:
// 改完设置后即使强杀进程/重装/主界面强退,最后一次改动也已在磁盘上。
export async function loadSettings(): Promise<Settings> {
  // 1) 优先读 Rust 端的 settings.json
  try {
    const raw = await invoke<string | null>("load_settings");
    if (raw) return { ...DEFAULT_SETTINGS, ...JSON.parse(raw) };
  } catch {
    // 忽略,继续兜底
  }
  // 2) 旧版本数据迁移:文件没有则尝试读 localStorage(读到后下次保存会写入文件)
  try {
    const legacy = localStorage.getItem(STORAGE_KEY);
    if (legacy) return { ...DEFAULT_SETTINGS, ...JSON.parse(legacy) };
  } catch {
    // 忽略损坏的本地存储
  }
  return { ...DEFAULT_SETTINGS };
}

export function saveSettings(s: Settings): void {
  // fire-and-forget:Rust 收到 IPC 后同步写盘,失败也不阻塞 UI
  invoke("save_settings", { json: JSON.stringify(s) }).catch(() => {});
}
