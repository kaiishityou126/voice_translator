import { useState } from "react";
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

// 说明里重点文字统一用浅蓝色
const EMPH = "#7cb9e8";

// Qwen3-ASR 官方支持的 30 种语言（另含 22 种中文方言）
const QWEN3_LANGS =
  "中文、英语、粤语、阿拉伯语、德语、法语、西班牙语、葡萄牙语、印尼语、意大利语、韩语、俄语、泰语、越南语、日语、土耳其语、印地语、马来语、荷兰语、瑞典语、丹麦语、芬兰语、波兰语、捷克语、菲律宾语、波斯语、希腊语、匈牙利语、马其顿语、罗马尼亚语";

interface Props {
  value: Settings;
  disabled: boolean;
  onChange: (next: Settings) => void;
}

export function SettingsPanel({ value, disabled, onChange }: Props) {
  const set = (patch: Partial<Settings>) => onChange({ ...value, ...patch });
  const [showLangs, setShowLangs] = useState(false);

  return (
    <div className="settings">
      <div className="settings-head">
        <span className="settings-title">⚙ 设置</span>
      </div>
      <div className="settings-body">
      <h3>语音识别（本地）</h3>
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
          识别快速、延迟低；仅支持{" "}
          <strong style={{ color: EMPH }}>中 / 英 / 日 / 韩 / 粤 5 种语言</strong>
          ，日常字幕够用；专名（人名、地名）准确度一般。（
          <strong style={{ color: EMPH }}>首次需下载 ≈380MB</strong>）
        </p>
      )}
      {value.asrEngine === "qwen3Asr" && (
        <>
          <p className="hint">
            识别更准 —— 专名（人名、地名）、同音词更稳；自动识别{" "}
            <strong
              role="button"
              tabIndex={0}
              onClick={() => setShowLangs((v) => !v)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setShowLangs((v) => !v);
                }
              }}
              style={{
                color: EMPH,
                textDecoration: "underline",
                cursor: "pointer",
              }}
            >
              30 种语言 + 22 种中文方言
            </strong>
            。延迟略高。（
            <strong style={{ color: EMPH }}>首次需下载 ≈940MB</strong>）
          </p>
          {showLangs && (
            <p className="hint" style={{ marginTop: -4 }}>
              支持语种：{QWEN3_LANGS}；另含 22 种中文方言（安徽、东北、四川、河南、山东…）。
            </p>
          )}
        </>
      )}
      <h3 style={{ marginTop: 18 }}>源语言（说话人语言）</h3>
      <div className="field">
        <select
          value={value.asrEngine === "qwen3Asr" ? "auto" : value.sourceLang}
          disabled={disabled || value.asrEngine === "qwen3Asr"}
          onChange={(e) => set({ sourceLang: e.target.value as SourceLang })}
        >
          {value.asrEngine === "qwen3Asr" ? (
            <option value="auto">自动（多语）</option>
          ) : (
            (["auto", "zh", "en", "ja", "ko", "yue"] as SourceLang[]).map((l) => (
              <option key={l} value={l}>
                {SOURCE_LANG_LABEL[l]}
              </option>
            ))
          )}
        </select>
      </div>
      {value.asrEngine === "qwen3Asr" ? (
        <p className="hint">Qwen3 自动识别多语，无需指定源语言。</p>
      ) : (
        <p className="hint">
          固定语种比「自动检测」更准（短片段上易把中/日/英判错），已知就指定。
        </p>
      )}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          fontSize: 13,
          margin: "2px 0 8px",
        }}
      >
        <input
          type="checkbox"
          checked={value.denoise}
          disabled={disabled}
          onChange={(e) => set({ denoise: e.target.checked })}
          style={{ width: "auto", cursor: disabled ? "not-allowed" : "pointer" }}
        />
        降噪（RNNoise）—— 喻杂环境/背景音下提升识别
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          fontSize: 13,
          margin: "2px 0 8px",
        }}
      >
        <input
          type="checkbox"
          checked={value.sileroVad}
          disabled={disabled}
          onChange={(e) => set({ sileroVad: e.target.checked })}
          style={{ width: "auto", cursor: disabled ? "not-allowed" : "pointer" }}
        />
        Silero VAD —— 更准的语音切分（关闭则用能量门限）
      </div>

      <h3 style={{ marginTop: 18 }}>翻译引擎</h3>

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
            本地运行、免费、无需 key。需先装 <b>Ollama</b> 并拉模型（如 <code>ollama pull qwen2.5</code>）。中英日推荐 qwen 系列。
          </p>
        </>
      )}

      {value.translationEngine === "google" && (
        <p className="hint">
          免费、无需 key，中英日质量好。
          <br />
          ⚠️ 走的是 Google 的<b>非官方接口</b>，可能限流或偶发失败，仅适合个人使用、不保证长期稳定。
        </p>
      )}

      {value.translationEngine === "none" && (
        <p className="hint">
          纯字幕模式：只显示<b>识别原文</b>、不做翻译。无需任何 key，可当离线字幕/转写工具用。
        </p>
      )}

      <h3 style={{ marginTop: 18 }}>显示</h3>
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

      <h3 style={{ marginTop: 18 }}>存档文件</h3>
      <div className="grid2">
        <div className="field">
          <label>译文保留天数</label>
          <input
            type="number"
            min={0}
            value={value.transcriptKeepDays}
            disabled={disabled}
            onChange={(e) =>
              set({ transcriptKeepDays: Math.max(0, Number(e.target.value) || 0) })
            }
          />
        </div>
        <div className="field">
          <label>提炼重点保留天数</label>
          <input
            type="number"
            min={0}
            value={value.summaryKeepDays}
            disabled={disabled}
            onChange={(e) =>
              set({ summaryKeepDays: Math.max(0, Number(e.target.value) || 0) })
            }
          />
        </div>
      </div>
      <p className="hint">
        每次「开始→停止」的双语译文存为一个会话文件；启动时自动清理超过保留天数的文件。
        两个天数都填 <b>0 或留空</b> 表示永久保留。
        点顶栏「✦ 提炼重点」用 LLM 总结本次会话要点（仅 OpenAI / Ollama 引擎）。
      </p>

      <h3 style={{ marginTop: 18 }}>重点提炼</h3>
      <div className="grid2">
        <div className="field">
          <label>单次提炼上下文（k）</label>
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
        </div>
      </div>
      <p className="hint">
        控制每次喂给模型多少字。会话很长时会自动拆成几段提炼再合并：数字越大拆得越少、越连贯，但小模型吃太多会跑偏、丢内容。
        默认 <b>10k</b>，小模型(免费款)别调高；用付费旗舰大模型可调到 30k+ 一次吃更多、更省请求。
      </p>      </div>
    </div>
  );
}
