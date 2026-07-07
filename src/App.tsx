import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, emit } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  PictureInPicture2,
  Sparkles,
  History,
  Settings as SettingsIcon,
  Play,
  Square,
  AudioLines,
  ChevronDown,
  Download,
  Mic,
  Volume2,
  Captions,
  Repeat,
  Users,
  AlertTriangle,
} from "lucide-react";
import {
  Settings,
  SubtitleEvent,
  StatusEvent,
  SummaryEvent,
  SummaryFile,
  DisplayMode,
  SessionMeta,
  Segment,
  TARGET_LANG_LABEL,
  TARGET_LANG_PRIMARY,
  TARGET_LANG_MORE,
  SOURCE_LANG_LABEL,
  TRANSLATION_ENGINE_LABEL,
  DISPLAY_MODE_SHORT,
  DISPLAY_MODE_LABEL,
  DEFAULT_SETTINGS,
  pickOverlayConfig,
  effectiveDisplayMode,
  loadSettings,
  saveSettings,
  translationLlmProfile,
  summaryLlmProfile,
} from "./types";
import { SubtitleView } from "./components/SubtitleView";
import { RelayBar } from "./components/RelayBar";
import { SettingsPanel } from "./components/SettingsPanel";
import { SummaryPanel } from "./components/SummaryPanel";
import { HistoryPanel } from "./components/HistoryPanel";
import "./App.css";

// 运行态文字：挪到焦点译文行展示（导航栏不再显示状态）
const LIVE_STATE_LABEL: Partial<Record<StatusEvent["state"], string>> = {
  listening: "正在聆听",
  recognizing: "正在识别",
  translating: "正在翻译",
};

// 虚拟声卡信息按平台切换：Windows 用 VB-Cable，macOS 用 BlackHole。
// activeCable 在应用挂载时按后端 get_platform 结果设置（默认 Windows）。
type CableInfo = { url: string; linkText: string; device: string; meetingMic: string };
const CABLE_INFO: Record<"windows" | "macos", CableInfo> = {
  windows: {
    url: "https://vb-audio.com/Cable/",
    linkText: "VB-Cable（免费版）",
    device: "CABLE Input",
    meetingMic: "CABLE Output",
  },
  macos: {
    url: "https://existential.audio/blackhole/",
    linkText: "BlackHole（免费）",
    device: "BlackHole 2ch",
    meetingMic: "BlackHole 2ch",
  },
};
let activeCable: CableInfo = CABLE_INFO.windows;

// 模型下载清单项：kind 唯一标识包，status 驱动"排队/下载中/完成"三态显示。
type DlKind = "asrSense" | "asrQwen" | "tts";
type DlItem = {
  kind: DlKind;
  label: string;
  status: "pending" | "active" | "done";
  pct: number;
  downloaded: number;
  total: number;
};

// 错误文案渲染：把出现的虚拟声卡链接文案整段渲染成可点击的下载链接，其余按纯文本。
function renderNotice(detail: string) {
  const parts = detail.split(activeCable.linkText);
  if (parts.length === 1) return detail;
  return parts.map((seg, i) => (
    <span key={i}>
      {seg}
      {i < parts.length - 1 && (
        <a
          className="notice-link"
          role="button"
          tabIndex={0}
          title={`点击可跳转到下载页面\n${activeCable.url}`}
          onClick={() => {
            openUrl(activeCable.url).catch(() => {});
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              openUrl(activeCable.url).catch(() => {});
            }
          }}
        >
          {activeCable.linkText}
        </a>
      )}
    </span>
  ));
}

// 重译目标语：回灌按字幕方向(出站=对方听 / 入站=你读)，非回灌用首页目标语。各模式各走各的设置。
function retransTargetLang(s: Settings, direction: "in" | "out"): string {
  if (!s.ttsEnabled) return s.targetLang;
  return direction === "out" ? s.ttsTargetLang : s.relayMyLang;
}
// 提炼语言：提炼的是入站转写(你读的内容)，回灌用「你读」的语言 relayMyLang，非回灌用首页目标语。
function summaryTargetLang(s: Settings): string {
  return s.ttsEnabled ? s.relayMyLang : s.targetLang;
}

