import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, emit } from "@tauri-apps/api/event";
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
} from "lucide-react";
import {
  Settings,
  SourceKind,
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
} from "./types";
import { SubtitleView } from "./components/SubtitleView";
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

function App() {
  const [settings, setSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [running, setRunning] = useState(false);
  const [status, setStatus] = useState<StatusEvent>({ state: "idle" });
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
  const [download, setDownload] = useState<{ pct: number; downloaded: number; total: number } | null>(null);
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
  // 用户是否已动过设置：防止 mount 后异步加载的存档把刚改的值盖回去
  const userTouched = useRef(false);

  // 启动时从磁盘加载已保存的设置（异步，故不在 useState 初始化里做）
  useEffect(() => {
    loadSettings().then((s) => {
      if (!userTouched.current) setSettings(s);
    });
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
      // 跟随真实事件流：error/idle → 未运行；活跃状态 → 运行中（与悬浮窗对称，防止被迟到事件卡死）
      const s = e.payload.state;
      if (s === "error" || s === "idle") setRunning(false);
      else setRunning(true); // listening/recognizing/translating
    }).then((u) => unsubs.push(u));
    listen<{ pct: number; downloaded: number; total: number }>("model-progress", (e) => {
      setDownload(e.payload);
    }).then((u) => unsubs.push(u));
    listen<{ vad: string }>("engine-info", (e) => {
      setVad(e.payload.vad);
    }).then((u) => unsubs.push(u));
    listen<{ lang: string }>("language-detected", (e) => {
      setDetectedLang(e.payload.lang);
    }).then((u) => unsubs.push(u));
    listen<SummaryEvent>("summary", (e) => {
      setSummary(e.payload.text);
      setSummaryError(false);
      setSummarizing(e.payload.pending);
    }).then((u) => unsubs.push(u));
    listen("overlay-start", () => startRef.current()).then((u) => unsubs.push(u));
    // 悬浮窗挂载后请求一次当前显示配置(它启动时拿不到主窗最新设置)
    listen("overlay-ready", () => {
      emit("overlay-config", pickOverlayConfig(settingsRef.current)).catch(() => {});
    }).then((u) => unsubs.push(u));
    // 悬浮窗上的字号/透明度控件改动 → 回写主窗口设置并落盘(单一数据源仍在主窗口)
    listen<Partial<Settings>>("overlay-config-change", (e) => {
      update({ ...settingsRef.current, ...e.payload });
    }).then((u) => unsubs.push(u));
    return () => unsubs.forEach((u) => u());
  }, []);

  // 点击面板外部任意位置即关闭设置 / 重点面板（排除各自的顶栏触发按钮；编辑重点时不关闭以防丢草稿）
  useEffect(() => {
    if (!showSettings && !summaryOpen && !historyOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (!t) return;
      if (
        showSettings &&
        !t.closest(".settings") &&
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
    setSettings(next);
    saveSettings(next);
  };

  async function start() {
    const eng = settings.translationEngine;
    if (eng === "openai" && !settings.llmApiKey.trim()) {
      setStatus({
        state: "error",
        detail: "OpenAI 接口需要 API Key（或改用 Google 免费 / 纯字幕）",
      });
      setShowSettings(true);
      return;
    }
    // 退出历史回看（若在），回到直播态
    setReview(null);
    // 开始新一轮前清空上一轮的字幕，避免新旧记录混在一起
    setSubtitles([]);
    // 清空上一轮的语种检测结果（auto 模式下后端会重新检测）
    setDetectedLang(null);
    // 模型按需下载：首次使用时先下载所选引擎的模型
    try {
      const has = await invoke<boolean>("model_exists", { engine: settings.asrEngine });
      if (!has) {
        const big = settings.asrEngine === "qwen3Asr";
        setStatus({
          state: "starting",
          detail: big
            ? "首次使用 Qwen3,正在下载语音识别模型(≈940MB,请耐心等待)…"
            : "首次使用，正在下载语音识别模型…",
        });
        setDownload({ pct: 0, downloaded: 0, total: 0 });
        await invoke("download_model", { engine: settings.asrEngine });
        setDownload(null);
      }
    } catch (e) {
      setStatus({ state: "error", detail: `模型下载失败：${String(e)}` });
      setDownload(null);
      return;
    }
    setStatus({ state: "starting", detail: "加载模型，请稍候…" });
    setRunning(true);
    try {
      await invoke("start_translation", { config: settings });
    } catch (err) {
      const raw = String(err);
      const detail = raw.includes("MODEL_MISSING")
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

  const engineIsLLM =
    settings.translationEngine === "openai" || settings.translationEngine === "ollama";

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
      const f = await invoke<SummaryFile>("load_summary", { targetLang: settings.targetLang });
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
      if (!engineIsLLM) {
        showSummaryError("尚未配置 LLM 引擎（OpenAI / Ollama），无法提炼重点。");
        return;
      }
      setSummary("");
      setSummaryError(false);
      setSummarizing(true);
      await invoke("summarize_session", { config: settings });
    } catch (e) {
      showSummaryError(`提炼失败：${String(e)}`);
    }
  }

  // 重新提炼：强制再次调 LLM 覆盖摘要文件
  async function resummarize() {
    if (!engineIsLLM) {
      showSummaryError("尚未配置 LLM 引擎（OpenAI / Ollama），无法提炼重点。");
      return;
    }
    setSummary("");
    setSummaryError(false);
    setSummarizing(true);
    try {
      await invoke("summarize_session", { config: settings });
    } catch (e) {
      showSummaryError(`提炼失败：${String(e)}`);
    }
  }

  // 保存手动编辑后的重点内容，写回摘要文件
  async function saveSummary(content: string) {
    setSummary(content);
    setSummaryError(false);
    try {
      await invoke("save_summary", { content, targetLang: settings.targetLang });
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
        config: settings,
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
  const liveState = running ? LIVE_STATE_LABEL[status.state] ?? "" : "";
  const vadLabel = vad ? (vad === "silero" ? "Silero" : "能量门限") : "";
  // 仅 auto 模式展示自动检测出的语种（手动选语种时不显示，避免冗余）
  const detectedLangLabel =
    settings.sourceLang === "auto" && detectedLang
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
            浮层内日后可再加「输出设备(倒灌 CABLE)」分组，无需占主栏横向空间 */}
        <div className="topmenu src-menu" title="音频来源">
          <button
            className={`topmenu-trigger${srcMenuOpen ? " open" : ""}`}
            disabled={running}
            onClick={() => {
              setSrcMenuOpen((v) => !v);
              setDispMenuOpen(false);
              setLangMenuOpen(false);
            }}
          >
            {settings.source === "loopback" ? (
              <Volume2 size={15} strokeWidth={1.9} />
            ) : (
              <Mic size={15} strokeWidth={1.9} />
            )}
            <span>{settings.source === "loopback" ? "系统音频" : "麦克风"}</span>
            <ChevronDown size={13} strokeWidth={2} />
          </button>
          {srcMenuOpen && !running && (
            <div className="lang-menu">
              <div className="menu-label">音频来源</div>
              {(["loopback", "microphone"] as SourceKind[]).map((s) => (
                <button
                  key={s}
                  className={settings.source === s ? "active" : ""}
                  onClick={() => {
                    update({ ...settings, source: s });
                    setSrcMenuOpen(false);
                  }}
                >
                  {s === "loopback" ? "系统音频（环回）" : "麦克风"}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* 目标语言：高频三个一键 segmented + 「更多」下拉收纳长尾语种（运行中可热切） */}
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
          canResummarize={engineIsLLM}
          onResummarize={resummarize}
          onSave={saveSummary}
          onOpenDir={openSummaryDir}
        />

        {showSettings && (
          <SettingsPanel value={settings} disabled={running} onChange={update} />
        )}

        {download !== null && (
          <div className="download-overlay">
            <div className="download-card">
              <div className="dl-icon">
                <Download size={26} strokeWidth={1.8} />
              </div>
              <h3>首次启用 · 正在下载语音识别模型</h3>
              <p className="dl-sub">
                {settings.asrEngine === "qwen3Asr"
                  ? "Qwen3 识别模型 · 约 940MB"
                  : "SenseVoice 识别模型 · 约 240MB"}
              </p>
              <div className="dl-bar">
                <div
                  className="dl-fill"
                  style={{ width: `${download.total > 0 ? download.pct : 5}%` }}
                />
              </div>
              <div className="dl-stat">
                {download.total > 0
                  ? `${download.pct}% · ${(download.downloaded / 1048576).toFixed(0)} / ${(
                      download.total / 1048576
                    ).toFixed(0)} MB`
                  : `已下载 ${(download.downloaded / 1048576).toFixed(0)} MB`}
              </div>
              <p className="dl-note">
                模型仅需下载这一次，存到本地后<b>离线即可使用</b>，下次启动无需再下。请保持网络连接，完成后会自动开始。
              </p>
            </div>
          </div>
        )}
      </div>

      {status.state === "starting" && download === null && (
        <div className="notice">{status.detail ?? "启动中…"}</div>
      )}

      {status.state === "error" && status.detail && (
        <div className="notice notice-err">{status.detail}</div>
      )}

      <div className="footer">
        <span className="chip">
          引擎 <span className="k">{TRANSLATION_ENGINE_LABEL[settings.translationEngine]}</span>
        </span>
        {settings.translationEngine === "openai" && (
          <span className="chip">
            模型 <span className="k">{settings.llmModel}</span>
          </span>
        )}
        {settings.translationEngine === "ollama" && (
          <span className="chip">
            模型 <span className="k">{settings.ollamaModel}</span>
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
