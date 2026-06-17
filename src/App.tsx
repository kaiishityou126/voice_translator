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
} from "lucide-react";
import {
  Settings,
  SourceKind,
  TargetLang,
  SubtitleEvent,
  StatusEvent,
  SummaryEvent,
  SummaryFile,
  TARGET_LANG_LABEL,
  TRANSLATION_ENGINE_LABEL,
  DEFAULT_SETTINGS,
  loadSettings,
  saveSettings,
} from "./types";
import { SubtitleView } from "./components/SubtitleView";
import { SettingsPanel } from "./components/SettingsPanel";
import { SummaryPanel } from "./components/SummaryPanel";
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
  const [showSettings, setShowSettings] = useState(true);
  const [overlayOn, setOverlayOn] = useState(false);
  const [downloadPct, setDownloadPct] = useState<number | null>(null);
  const [vad, setVad] = useState<string | null>(null);
  // 重点提炼：面板可见性、流式文本、是否生成中
  const [summaryOpen, setSummaryOpen] = useState(false);
  const [summary, setSummary] = useState("");
  // 重点面板是否处于错误/提示态（非正常摘要内容），用于整段标红
  const [summaryError, setSummaryError] = useState(false);
  const [summarizing, setSummarizing] = useState(false);
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
        return [...prev.slice(-199), e.payload];
      });
    }).then((u) => unsubs.push(u));
    listen<StatusEvent>("status", (e) => {
      setStatus(e.payload);
      // 跟随真实事件流：error/idle → 未运行；活跃状态 → 运行中（与悬浮窗对称，防止被迟到事件卡死）
      const s = e.payload.state;
      if (s === "error" || s === "idle") setRunning(false);
      else setRunning(true); // listening/recognizing/translating
    }).then((u) => unsubs.push(u));
    listen<{ pct: number }>("model-progress", (e) => {
      setDownloadPct(e.payload.pct);
    }).then((u) => unsubs.push(u));
    listen<{ vad: string }>("engine-info", (e) => {
      setVad(e.payload.vad);
    }).then((u) => unsubs.push(u));
    listen<SummaryEvent>("summary", (e) => {
      setSummary(e.payload.text);
      setSummaryError(false);
      setSummarizing(e.payload.pending);
    }).then((u) => unsubs.push(u));
    listen("overlay-start", () => startRef.current()).then((u) => unsubs.push(u));
    return () => unsubs.forEach((u) => u());
  }, []);

  // 点击面板外部任意位置即关闭设置 / 重点面板（排除各自的顶栏触发按钮；编辑重点时不关闭以防丢草稿）
  useEffect(() => {
    if (!showSettings && !summaryOpen) return;
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
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [showSettings, summaryOpen]);

  const update = (next: Settings) => {
    userTouched.current = true;
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
    // 开始新一轮前清空上一轮的字幕，避免新旧记录混在一起
    setSubtitles([]);
    // 模型按需下载：首次使用时先下载 SenseVoice 模型
    try {
      const has = await invoke<boolean>("model_exists");
      if (!has) {
        setStatus({ state: "starting", detail: "首次使用，正在下载语音识别模型…" });
        setDownloadPct(0);
        await invoke("download_model");
        setDownloadPct(null);
      }
    } catch (e) {
      setStatus({ state: "error", detail: `模型下载失败：${String(e)}` });
      setDownloadPct(null);
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
        : raw.includes("屏幕录制")
        ? `${raw}\n（在「系统设置 → 隐私与安全性 → 屏幕录制」勾选本应用后，需重启应用生效）`
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

  async function toggleOverlay() {
    try {
      const on = await invoke<boolean>("toggle_overlay");
      setOverlayOn(on);
      // 刚打开悬浮框时广播一次当前状态，让其按钮（▶/■）立即与主窗口对齐
      if (on) {
        emit("status", running ? status : { state: "idle" }).catch(() => {});
      }
    } catch (e) {
      setStatus({ state: "error", detail: String(e) });
    }
  }

  // 焦点行状态文字 + VAD 标签（替代原导航栏状态块）
  const liveState = running ? LIVE_STATE_LABEL[status.state] ?? "" : "";
  const vadLabel = vad ? (vad === "silero" ? "Silero" : "能量门限") : "";

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

        {/* 音频来源 */}
        <div className="seg" title="音频来源">
          {(["loopback", "microphone"] as SourceKind[]).map((s) => (
            <button
              key={s}
              className={settings.source === s ? "active" : ""}
              disabled={running}
              onClick={() => update({ ...settings, source: s })}
            >
              {s === "loopback" ? "系统音频" : "麦克风"}
            </button>
          ))}
        </div>

        {/* 目标语言 */}
        <div className="seg" title="翻译目标语言">
          {(["zh", "en", "ja"] as TargetLang[]).map((l) => (
            <button
              key={l}
              className={settings.targetLang === l ? "active" : ""}
              disabled={running}
              onClick={() => update({ ...settings, targetLang: l })}
            >
              {TARGET_LANG_LABEL[l]}
            </button>
          ))}
        </div>

        <div className="spacer" />

        <div className="icon-group">
          <button
            className={`iconbtn${overlayOn ? " active" : ""}`}
            onClick={toggleOverlay}
            title={overlayOn ? "关闭悬浮窗" : "悬浮窗"}
          >
            <PictureInPicture2 size={17} strokeWidth={1.8} />
          </button>

          <button
            className={`iconbtn${summarizing ? " busy" : ""}`}
            onClick={summarize}
            disabled={summarizing}
            data-toggle="summary"
            title={summarizing ? "提炼中…" : "提炼本次会话重点（已有重点文件则直接显示）"}
          >
            <Sparkles size={17} strokeWidth={1.8} />
          </button>

          <button className="iconbtn" onClick={openSummaryDir} title="打开译文存档文件夹">
            <History size={17} strokeWidth={1.8} />
          </button>

          <button
            className={`iconbtn${showSettings ? " active" : ""}`}
            title="设置"
            data-toggle="settings"
            onClick={() =>
              setShowSettings((v) => {
                const next = !v;
                if (next) setSummaryOpen(false);
                return next;
              })
            }
          >
            <SettingsIcon size={17} strokeWidth={1.8} />
          </button>
        </div>

        {running ? (
          <button className="btn btn-stop btn-icon" onClick={stop} title="停止">
            <Square size={16} fill="currentColor" strokeWidth={0} />
          </button>
        ) : (
          <button className="btn btn-start btn-icon" onClick={start} title="开始">
            <Play size={16} fill="currentColor" strokeWidth={0} />
          </button>
        )}
      </div>

      <div className="app-main">
        <SubtitleView
          items={subtitles}
          targetLang={settings.targetLang}
          liveState={liveState}
          vadLabel={vadLabel}
        />

        {(showSettings || summaryOpen) && <div className="drawer-scrim" />}

        <SummaryPanel
          text={summary}
          error={summaryError}
          pending={summarizing}
          visible={summaryOpen}
          canResummarize={engineIsLLM}
          onResummarize={resummarize}
          onSave={saveSummary}
        />

        {showSettings && (
          <SettingsPanel value={settings} disabled={running} onChange={update} />
        )}
      </div>

      {downloadPct !== null && (
        <div className="notice">
          ⬇ 正在下载语音模型… {downloadPct}%（仅首次，存到本地后无需再下）
        </div>
      )}

      {status.state === "starting" && downloadPct === null && (
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
