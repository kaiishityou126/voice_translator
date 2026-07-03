import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle } from "lucide-react";

import {
  Settings,
  SourceLang,
  AsrEngine,
  TranslationEngine,
  ThemeMode,
  SubtitleScale,
  SOURCE_LANG_LABEL,
  ASR_ENGINE_LABEL,
  TRANSLATION_ENGINE_LABEL,
  THEME_LABEL,
  SUBTITLE_SCALE_LABEL,
} from "../types";

// Qwen3-ASR 官方支持的 30 种语言（另含 22 种中文方言）
const QWEN3_LANGS =
  "中文、英语、粤语、阿拉伯语、德语、法语、西班牙语、葡萄牙语、印尼语、意大利语、韩语、俄语、泰语、越南语、日语、土耳其语、印地语、马来语、荷兰语、瑞典语、丹麦语、芬兰语、波兰语、捷克语、菲律宾语、波斯语、希腊语、匈牙利语、马其顿语、罗马尼亚语";

interface Props {
  value: Settings;
  disabled: boolean;
  onChange: (next: Settings) => void;
}

// 源语言选择器：识别区「源语言」与回灌区「你说的语言」共用。
// Qwen3 多语自动、源语言不参与加载 → 锁定显示「自动检测（多语）」不可选。
function SourceLangSelect({
  value,
  asrEngine,
  disabled,
  onChange,
  label,
}: {
  value: SourceLang;
  asrEngine: AsrEngine;
  disabled: boolean;
  onChange: (l: SourceLang) => void;
  label?: string;
}) {
  const isQwen = asrEngine === "qwen3Asr";
  return (
    <div className="field">
      {label && <label>{label}</label>}
      <select
        value={isQwen ? "auto" : value}
        disabled={disabled || isQwen}
        onChange={(e) => onChange(e.target.value as SourceLang)}
      >
        {isQwen ? (
          <option value="auto">自动检测（多语）</option>
        ) : (
          (["auto", "zh", "en", "ja", "ko", "yue"] as SourceLang[]).map((l) => (
            <option key={l} value={l}>
              {SOURCE_LANG_LABEL[l]}
            </option>
          ))
        )}
      </select>
    </div>
  );
}

// 可折叠区块标题：点击整行收起/展开下方内容（低频区默认收起，减少首屏滚动）。
function CollapseHead({
  title,
  open,
  onToggle,
}: {
  title: string;
  open: boolean;
  onToggle: () => void;
}) {
  return (
    <h3
      className="collapse-head"
      style={{ marginTop: 18 }}
      role="button"
      tabIndex={0}
      onClick={onToggle}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onToggle();
        }
      }}
    >
      {title}
      <span className="caret">{open ? "▾" : "▸"}</span>
    </h3>
  );
}

// ⓘ 说明行：左 ⓘ 图标 + 一行摘要常驻 + 行尾「详情/说明」虚线链接，点开才展长说明。
// 把科普收起来、减少首屏噪音（对比稿 .info-line 模式）。
function InfoLine({
  children,
  detail,
  moreLabel = "详情",
  warn = false,
}: {
  children: ReactNode;
  detail: ReactNode;
  moreLabel?: string;
  warn?: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <p className={`info-line${warn ? " info-line--warn" : ""}`}>
        <span>
          {children}
          <button
            type="button"
            className="more"
            onClick={() => setOpen((o) => !o)}
          >
            {open ? "收起" : moreLabel}
          </button>
        </span>
      </p>
      {open && (
        <p className="hint" style={{ marginTop: -4 }}>
          {detail}
        </p>
      )}
    </>
  );
}

// 警告图标：染成警告色（--warn）的 SVG 三角，放在 .hint--warn / .info-line--warn 里随主题明暗，
// 取代固定色 emoji（主流设计系统的做法：图标染语义色、随明暗）。普通说明不加图标。
function WarnIcon() {
  return (
    <AlertTriangle
      size={13}
      strokeWidth={2}
      aria-hidden
      style={{ flexShrink: 0, verticalAlign: "-2px", marginRight: 4 }}
    />
  );
}

