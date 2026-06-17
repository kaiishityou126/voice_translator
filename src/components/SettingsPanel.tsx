import {
  Settings,
  SourceLang,
  TranslationEngine,
  SOURCE_LANG_LABEL,
  TRANSLATION_ENGINE_LABEL,
} from "../types";

interface Props {
  value: Settings;
  disabled: boolean;
  onChange: (next: Settings) => void;
}

export function SettingsPanel({ value, disabled, onChange }: Props) {
  const set = (patch: Partial<Settings>) => onChange({ ...value, ...patch });

  return (
    <div className="settings">
      <div className="settings-head">
        <span className="settings-title">⚙ 设置</span>
      </div>
      <div className="settings-body">
      <h3>语音识别（本地）</h3>
      <div className="field">
        <label>源语言（说话人语言）</label>
        <select
          value={value.sourceLang}
          disabled={disabled}
          onChange={(e) => set({ sourceLang: e.target.value as SourceLang })}
        >
          {(["auto", "zh", "en", "ja", "ko", "yue"] as SourceLang[]).map((l) => (
            <option key={l} value={l}>
              {SOURCE_LANG_LABEL[l]}
            </option>
          ))}
        </select>
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
      <p className="hint">
        固定语种比「自动检测」更准（短片段上易把中/日/英判错），已知就指定。
        首次使用会自动下载识别模型（SenseVoice，约 239MB）。
      </p>

      <h3 style={{ marginTop: 18 }}>翻译</h3>

      <div className="field">
        <label>翻译引擎</label>
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
      </div>
    </div>
  );
}
