// 前后端共享的类型定义。字段用 camelCase，与 Rust 端 serde(rename_all="camelCase") 对齐。

export type SourceKind = "loopback" | "microphone";
export type TargetLang = "zh" | "en" | "ja" | "ko" | "yue" | "fr" | "es" | "de" | "ru";
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
}

// 主窗口 → 悬浮窗 下发的显示配置（悬浮窗是独立窗口、读不到 Settings，靠事件同步）
export interface OverlayConfig {
  displayMode: DisplayMode;
  overlayFontScale: SubtitleScale;
  overlayOpacity: number;
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
  senseVoice: "快速（SenseVoice）",
  qwen3Asr: "精准（Qwen3·需下载）",
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