export function SettingsPanel({ value, disabled, onChange }: Props) {
  const set = (patch: Partial<Settings>) => onChange({ ...value, ...patch });
  const [showLangs, setShowLangs] = useState(false);
  const [showDisplay, setShowDisplay] = useState(false);
  const [showArchive, setShowArchive] = useState(false);
  const [showVoiceprint, setShowVoiceprint] = useState(false);
  // 声纹登记状态：是否已登记 / 是否正在录音 / 提示文案
  const [enrolled, setEnrolled] = useState(false);
  const [enrolling, setEnrolling] = useState(false);
  const [enrollMsg, setEnrollMsg] = useState("");

  useEffect(() => {
    invoke<boolean>("voiceprint_exists").then(setEnrolled).catch(() => {});
  }, []);

  const doEnroll = async () => {
    setEnrolling(true);
    setEnrollMsg("录音中，请对着麦克风清晰说 3~5 秒…");
    try {
      await invoke("enroll_voiceprint");
      setEnrolled(true);
      setEnrollMsg("已登记 ✓");
    } catch (e) {
      setEnrollMsg(String(e));
    } finally {
      setEnrolling(false);
    }
  };

  const doClearVoiceprint = async () => {
    try {
      await invoke("clear_voiceprint");
    } catch {
      // 忽略
    }
    setEnrolled(false);
    setEnrollMsg("已清除登记");
  };

  return (
    <div className="settings">
      <div className="settings-head">
        <span className="settings-title">⚙ 设置</span>
      </div>
      <div className="settings-body">
      <h3>语音识别</h3>
      <div className="field">
        <select
          value={value.asrEngine}
          disabled={disabled}
          onChange={(e) => set({ asrEngine: e.target.value as AsrEngine })}
        >
          {(["senseVoice", "qwen3Asr"] as AsrEngine[]).map((l) => (
            <option key={l} value={l}>
              {ASR_ENGINE_LABEL[l]}
            </option>
          ))}
        </select>
      </div>
      {value.asrEngine === "senseVoice" && (
        <p className="hint">
          仅识别 中/英/日/韩/粤 5 种；首次需下载{" "}
          <strong style={{ color: "var(--text)" }}>≈380MB</strong>。
        </p>
      )}
      {value.asrEngine === "qwen3Asr" && (
        <>
          <p className="hint">
            连续大段语音识别更精准、同音词更稳；自动识别{" "}
            <span
              className="inline-more"
              role="button"
              tabIndex={0}
              onClick={() => setShowLangs((v) => !v)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setShowLangs((v) => !v);
                }
              }}
            >
              30 种语言 + 22 种中文方言
            </span>
            。延迟略高。（首次需下载{" "}
            <strong style={{ color: "var(--text)" }}>≈940MB</strong>）
          </p>
          {showLangs && (
            <p className="hint" style={{ marginTop: -4 }}>
              支持语种：{QWEN3_LANGS}；另含 22 种中文方言（安徽、东北、四川、河南、山东…）。
            </p>
          )}
        </>
      )}
      <SourceLangSelect
        label="源语言"
        value={value.sourceLang}
        asrEngine={value.asrEngine}
        disabled={disabled}
        onChange={(l) => set({ sourceLang: l })}
      />
      {value.asrEngine === "qwen3Asr" ? (
        <p className="hint">Qwen3 自动识别多语，无需指定源语言。</p>
      ) : (
        <InfoLine detail="短片段上易把中 / 日 / 英判错，已知就指定，比「自动检测」稳。">
          固定语种比自动检测更准
        </InfoLine>
      )}
      <label className={`toggle-row${disabled ? " is-disabled" : ""}`}>
        <input
          type="checkbox"
          checked={value.denoise}
          disabled={disabled}
          onChange={(e) => set({ denoise: e.target.checked })}
        />
        降噪（RNNoise）—— 嘈杂环境/背景音下提升识别
      </label>
      <label className={`toggle-row${disabled ? " is-disabled" : ""}`}>
        <input
          type="checkbox"
          checked={value.sileroVad}
          disabled={disabled}
          onChange={(e) => set({ sileroVad: e.target.checked })}
        />
        Silero VAD —— 更准的语音切分（关闭则用能量门限）
      </label>

      <h3 style={{ marginTop: 18 }}>翻译</h3>

      <div className="field">
        <select
          value={value.translationEngine}
          disabled={disabled}
          onChange={(e) =>
            set({ translationEngine: e.target.value as TranslationEngine })
          }
        >
          {(
            ["openai", "ollama", "google", "none"] as TranslationEngine[]
          ).map((en) => (
              <option key={en} value={en}>
                {TRANSLATION_ENGINE_LABEL[en]}
              </option>
            )
          )}
        </select>
      </div>

      {value.translationEngine === "openai" && (
        <>
          <div className="field">
            <label>API 接口地址 Base URL</label>
            <input
              value={value.llmBaseUrl}
              disabled={disabled}
              onChange={(e) => set({ llmBaseUrl: e.target.value })}
              placeholder="https://api.openai.com/v1"
            />
          </div>
          <div className="grid2">
            <div className="field">
              <label>API Key</label>
              <input
                type="password"
                value={value.llmApiKey}
                disabled={disabled}
                onChange={(e) => set({ llmApiKey: e.target.value })}
                placeholder="sk-..."
              />
            </div>
            <div className="field">
              <label>模型名 Model</label>
              <input
                value={value.llmModel}
                disabled={disabled}
                onChange={(e) => set({ llmModel: e.target.value })}
                placeholder="gpt-4o-mini"
              />
            </div>
          </div>
          <p className="hint">
            兼容 OpenAI / DeepSeek / 通义千问 / Kimi 等任意 OpenAI 风格端点。质量最好，需 key。
          </p>
        </>
      )}

      {value.translationEngine === "ollama" && (
        <>
          <div className="grid2">
            <div className="field">
              <label>Ollama 地址</label>
              <input
                value={value.ollamaBaseUrl}
                disabled={disabled}
                onChange={(e) => set({ ollamaBaseUrl: e.target.value })}
                placeholder="http://localhost:11434/v1"
              />
            </div>
            <div className="field">
              <label>模型名</label>
              <input
                value={value.ollamaModel}
                disabled={disabled}
                onChange={(e) => set({ ollamaModel: e.target.value })}
                placeholder="qwen2.5"
              />
            </div>
          </div>
          <p className="hint">
            本地运行、免费、无需 key。需先装 Ollama 并拉模型（如 ollama pull qwen2.5）。中英日推荐 qwen 系列。
          </p>
        </>
      )}

      {value.translationEngine === "google" && (
        <p className="hint">
          免费、无需 key，中英日质量好。
          <br />
          <span className="hint--warn">
            <WarnIcon />走的是 Google 的<b>非官方接口</b>，可能限流或偶发失败，仅适合个人使用、不保证长期稳定。
          </span>
        </p>
      )}

      {value.translationEngine === "none" && (
        <p className="hint">
          纯字幕模式：只显示<b>识别原文</b>、不做翻译。无需任何 key，可当离线字幕/转写工具用。
        </p>
      )}

      <div className="settings-divider">以下可在运行中修改</div>

      <CollapseHead title="显示" open={showDisplay} onToggle={() => setShowDisplay((v) => !v)} />
      {showDisplay && (
      <>
      <div className="grid2">
        <div className="field">
          <label>界面主题</label>
          <select
            value={value.theme}
            onChange={(e) => set({ theme: e.target.value as ThemeMode })}
          >
            {(["system", "light", "dark"] as ThemeMode[]).map((t) => (
              <option key={t} value={t}>
                {THEME_LABEL[t]}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label>字幕字号</label>
          <select
            value={value.subtitleScale}
            onChange={(e) => set({ subtitleScale: e.target.value as SubtitleScale })}
          >
            {(["sm", "md", "lg"] as SubtitleScale[]).map((s) => (
              <option key={s} value={s}>
                {SUBTITLE_SCALE_LABEL[s]}
              </option>
            ))}
          </select>
        </div>
      </div>
      <p className="hint">
        悬浮窗的字号与透明度直接在悬浮窗上调（鼠标悬停时左上角出现控件）。
      </p>
      </>
      )}

      <CollapseHead title="存档与提炼" open={showArchive} onToggle={() => setShowArchive((v) => !v)} />
      {showArchive && (
      <>
      <div className="grid2">
        <div className="field">
          <label>译文保留</label>
          <div className="input-suffix">
            <input
              type="number"
              min={0}
              value={value.transcriptKeepDays}
              disabled={disabled}
              onChange={(e) =>
                set({ transcriptKeepDays: Math.max(0, Number(e.target.value) || 0) })
              }
            />
            <span>天</span>
          </div>
        </div>
        <div className="field">
          <label>提炼重点保留</label>
          <div className="input-suffix">
            <input
              type="number"
              min={0}
              value={value.summaryKeepDays}
              disabled={disabled}
              onChange={(e) =>
                set({ summaryKeepDays: Math.max(0, Number(e.target.value) || 0) })
              }
            />
            <span>天</span>
          </div>
        </div>
      </div>
      <InfoLine
        detail={
          <>
            每次「开始→停止」的译文会存为一个文件；点顶栏「✦ 提炼重点」用 LLM 总结本次会话要点（仅 OpenAI / Ollama 引擎）会保留一份摘要文件；启动时自动清理超过保留天数的文件。如果填 <b>0 或留空</b> 表示永久保留。
          </>
        }
      >
        译文、提炼重点文件的保留天数，到期自动清理；
      </InfoLine>

      <div className="grid2">
        <div className="field">
          <label>单次提炼上下文</label>
          <div className="input-suffix">
            <input
              type="text"
              inputMode="numeric"
              value={Math.round(value.summaryMaxContext / 1000) || ""}
              disabled={disabled}
              onChange={(e) => {
                const digits = e.target.value.replace(/\D/g, "").slice(0, 4);
                set({ summaryMaxContext: (parseInt(digits, 10) || 0) * 1000 });
              }}
            />
            <span>k</span>
          </div>
        </div>
      </div>
      <InfoLine
        detail={
          <>
            默认 <b>10k</b>。会话很长时会自动拆成几段提炼再合并：数字越大拆得越少、越连贯，但小模型吃太多会跑偏、丢内容；付费旗舰大模型可调到 30k+ 一次吃更多、更省请求。
          </>
        }
      >
        控制每次喂给模型多少字，越大越连贯、小模型调高易跑偏
      </InfoLine>
      </>
      )}

      <CollapseHead
        title="说话人门控（同传）"
        open={showVoiceprint}
        onToggle={() => setShowVoiceprint((v) => !v)}
      />
      {showVoiceprint && (
      <div className="vp-section">
      <label className={`toggle-row${disabled ? " is-disabled" : ""}`}>
        <input
          type="checkbox"
          checked={value.voiceprintGate}
          disabled={disabled}
          onChange={(e) => set({ voiceprintGate: e.target.checked })}
        />
        <span>只识别「我的声音」，挡掉旁人（同传出站）</span>
      </label>
      <div className="field">
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            type="button"
            className="btn-secondary"
            disabled={disabled || enrolling}
            onClick={doEnroll}
          >
            {enrolling ? "录音中…" : enrolled ? "重新登记声音" : "登记我的声音"}
          </button>
          {enrolled && !enrolling && (
            <button
              type="button"
              className="btn-secondary"
              disabled={disabled}
              onClick={doClearVoiceprint}
            >
              清除
            </button>
          )}
          <span style={{ fontSize: 12, opacity: 0.75 }}>
            {enrollMsg || (enrolled ? "已登记 ✓" : "未登记")}
          </span>
        </div>
      </div>
      <div className="field vp-slider">
        <label>相似度阈值 {value.voiceprintThreshold.toFixed(2)}</label>
        <input
          type="range"
          min={0.2}
          max={0.8}
          step={0.05}
          value={value.voiceprintThreshold}
          disabled={disabled}
          onChange={(e) => set({ voiceprintThreshold: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
            仅在<b>回灌·双向同传</b>时生效：出站每段先比对你登记的声纹，不像你的（旁人、两桌外的人）直接丢弃，不识别也不合成。需先<b>登记声音</b>并开启开关；登记要在同传<b>未启动</b>时进行。阈值越高越严，误挡自己就调低、漏进旁人就调高（默认 0.50）。
          </>
        }
      >
        登记你的声纹后，出站只放行你本人，挡掉旁人说话
      </InfoLine>
      </div>
      )}
      </div>
    </div>
  );
}