function App() {
  const [settings, setSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState<StatusEvent>({ state: "idle" });
  // 各方向最近状态：回灌时入站/出站分开记，焦点行按其方向取状态文字，避免两条流水线抢同一行标签
  const [dirState, setDirState] = useState<{
    in: StatusEvent["state"];
    out: StatusEvent["state"];
  }>({ in: "idle", out: "idle" });
  const [subtitles, setSubtitles] = useState<SubtitleEvent[]>([]);
  // 正在「重译此句」的段落 id 集合（卡片上显示重译中、按钮禁用）
  const [retranslatingIds, setRetranslatingIds] = useState<Set<number>>(new Set());
  const [showSettings, setShowSettings] = useState(true);
  const [overlayOn, setOverlayOn] = useState(false);
  // 目标语言「更多」下拉是否展开
  const [langMenuOpen, setLangMenuOpen] = useState(false);
  // 音频来源（设备入口）下拉是否展开
  const [srcMenuOpen, setSrcMenuOpen] = useState(false);
  // 字幕显示内容（CC 浮层）是否展开
  const [dispMenuOpen, setDispMenuOpen] = useState(false);
  // 待下模型清单（点开始时先算，串行下载逐项点亮）：null=当前无下载。
  const [dlList, setDlList] = useState<DlItem[] | null>(null);
  const [vad, setVad] = useState<string | null>(null);
  // auto 模式下后端 LID 检测到的源语种（zh/en/ja/ko/yue）；用于在焦点行展示「已检测: 日本語」
  const [detectedLang, setDetectedLang] = useState<string | null>(null);
  // 重点提炼：面板可见性、流式文本、是否生成中
  const [summaryOpen, setSummaryOpen] = useState(false);
  const [summary, setSummary] = useState("");
  // 重点面板是否处于错误/提示态（非正常摘要内容），用于整段标红
  const [summaryError, setSummaryError] = useState(false);
  const [summarizing, setSummarizing] = useState(false);
  // 历史会话面板可见性；回看会话(只读)：null=直播态，非空=回看某历史会话
  const [historyOpen, setHistoryOpen] = useState(false);
  const [review, setReview] = useState<{ stem: string; items: SubtitleEvent[] } | null>(null);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  // 悬浮窗 ▶ 启动：悬浮窗读不到主窗口的设置，故由它发事件、主窗口用自己保存的设置开始
  const startRef = useRef<() => void>(() => {});
  // 悬浮窗配置回写走同一个 update；用 ref 持最新闭包，避免 mount-only 监听捕获陈旧 running/settings
  const updateRef = useRef<(next: Settings) => void>(() => {});
  // 用户是否已动过设置：防止 mount 后异步加载的存档把刚改的值盖回去
  const userTouched = useRef(false);

  // 启动时从磁盘加载已保存的设置（异步，故不在 useState 初始化里做）
  useEffect(() => {
    loadSettings().then((s) => {
      if (!userTouched.current) setSettings(s);
    });
  }, []);

  // 按平台切换虚拟声卡文案（Windows=VB-Cable / macOS=BlackHole）
  useEffect(() => {
    invoke<string>("get_platform")
      .then((p) => {
        if (p === "macos") activeCable = CABLE_INFO.macos;
      })
      .catch(() => {});
  }, []);

  // 应用界面主题：system 跟随 OS，写到 <html data-theme> 供 CSS 覆盖
  useEffect(() => {
    const root = document.documentElement;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark =
        settings.theme === "dark" ||
        (settings.theme === "system" && mq.matches);
      root.setAttribute("data-theme", dark ? "dark" : "light");
    };
    apply();
    // 跟随系统时，监听 OS 深/浅色切换
    if (settings.theme === "system") {
      mq.addEventListener("change", apply);
      return () => mq.removeEventListener("change", apply);
    }
  }, [settings.theme]);

  // 字幕字号档位 → CSS 变量 --sub-scale（仅缩放字幕区文字）
  useEffect(() => {
    const factor =
      settings.subtitleScale === "sm"
        ? "0.85"
        : settings.subtitleScale === "lg"
        ? "1.25"
        : "1";
    document.documentElement.style.setProperty("--sub-scale", factor);
  }, [settings.subtitleScale]);

  // 显示配置(显示模式 / 悬浮窗字号 / 不透明度)变化时下发给悬浮窗(独立窗口读不到 Settings)
  useEffect(() => {
    emit("overlay-config", pickOverlayConfig(settings)).catch(() => {});
  }, [
    settings.displayMode,
    settings.translationEngine,
    settings.overlayFontScale,
    settings.overlayOpacity,
    settings.source,
    settings.ttsEnabled,
    settings.relayPassthrough,
  ]);

  // 订阅 Rust 端事件
  useEffect(() => {
    const unsubs: Array<() => void> = [];
    listen<SubtitleEvent>("subtitle", (e) => {
      // 同 id 回填（原文→译文），否则追加
      setSubtitles((prev) => {
        const idx = prev.findIndex((s) => s.id === e.payload.id);
        if (idx >= 0) {
          const next = prev.slice();
          next[idx] = e.payload;
          return next;
        }
        // 整场留内存：去掉旧的 200 条上限，DOM 由 SubtitleView 虚拟滚动兜底
        return [...prev, e.payload];
      });
    }).then((u) => unsubs.push(u));
    listen<StatusEvent>("status", (e) => {
      setStatus(e.payload);
      const s = e.payload.state;
      // 按方向记最近状态（direction 缺省当入站）；"all"（停止的权威 idle）两条都置。
      // running 不在此直接置 —— 见下方按 dirState 推导的 effect（回灌时单条方向报错/停不误停另一条）。
      const d = e.payload.direction;
      setDirState((prev) =>
        d === "all"
          ? { in: s, out: s }
          : d === "out"
            ? { ...prev, out: s }
            : { ...prev, in: s }
      );
    }).then((u) => unsubs.push(u));
    listen<{ pct: number; downloaded: number; total: number }>("model-progress", (e) => {
      // 识别模型进度：回填清单里的 ASR 项（asrSense/asrQwen 只会存在其一），保留 kind/label/status。
      setDlList((prev) =>
        prev &&
        prev.map((it) =>
          it.kind === "asrSense" || it.kind === "asrQwen" ? { ...it, ...e.payload } : it
        )
      );
    }).then((u) => unsubs.push(u));
    listen<{ pct: number; downloaded: number; total: number }>("tts-progress", (e) => {
      // 合成模型进度：回填清单里的 tts 项。
      setDlList((prev) => prev && prev.map((it) => (it.kind === "tts" ? { ...it, ...e.payload } : it)));
    }).then((u) => unsubs.push(u));
    listen<{ vad: string }>("engine-info", (e) => {
      setVad(e.payload.vad);
    }).then((u) => unsubs.push(u));
    listen<{ lang: string }>("language-detected", (e) => {
      setDetectedLang(e.payload.lang);
    }).then((u) => unsubs.push(u));
    listen<SummaryEvent>("summary", (e) => {
      // 流式线程把「[提炼失败] ...」也当一条 summary 终帧发来（invoke 早已 Ok 返回，走不到 catch）；
      // 命中失败标记时置 error 让面板走警告色，而非当普通正文黑字显示。
      const text = e.payload.text;
      setSummary(text);
      setSummaryError(text.startsWith("[提炼失败]"));
      setSummarizing(e.payload.pending);
    }).then((u) => unsubs.push(u));
    listen("overlay-start", () => startRef.current()).then((u) => unsubs.push(u));
    // 悬浮窗挂载后请求一次当前显示配置(它启动时拿不到主窗最新设置)
    listen("overlay-ready", () => {
      emit("overlay-config", pickOverlayConfig(settingsRef.current)).catch(() => {});
    }).then((u) => unsubs.push(u));
    // 悬浮窗上的字号/透明度控件改动 → 回写主窗口设置并落盘(单一数据源仍在主窗口)
    listen<Partial<Settings>>("overlay-config-change", (e) => {
      updateRef.current({ ...settingsRef.current, ...e.payload });
    }).then((u) => unsubs.push(u));
    return () => unsubs.forEach((u) => u());
  }, []);

  // running 由两方向状态推导：任一方向活跃(listening/recognizing/translating)即运行中；
  // 两条都非活跃(idle/error)才算停 → 回灌时单条流水线报错/停不会误停另一条。
  // 只在 dirState 变化时触发；点「开始」时 dirState 未变、不触发，故乐观置的 running=true 得以保留。
  useEffect(() => {
    const active = (st: StatusEvent["state"]) =>
      st === "listening" || st === "recognizing" || st === "translating";
    setRunning(active(dirState.in) || active(dirState.out));
  }, [dirState]);

  // 点击面板外部任意位置即关闭设置 / 重点面板（排除各自的顶栏触发按钮；编辑重点时不关闭以防丢草稿）
  useEffect(() => {
    if (!showSettings && !summaryOpen && !historyOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (!t) return;
      if (
        showSettings &&
        !t.closest(".settings") &&
        !t.closest(".llm-mask") &&
        !t.closest('[data-toggle="settings"]')
      ) {
        setShowSettings(false);
      }
      if (
        summaryOpen &&
        !t.closest(".summary-panel") &&
        !t.closest('[data-toggle="summary"]')
      ) {
        const editing =
          document.querySelector(".summary-panel")?.getAttribute("data-editing") ===
          "true";
        if (!editing) setSummaryOpen(false);
      }
      if (
        historyOpen &&
        !t.closest(".history-panel") &&
        !t.closest('[data-toggle="history"]')
      ) {
        setHistoryOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [showSettings, summaryOpen, historyOpen]);

  // 点击「更多」语言下拉外部即关闭
  useEffect(() => {
    if (!langMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (t && !t.closest(".lang-more")) setLangMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [langMenuOpen]);

  // 点击「音频来源」下拉外部即关闭
  useEffect(() => {
    if (!srcMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (t && !t.closest(".src-menu")) setSrcMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [srcMenuOpen]);

  // 点击「字幕显示」CC 浮层外部即关闭
  useEffect(() => {
    if (!dispMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (t && !t.closest(".cc-menu")) setDispMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [dispMenuOpen]);

  const update = (next: Settings) => {
    userTouched.current = true;
    // 运行中切目标语言：热推给流水线，不重启（仅后续段生效）
    if (running && next.targetLang !== settings.targetLang) {
      invoke("set_target_lang", { targetLang: next.targetLang }).catch(() => {});
    }
    // 运行中切「原声」：只换出站流水线（译文↔原声），入站不动。失败则回退开关并提示。
    if (running && next.ttsEnabled && next.relayPassthrough !== settings.relayPassthrough) {
      invoke("set_relay_passthrough", { config: next }).catch((err) => {
        const raw = String(err);
        setSettings((s) => ({ ...s, relayPassthrough: !next.relayPassthrough }));
        saveSettings({ ...next, relayPassthrough: !next.relayPassthrough });
        setStatus({
          state: "error",
          detail: raw.includes("TTS_MODEL_MISSING")
            ? "语音模型未下载：请停止后在译文模式点开始，先下好模型。"
            : raw.includes("CABLE_MISSING")
            ? `未检测到虚拟声卡 ${activeCable.device}。`
            : `切换原声失败：${raw}`,
        });
      });
    }
    // 运行中切「说话人分离」：热推给入站流水线，不重启（仅后续段生效）
    if (running && next.diarizationEnabled !== settings.diarizationEnabled) {
      invoke("set_diarization", { enabled: next.diarizationEnabled }).catch(() => {});
    }
    setSettings(next);
    saveSettings(next);
  };
  updateRef.current = update;

  async function start() {
    const eng = settings.translationEngine;
    if (eng === "llm") {
      const p = translationLlmProfile(settings);
      if (!p) {
        setStatus({
          state: "error",
          detail: "未选择翻译模型，请在设置的「模型库管理」里添加并选择一个",
        });
        setShowSettings(true);
        return;
      }
      if (!p.isLocal && !p.apiKey.trim()) {
        setStatus({
          state: "error",
          detail: `翻译模型「${p.label}」需要 API Key（或改用 Google 免费 / 纯字幕）`,
        });
        setShowSettings(true);
        return;
      }
    }
    // 退出历史回看（若在），回到直播态
    setReview(null);
    // 开始新一轮前清空上一轮的字幕，避免新旧记录混在一起
    setSubtitles([]);
    // 清空上一轮的语种检测结果（auto 模式下后端会重新检测）
    setDetectedLang(null);
    // 回灌预检：没装虚拟声卡就别白下模型（ASR≈237MB + TTS≈310MB），先提示装 VB-Cable。
    // 后端启动入站前还会再兜一次；此处 fail-open（查询异常按有声卡处理，交给后端把关）。
    if (settings.ttsEnabled && !settings.outputDevice) {
      const hasCable = await invoke<boolean>("cable_exists").catch(() => true);
      if (!hasCable) {
        setStatus({
          state: "error",
          detail: `未检测到虚拟声卡 ${activeCable.device}：请先安装 ${activeCable.linkText} 并重启电脑，或在顶栏输入模式改用「系统音频/麦克风」以原声进会议。`,
        });
        return;
      }
    }
    // 模型按需下载：点开始时先算出"缺哪些包"清单，再串行下载、逐项点亮（观感清晰；不并发）。
    try {
      const need: DlItem[] = [];
      const big = settings.asrEngine === "qwen3Asr";
      const hasAsr = await invoke<boolean>("model_exists", { engine: settings.asrEngine });
      if (!hasAsr) {
        need.push({
          kind: big ? "asrQwen" : "asrSense",
          label: big ? "Qwen3 识别模型" : "SenseVoice 识别模型",
          status: "pending",
          pct: 0,
          downloaded: 0,
          total: 0,
        });
      }
      // 回灌开启：Kokoro 合成模型也纳入清单（原声模式也下好，便于运行中热切回译文）
      if (settings.ttsEnabled) {
        const hasTts = await invoke<boolean>("tts_model_exists");
        if (!hasTts) {
          need.push({ kind: "tts", label: "Kokoro 语音合成模型", status: "pending", pct: 0, downloaded: 0, total: 0 });
        }
      }
      if (need.length > 0) {
        setDlList(need);
        for (let i = 0; i < need.length; i++) {
          const it = need[i];
          // 标记当前项为下载中
          setDlList((prev) => prev && prev.map((x) => (x.kind === it.kind ? { ...x, status: "active" } : x)));
          setStatus({ state: "starting", detail: `正在下载 ${it.label}（${i + 1}/${need.length}）…` });
          if (it.kind === "tts") {
            await invoke("download_tts_model");
          } else {
            await invoke("download_model", { engine: settings.asrEngine });
          }
          // 标记当前项完成
          setDlList((prev) => prev && prev.map((x) => (x.kind === it.kind ? { ...x, status: "done", pct: 100 } : x)));
        }
        setDlList(null);
      }
    } catch (e) {
      setStatus({ state: "error", detail: `模型下载失败：${String(e)}` });
      setDlList(null);
      return;
    }
    setStatus({ state: "starting", detail: "加载模型，请稍候…" });
    setRunning(true);
    try {
      await invoke("start_translation", { config: settings });
    } catch (err) {
      const raw = String(err);
      const detail = raw.includes("CABLE_MISSING")
        ? `未检测到虚拟声卡 ${activeCable.device}：请先安装 ${activeCable.linkText} 并重启电脑，或在顶栏输入模式改用「系统音频/麦克风」以原声进会议。`
        : raw.includes("RELAY_LANG_UNSUPPORTED")
        ? "语音回灌时，SenseVoice 只能识别 中/英/日/韩/粤 作为你的语言。请在回灌语言卡片里把「你的语言」改成这几种之一，或在设置里把识别引擎切换为 Qwen3。"
        : raw.includes("TTS_MODEL_MISSING")
        ? "语音回灌模型缺失，请重新点击开始以下载。"
        : raw.includes("MODEL_MISSING")
        ? "语音识别模型缺失，请重新点击开始以下载。"
        : raw.includes("SCREEN_PERMISSION")
        ? "系统音频采集不可用：请在「系统设置 → 隐私与安全性 → 屏幕录制」中授权本应用，然后重启应用生效。"
        : raw;
      setStatus({ state: "error", detail });
      setRunning(false);
    }
  }
  startRef.current = start;

  async function stop() {
    try {
      await invoke("stop_translation");
    } catch {
      /* 忽略 */
    }
    setRunning(false);
    setStatus({ state: "idle" });
  }

  // 能否提炼重点：有可用的提炼模型即可（独立于翻译引擎，Google/纯字幕也能提炼）
  const canSummarize = !!summaryLlmProfile(settings);

  // 在重点面板显示一条标红的错误/提示
  const showSummaryError = (msg: string) => {
    setSummary(msg);
    setSummaryError(true);
    setSummarizing(false);
  };

  // 首次提炼：有摘要文件直接显示（不调 LLM）；无摘要但有译文且引擎为 LLM 时才调 LLM
  async function summarize() {
    setShowSettings(false);
    setHistoryOpen(false);
    setSummaryOpen(true);
    try {
      const f = await invoke<SummaryFile>("load_summary", { targetLang: summaryTargetLang(settings) });
      if (f.exists) {
        setSummary(f.text);
        setSummaryError(false);
        setSummarizing(false);
        return;
      }
      if (!f.hasTranscript) {
        showSummaryError("本次还没有可提炼的译文记录。");
        return;
      }
      if (!canSummarize) {
        showSummaryError("尚未配置提炼模型，请在设置的「模型库管理」里添加一个 LLM 并选为提炼模型。");
        return;
      }
      setSummary("");
      setSummaryError(false);
      setSummarizing(true);
      await invoke("summarize_session", { config: { ...settings, targetLang: summaryTargetLang(settings) } });
    } catch (e) {
      showSummaryError(`提炼失败：${String(e)}`);
    }
  }

  // 重新提炼：强制再次调 LLM 覆盖摘要文件
  async function resummarize() {
    if (!canSummarize) {
      showSummaryError("尚未配置提炼模型，请在设置的「模型库管理」里添加一个 LLM 并选为提炼模型。");
      return;
    }
    setSummary("");
    setSummaryError(false);
    setSummarizing(true);
    try {
      await invoke("summarize_session", { config: { ...settings, targetLang: summaryTargetLang(settings) } });
    } catch (e) {
      showSummaryError(`提炼失败：${String(e)}`);
    }
  }

  // 保存手动编辑后的重点内容，写回摘要文件
  async function saveSummary(content: string) {
    setSummary(content);
    setSummaryError(false);
    try {
      await invoke("save_summary", { content, targetLang: summaryTargetLang(settings) });
    } catch (e) {
      setStatus({ state: "error", detail: `保存失败：${String(e)}` });
    }
  }

  async function openSummaryDir() {
    try {
      await invoke("open_summary_dir");
    } catch (e) {
      setStatus({ state: "error", detail: String(e) });
    }
  }

  // 进入历史回看：加载某会话全部段落到主字幕区（只读）
  async function openReview(s: SessionMeta) {
    try {
      const segs = await invoke<Segment[]>("load_session", { path: s.path });
      const items: SubtitleEvent[] = segs.map((g) => ({
        id: g.id,
        original: g.original,
        translated: g.translated,
        ts: g.ts,
        pending: false,
        direction: "in",
        speaker: g.speaker,
      }));
      setReview({ stem: s.stem, items });
      setHistoryOpen(false);
      setShowSettings(false);
      setSummaryOpen(false);
    } catch (e) {
      setStatus({ state: "error", detail: `加载会话失败：${String(e)}` });
    }
  }

  // 重译此句：调用后端独立翻译（不带上下文），结果按同 id 回填 subtitles
  async function retranslate(item: SubtitleEvent) {
    if (retranslatingIds.has(item.id)) return;
    setRetranslatingIds((prev) => new Set(prev).add(item.id));
    try {
      const translated = await invoke<string>("retranslate", {
        original: item.original,
        // 各模式各走各的设置：回灌按方向选目标语(源语言不动，沿用当前设置)。
        config: { ...settings, targetLang: retransTargetLang(settings, item.direction) },
      });
      setSubtitles((prev) => {
        const idx = prev.findIndex((s) => s.id === item.id);
        if (idx < 0) return prev;
        const next = prev.slice();
        next[idx] = { ...next[idx], translated, pending: false };
        return next;
      });
    } catch (e) {
      setStatus({ state: "error", detail: `重译失败：${String(e)}` });
    } finally {
      setRetranslatingIds((prev) => {
        const next = new Set(prev);
        next.delete(item.id);
        return next;
      });
    }
  }

  async function toggleOverlay() {
    try {
      const on = await invoke<boolean>("toggle_overlay");
      setOverlayOn(on);
      // 刚打开悬浮框时广播一次当前状态，让其按钮（▶/■）立即与主窗口对齐
      if (on) {
        emit("status", running ? status : { state: "idle" }).catch(() => {});
        // 通知悬浮窗「刚被打开」：先闪一下全背景色+按钮，再淡到设定透明度
        emit("overlay-shown").catch(() => {});
      }
    } catch (e) {
      setStatus({ state: "error", detail: String(e) });
    }
  }

  // 焦点行状态文字 + VAD 标签（替代原导航栏状态块）
  // 焦点行（最新一条）的方向 → 取该方向的状态文字，回灌时不再两条流水线抢同一行
  const liveDir = subtitles.length ? subtitles[subtitles.length - 1].direction : "in";
  const liveState = running
    ? LIVE_STATE_LABEL[dirState[liveDir === "out" ? "out" : "in"]] ?? ""
    : "";
  const vadLabel = vad ? (vad === "silero" ? "Silero" : "能量门限") : "";
  // 仅 auto 模式展示自动检测出的语种（手动选语种时不显示，避免冗余）
  // 有效入站源语言：回灌模式用独立的 relayInboundSourceLang，非回灌用首页 sourceLang
  const effInboundSourceLang = settings.ttsEnabled
    ? settings.relayInboundSourceLang
    : settings.sourceLang;
  const detectedLangLabel =
    effInboundSourceLang === "auto" && detectedLang
      ? `检测: ${SOURCE_LANG_LABEL[detectedLang as keyof typeof SOURCE_LANG_LABEL] ?? detectedLang}`
      : "";

  return (
    <div className="app">
      <div className="topbar">
        <div className="title">
          <span className="brand-mark">
            <AudioLines size={15} strokeWidth={2.2} />
          </span>
          语音<span>翻译</span>
          <small>实时</small>
        </div>

        {/* 音频来源（设备入口）：开播前设一次、运行中禁用，故收成紧凑下拉。
            浮层内日后可再加「输出设备(倒灌 CABLE)」分组，无需占主栏横向空间。
            回灌开启时入站固定系统音频(麦克风归出站独占)，故锁死置灰、不可切换 */}
        <div className="topmenu src-menu" title="输入模式">
          <button
            className={`topmenu-trigger${srcMenuOpen ? " open" : ""}`}
            disabled={running}
            onClick={() => {
              setSrcMenuOpen((v) => !v);
              setDispMenuOpen(false);
              setLangMenuOpen(false);
            }}
          >
            {settings.ttsEnabled ? (
              <Repeat size={15} strokeWidth={1.9} />
            ) : settings.source === "loopback" ? (
              <Volume2 size={15} strokeWidth={1.9} />
            ) : (
              <Mic size={15} strokeWidth={1.9} />
            )}
            <span>
              {settings.ttsEnabled ? "回灌" : settings.source === "loopback" ? "系统音频" : "麦克风"}
            </span>
            <ChevronDown size={13} strokeWidth={2} />
          </button>
          {srcMenuOpen && !running && (
            <div className="lang-menu">
              <div className="menu-label">输入模式</div>
              <button
                className={!settings.ttsEnabled && settings.source === "loopback" ? "active" : ""}
                onClick={() => {
                  update({ ...settings, source: "loopback", ttsEnabled: false });
                  setSrcMenuOpen(false);
                }}
              >
                系统音频（环回）
              </button>
              <button
                className={!settings.ttsEnabled && settings.source === "microphone" ? "active" : ""}
                onClick={() => {
                  update({ ...settings, source: "microphone", ttsEnabled: false });
                  setSrcMenuOpen(false);
                }}
              >
                麦克风
              </button>
              <div className="menu-sep" />
              <button
                className={settings.ttsEnabled ? "active" : ""}
                disabled={settings.translationEngine === "none"}
                title={settings.translationEngine === "none" ? "回灌需要翻译引擎（当前为纯字幕）" : undefined}
                onClick={() => {
                  update({ ...settings, ttsEnabled: true, source: "loopback" });
                  setSrcMenuOpen(false);
                }}
              >
                回灌 · 双向同传
              </button>
            </div>
          )}
        </div>

        {/* 回灌模式：两层语言块+原声开关取代目标语言切换；否则显示目标语言 segmented */}
        {settings.ttsEnabled ? (
          <RelayBar settings={settings} update={update} running={running} />
        ) : (
        <div className="seg seg-lang" title="翻译目标语言">
          {TARGET_LANG_PRIMARY.map((l) => (
            <button
              key={l}
              className={settings.targetLang === l ? "active" : ""}
              onClick={() => update({ ...settings, targetLang: l })}
            >
              {TARGET_LANG_LABEL[l]}
            </button>
          ))}
          <div className="lang-more" data-toggle="lang-more">
            <button
              className={
                TARGET_LANG_MORE.includes(settings.targetLang) ? "active" : ""
              }
              onClick={() => setLangMenuOpen((v) => !v)}
              title="更多目标语言"
            >
              {TARGET_LANG_MORE.includes(settings.targetLang)
                ? TARGET_LANG_LABEL[settings.targetLang]
                : "更多"}
              <ChevronDown size={13} strokeWidth={2} />
            </button>
            {langMenuOpen && (
              <div className="lang-menu">
                {TARGET_LANG_MORE.map((l) => (
                  <button
                    key={l}
                    className={settings.targetLang === l ? "active" : ""}
                    onClick={() => {
                      update({ ...settings, targetLang: l });
                      setLangMenuOpen(false);
                    }}
                  >
                    {TARGET_LANG_LABEL[l]}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
        )}

        {/* 字幕显示内容：CC 浮层（双语/译文/原文），可运行时随手切换；
            纯字幕引擎下只有原文，隐藏。触发器并排显示当前档位短标，状态一目了然 */}
        {settings.translationEngine !== "none" && (
          <div className="topmenu cc-menu" title="字幕显示内容">
            <button
              className={`topmenu-trigger${dispMenuOpen ? " open" : ""}`}
              onClick={() => {
                setDispMenuOpen((v) => !v);
                setSrcMenuOpen(false);
                setLangMenuOpen(false);
              }}
            >
              <Captions size={16} strokeWidth={1.9} />
              <span>{DISPLAY_MODE_SHORT[settings.displayMode]}</span>
              <ChevronDown size={13} strokeWidth={2} />
            </button>
            {dispMenuOpen && (
              <div className="lang-menu">
                <div className="menu-label">字幕显示内容</div>
                {(["bilingual", "transOnly", "origOnly"] as DisplayMode[]).map((m) => (
                  <button
                    key={m}
                    className={settings.displayMode === m ? "active" : ""}
                    onClick={() => {
                      update({ ...settings, displayMode: m });
                      setDispMenuOpen(false);
                    }}
                  >
                    {DISPLAY_MODE_LABEL[m]}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}

        <div className="spacer" />

        <div className="icon-group">
          <button
            className={`iconbtn${settings.diarizationEnabled && settings.asrEngine !== "qwen3Asr" ? " active" : ""}`}
            disabled={settings.asrEngine === "qwen3Asr"}
            onClick={() =>
              update({ ...settings, diarizationEnabled: !settings.diarizationEnabled })
            }
            data-tip={
              settings.asrEngine === "qwen3Asr"
                ? "Qwen 模型不支持说话人分离"
                : settings.diarizationEnabled
                  ? "说话人分离：开（按说话人拆分字幕）"
                  : "说话人分离：关"
            }
          >
            <Users size={17} strokeWidth={1.8} />
          </button>

          <span className="icon-divider" aria-hidden="true" />

          <button
            className={`iconbtn${overlayOn ? " active" : ""}`}
            onClick={toggleOverlay}
            data-tip={overlayOn ? "关闭悬浮窗" : "悬浮窗"}
          >
            <PictureInPicture2 size={17} strokeWidth={1.8} />
          </button>

          <span className="icon-divider" aria-hidden="true" />

          <button
            className={`iconbtn${summaryOpen ? " active" : ""}${summarizing ? " busy" : ""}`}
            onClick={() => {
              // 面板已开 → 直接关闭（后台提炼会继续）
              if (summaryOpen) {
                setSummaryOpen(false);
                return;
              }
              // 面板未开 → 提炼中仅打开看进度，否则走提炼逻辑
              if (summarizing) {
                setShowSettings(false);
                setHistoryOpen(false);
                setSummaryOpen(true);
              } else {
                summarize();
              }
            }}
            data-toggle="summary"
            data-tip={
              summaryOpen
                ? "关闭重点面板"
                : summarizing
                  ? "提炼中…点击查看进度"
                  : "提炼本次会话重点（已有重点文件则直接显示）"
            }
          >
            <Sparkles size={17} strokeWidth={1.8} />
          </button>

          <button
            className={`iconbtn${historyOpen ? " active" : ""}`}
            data-toggle="history"
            onClick={() =>
              setHistoryOpen((v) => {
                const next = !v;
                if (next) {
                  setShowSettings(false);
                  setSummaryOpen(false);
                }
                return next;
              })
            }
            data-tip="历史会话"
          >
            <History size={17} strokeWidth={1.8} />
          </button>

          <span className="icon-divider" aria-hidden="true" />

          <button
            className={`iconbtn${showSettings ? " active" : ""}`}
            data-tip="设置"
            data-tip-pos="end"
            data-toggle="settings"
            onClick={() =>
              setShowSettings((v) => {
                const next = !v;
                if (next) {
                  setSummaryOpen(false);
                  setHistoryOpen(false);
                }
                return next;
              })
            }
          >
            <SettingsIcon size={17} strokeWidth={1.8} />
          </button>
        </div>

        {running ? (
          <button className="btn btn-stop btn-icon" onClick={stop} data-tip="停止" data-tip-pos="end">
            <Square size={16} fill="currentColor" strokeWidth={0} />
          </button>
        ) : (
          <button className="btn btn-start btn-icon" onClick={start} data-tip="开始" data-tip-pos="end">
            <Play size={16} fill="currentColor" strokeWidth={0} />
          </button>
        )}
      </div>

      <div className="app-main">
        {review && (
          <div className="review-bar">
            <span className="review-bar-label">
              回看历史会话 · {review.stem.replace(/_/, " ")} · {review.items.length} 段
            </span>
            <button className="review-bar-exit" onClick={() => setReview(null)}>
              退出回看
            </button>
          </div>
        )}
        <SubtitleView
          items={review ? review.items : subtitles}
          targetLang={settings.targetLang}
          displayMode={effectiveDisplayMode(settings)}
          liveState={liveState}
          vadLabel={vadLabel}
          detectedLangLabel={detectedLangLabel}
          onRetranslate={settings.translationEngine === "none" ? undefined : retranslate}
          retranslatingIds={retranslatingIds}
          readOnly={!!review}
        />

        {(showSettings || summaryOpen || historyOpen) && (
          <div
            className="drawer-scrim"
            onClick={() => {
              // 点遮罩关闭历史会话面板(设置/重点面板各自有关闭入口,保持原行为)
              if (historyOpen) setHistoryOpen(false);
            }}
          />
        )}

        <HistoryPanel
          visible={historyOpen}
          running={running}
          onOpen={openReview}
          onOpenDir={openSummaryDir}
        />

        <SummaryPanel
          text={summary}
          error={summaryError}
          pending={summarizing}
          visible={summaryOpen}
          canResummarize={canSummarize}
          onResummarize={resummarize}
          onSave={saveSummary}
          onOpenDir={openSummaryDir}
        />

        {showSettings && (
          <SettingsPanel value={settings} disabled={running} onChange={update} />
        )}

        {dlList !== null && (
          <div className="download-overlay">
            <div className="download-card">
              <div className="dl-icon">
                <Download size={26} strokeWidth={1.8} />
              </div>
              <h3>首次启用 · 正在下载模型</h3>
              <p className="dl-sub">
                共 {dlList.length} 个 · 依次下载，完成后自动开始
              </p>
              <ul className="dl-list">
                {dlList.map((it) => (
                  <li key={it.kind} className={`dl-item ${it.status}`}>
                    <div className="dl-item-head">
                      <span className="dl-item-name">{it.label}</span>
                      <span className="dl-item-state">
                        {it.status === "done"
                          ? "完成"
                          : it.status === "pending"
                          ? "排队中"
                          : it.total > 0
                          ? `${it.pct}% · ${(it.downloaded / 1048576).toFixed(0)} / ${(
                              it.total / 1048576
                            ).toFixed(0)} MB`
                          : `已下载 ${(it.downloaded / 1048576).toFixed(0)} MB`}
                      </span>
                    </div>
                    <div className="dl-bar">
                      <div
                        className="dl-fill"
                        style={{
                          width: `${
                            it.status === "done"
                              ? 100
                              : it.status === "pending"
                              ? 0
                              : it.total > 0
                              ? it.pct
                              : 5
                          }%`,
                        }}
                      />
                    </div>
                  </li>
                ))}
              </ul>
              <p className="dl-note">
                模型仅需下载这一次，存到本地后<b>离线即可使用</b>，下次启动无需再下。请保持网络连接，完成后会自动开始。
              </p>
            </div>
          </div>
        )}
      </div>

      {status.state === "starting" && dlList === null && (
        <div className="notice">{status.detail ?? "启动中…"}</div>
      )}

      {status.state === "error" && status.detail && (
        <div className="notice notice-err">
          <AlertTriangle size={14} strokeWidth={2} className="notice-ico" />
          <span>{renderNotice(status.detail)}</span>
        </div>
      )}

      <div className="footer">
        <span className="chip">
          引擎 <span className="k">{TRANSLATION_ENGINE_LABEL[settings.translationEngine]}</span>
        </span>
        {settings.translationEngine === "llm" && translationLlmProfile(settings) && (
          <span className="chip">
            模型 <span className="k">{translationLlmProfile(settings)!.model}</span>
          </span>
        )}
        <span className="chip">
          降噪 <span className="k">{settings.denoise ? "RNNoise" : "关"}</span>
        </span>
        <div className="spacer" />
        <span className="footer-note">音频不出本机 · 仅识别后文本送翻译</span>
      </div>
    </div>
  );
}

export default App;
