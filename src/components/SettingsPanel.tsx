import { useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle } from "lucide-react";

import {
  Settings,
  SourceLang,
  AsrEngine,
  TranslationEngine,
  LlmProfile,
  ThemeMode,
  SubtitleScale,
  SOURCE_LANG_LABEL,
  ASR_ENGINE_LABEL,
  TRANSLATION_ENGINE_LABEL,
  THEME_LABEL,
  SUBTITLE_SCALE_LABEL,
  newLlmId,
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

// 显示名：同一个 model 有多条时按出现顺序编号（第 1 条=模型名，其余加 (n)），
// 避免同模型多配置在下拉里分不清。动态计算（不落库）：删除后自动重排，不留空号。
function modelDisplayName(p: LlmProfile, all: LlmProfile[]): string {
  const model = p.model || "?";
  const same = all.filter((x) => x.model === p.model);
  if (same.length <= 1) return model;
  const idx = same.findIndex((x) => x.id === p.id);
  return idx <= 0 ? model : `${model}(${idx + 1})`;
}

// LLM 模型新增/编辑弹窗（自带表单状态，保存时回传整条）
function LlmEditModal({
  initial,
  exists,
  onSave,
  onCancel,
}: {
  initial: LlmProfile;
  exists: boolean;
  onSave: (p: LlmProfile) => void;
  onCancel: () => void;
}) {
  const [p, setP] = useState<LlmProfile>(initial);
  const upd = (patch: Partial<LlmProfile>) => setP((prev) => ({ ...prev, ...patch }));
  const OLLAMA_DEFAULT = "http://localhost:11434/v1";
  const setLocal = (local: boolean) =>
    setP((prev) => {
      let baseUrl = prev.baseUrl;
      if (local) {
        // 切本地：仅当空时补默认地址，不覆盖用户已填
        if (!baseUrl) baseUrl = OLLAMA_DEFAULT;
      } else if (baseUrl === OLLAMA_DEFAULT) {
        // 切云端：若当前正好是 Ollama 默认地址则清空，露出云端 placeholder（云端无普适默认）
        baseUrl = "";
      }
      return { ...prev, isLocal: local, baseUrl };
    });
  return (
    <div className="llm-mask" onClick={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="llm-modal llm-modal--narrow">
        <div className="llm-modal-head">
          <h3>{exists ? "编辑模型" : "新增模型"}</h3>
          <button type="button" className="llm-modal-x" onClick={onCancel}>
            ×
          </button>
        </div>
        <div className="field">
          <label>类型</label>
          <div className="llm-type-seg">
            <button type="button" className={!p.isLocal ? "on" : ""} onClick={() => setLocal(false)}>
              云端（OpenAI 兼容）
            </button>
            <button type="button" className={p.isLocal ? "on" : ""} onClick={() => setLocal(true)}>
              本地 Ollama
            </button>
          </div>
        </div>
        <div className="field">
          <label>接口地址 Base URL</label>
          <input value={p.baseUrl} onChange={(e) => upd({ baseUrl: e.target.value })} placeholder="如：https://api.openai.com/v1" />
        </div>
        {!p.isLocal && (
          <div className="field">
            <label>API Key</label>
            <input type="password" value={p.apiKey} onChange={(e) => upd({ apiKey: e.target.value })} placeholder="如：sk-..." />
          </div>
        )}
        <div className="field">
          <label>模型名 Model</label>
          <input value={p.model} onChange={(e) => upd({ model: e.target.value })} placeholder="如：gpt-4o-mini" />
        </div>
        <div className="field">
          <label>模型用途（决定它出现在哪个下拉）</label>
          <div className="llm-caps">
            <label className="llm-cap">
              <input type="checkbox" checked={p.capTranslation} onChange={(e) => upd({ capTranslation: e.target.checked })} />
              用于翻译
            </label>
            <label className="llm-cap">
              <input type="checkbox" checked={p.capSummary} onChange={(e) => upd({ capSummary: e.target.checked })} />
              用于提炼
            </label>
          </div>
        </div>
        <div className="llm-modal-foot">
          <button type="button" className="llm-btn" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="llm-btn llm-btn--primary" onClick={() => onSave(p)}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
}

export function SettingsPanel({ value, disabled, onChange }: Props) {
  const set = (patch: Partial<Settings>) => onChange({ ...value, ...patch });
  const [showLangs, setShowLangs] = useState(false);
  const [showDisplay, setShowDisplay] = useState(false);
  const [showArchive, setShowArchive] = useState(false);
  const [showVoiceprint, setShowVoiceprint] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  // 声纹登记状态：是否已登记 / 是否正在录音 / 提示文案
  const [enrolled, setEnrolled] = useState(false);
  const [enrolling, setEnrolling] = useState(false);
  const [enrollMsg, setEnrollMsg] = useState("");

  // LLM 模型库弹窗 + 编辑弹窗（editProfile 非空 = 正在新增/编辑）
  const [libOpen, setLibOpen] = useState(false);
  const [editProfile, setEditProfile] = useState<LlmProfile | null>(null);

  // 翻译引擎归一：旧的 openai/ollama 统一显示为 llm（下拉只有 llm/google/none）
  const engineValue: TranslationEngine =
    value.translationEngine === "google" || value.translationEngine === "none"
      ? value.translationEngine
      : "llm";
  const transProfiles = value.llmProfiles.filter((p) => p.capTranslation);
  const sumProfiles = value.llmProfiles.filter((p) => p.capSummary);
  const profileLabel = (p: LlmProfile) =>
    `${p.isLocal ? "Ollama" : "云端"}·${modelDisplayName(p, value.llmProfiles)}`;

  const upsertProfile = (p: LlmProfile) =>
    set({
      llmProfiles: value.llmProfiles.some((x) => x.id === p.id)
        ? value.llmProfiles.map((x) => (x.id === p.id ? p : x))
        : [...value.llmProfiles, p],
    });
  const deleteProfile = (id: string) =>
    set({
      llmProfiles: value.llmProfiles.filter((x) => x.id !== id),
      translationLlmId: value.translationLlmId === id ? "" : value.translationLlmId,
      summaryLlmId: value.summaryLlmId === id ? "" : value.summaryLlmId,
    });
  const toggleCap = (id: string, cap: "capTranslation" | "capSummary", on: boolean) =>
    set({
      llmProfiles: value.llmProfiles.map((x) => (x.id === id ? { ...x, [cap]: on } : x)),
      // 取消能力时，若该模型正被指派为对应角色，一并清掉指派（避免“选中但不可用”的矛盾）
      ...(!on && cap === "capTranslation" && value.translationLlmId === id
        ? { translationLlmId: "" }
        : {}),
      ...(!on && cap === "capSummary" && value.summaryLlmId === id ? { summaryLlmId: "" } : {}),
    });

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
          value={engineValue}
          disabled={disabled}
          onChange={(e) => set({ translationEngine: e.target.value as TranslationEngine })}
        >
          {(["llm", "google", "none"] as TranslationEngine[]).map((en) => (
            <option key={en} value={en}>
              {TRANSLATION_ENGINE_LABEL[en]}
            </option>
          ))}
        </select>
      </div>

      {engineValue === "llm" && (
        <p className="hint">
          云端或本地大模型，翻译质量最好、多语最稳。在下方选择/管理模型。
        </p>
      )}
      {engineValue === "google" && (
        <p className="hint">
          免费、无需 key，中英日质量好。
          <br />
          <span className="hint--warn">
            <WarnIcon />走的是 Google 的<b>非官方接口</b>，可能限流或偶发失败，仅适合个人使用、不保证长期稳定。
          </span>
        </p>
      )}
      {engineValue === "none" && (
        <p className="hint">
          纯字幕模式：只显示<b>识别原文</b>、不做翻译。无需任何 key，可当离线字幕/转写工具用。
        </p>
      )}

      {engineValue === "llm" && (
        <div className="field">
          <div className="llm-pick-head">
            <label>翻译用模型</label>
            <button
              type="button"
              className="llm-btn"
              disabled={disabled}
              onClick={() => setLibOpen(true)}
            >
              ⚙ 模型库管理
            </button>
          </div>
          <select
            value={value.translationLlmId}
            disabled={disabled}
            onChange={(e) => set({ translationLlmId: e.target.value })}
          >
            {transProfiles.length === 0 ? (
              <option value="">（库里没有「可翻译」的模型）</option>
            ) : (
              <>
                {!transProfiles.some((p) => p.id === value.translationLlmId) && (
                  <option value="">（未选择）</option>
                )}
                {transProfiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {profileLabel(p)}
                  </option>
                ))}
              </>
            )}
          </select>
          {transProfiles.length === 0 && (
            <p className="hint hint--warn">
              <WarnIcon />点「模型库管理」添加一个模型，并勾上「可用于翻译」。
            </p>
          )}
        </div>
      )}

      <div className="field">
        <div className="llm-pick-head">
          <label>提炼重点用模型</label>
          <button
            type="button"
            className="llm-btn"
            disabled={disabled}
            onClick={() => setLibOpen(true)}
          >
            ⚙ 模型库管理
          </button>
        </div>
        <select
          value={value.summaryLlmId}
          disabled={disabled}
          onChange={(e) => set({ summaryLlmId: e.target.value })}
        >
          <option value="">（未指定 → 默认用翻译模型）</option>
          {sumProfiles.map((p) => (
            <option key={p.id} value={p.id}>
              {profileLabel(p)}
            </option>
          ))}
        </select>
        <p className="hint">
          「✦ 提炼重点」用的模型，独立于翻译引擎（Google / 纯字幕也能提炼）；不指定就用翻译模型。
        </p>
      </div>

      <div className="settings-divider">高级设置</div>

      <CollapseHead
        title="进阶微调"
        open={showAdvanced}
        onToggle={() => setShowAdvanced((v) => !v)}
      />
      {showAdvanced && (
      <div className="vp-section">
      <div className="field vp-slider">
        <label>VAD·停顿（句界）{value.segSilenceMs}ms</label>
        <input
          type="range"
          min={400}
          max={1500}
          step={50}
          value={value.segSilenceMs}
          disabled={disabled}
          onChange={(e) => set({ segSilenceMs: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
说话人停顿多久算「一句说完」并断句翻译。默认 500ms。调得越小字幕出得越快、但一句可能被切碎；调得越大句子越完整、字幕出得越慢。
          </>
        }
      >
        停顿多久算一句话说完（断句快慢）
      </InfoLine>
      <div className="field vp-slider">
        <label>攒句·单句上限 {value.sentenceMaxS.toFixed(0)}s</label>
        <input
          type="range"
          min={8}
          max={40}
          step={1}
          value={value.sentenceMaxS}
          disabled={disabled}
          onChange={(e) => set({ sentenceMaxS: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
无停顿的连续说话，最多攒到这个时长就强制断句。默认 18s。调大→长独白攒得更完整，但译文更晚出；调小→更早断句、译文出得更勤，但长独白可能被从中切断。
          </>
        }
      >
        无停顿的连续说话上限
      </InfoLine>
      <div className="field vp-slider">
        <label>翻译超时 {value.translateTimeoutS}s</label>
        <input
          type="range"
          min={10}
          max={120}
          step={5}
          value={value.translateTimeoutS}
          disabled={disabled}
          onChange={(e) => set({ translateTimeoutS: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
单句翻译等多久还没返回就放弃。默认 30s。用本地大模型(Ollama)且长句偶尔翻不出来、报超时，就往大调；用云端的大模型一般不用动。
          </>
        }
      >
        一句翻译最多等多久(本地大模型可调大)
      </InfoLine>
      <div className="field vp-slider">
        <label>说话人·归并阈值 {value.speakerMergeThreshold.toFixed(2)}</label>
        <input
          type="range"
          min={0.3}
          max={0.9}
          step={0.05}
          value={value.speakerMergeThreshold}
          disabled={disabled}
          onChange={(e) => set({ speakerMergeThreshold: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
开启「说话人分离」后，用来判断两段话是不是同一个人。默认 0.55。调大→认人更严：同一个人可能被拆成「说话人1」「说话人2」；调小→认人更松：两个不同的人可能被当成同一个人。遇到"一个人被拆成好几号"就往小调，"两个人混成一号"就往大调。改动在下次「开始」时生效。
          </>
        }
      >
        同一个人是否被认成同一个编号
      </InfoLine>
      </div>
      )}

      <CollapseHead
        title="麦克风·旁人过滤"
        open={showVoiceprint}
        onToggle={() => setShowVoiceprint((v) => !v)}
      />
      {showVoiceprint && (
      <div className="vp-section">
      <div className="field vp-slider">
        <label>按音量挡旁人 {value.micGateRms.toFixed(3)}</label>
        <input
          type="range"
          min={0.005}
          max={0.05}
          step={0.005}
          value={value.micGateRms}
          disabled={disabled}
          onChange={(e) => set({ micGateRms: Number(e.target.value) })}
        />
      </div>
      <InfoLine
        detail={
          <>
用麦克风时生效(麦克风翻译 + 回灌出站)。它<b>按响度</b>挡掉离麦克风远的旁人/杂音——离得远=声音小=判为旁人挡掉；系统声(环回)不受影响。默认 0.02。调大→挡得更严，只收近处、清晰的说话，但太小声时也可能被误挡；调小→放得更宽，收音更全，但旁边的人声/杂音更容易混进来。
          </>
        }
      >
        按音量挡掉远处旁人（麦克风翻译 + 回灌）
      </InfoLine>
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
        <label>声纹相似度阈值 {value.voiceprintThreshold.toFixed(2)}</label>
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
在<b>回灌·双向同传</b>时生效：出站每段声音先与你登记的声纹做对比，不像你的（旁人、别人说话）直接丢弃，不识别也不合成。需先<b>登记声音</b>并开启开关；阈值越高越严，误挡自己就调低、漏进旁人就调高（默认 0.50）。
          </>
        }
      >
        登记你的声纹后，出站只放行你本人，挡掉旁人说话
      </InfoLine>
      </div>
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

      </div>

      {libOpen &&
        createPortal(
          <div className="llm-mask" onClick={(e) => e.target === e.currentTarget && setLibOpen(false)}>
            <div className="llm-modal">
              <div className="llm-modal-head">
                <h3>LLM 模型库</h3>
                <button type="button" className="llm-modal-x" onClick={() => setLibOpen(false)}>
                  ×
                </button>
              </div>
              <div className="llm-lib-toolbar">
                <span className="hint">共 {value.llmProfiles.length} 个模型</span>
                <button
                  type="button"
                  className="llm-btn llm-btn--primary"
                  onClick={() =>
                    setEditProfile({
                      id: newLlmId(),
                      label: "",
                      isLocal: false,
                      baseUrl: "",
                      apiKey: "",
                      model: "",
                      capTranslation: true,
                      capSummary: true,
                    })
                  }
                >
                  + 新增模型
                </button>
              </div>
              <p className="hint" style={{ marginTop: 0 }}>
                「可翻译 / 可提炼」是能力：勾了才会出现在对应下拉里，一个模型可两者都勾。
              </p>
              {value.llmProfiles.length === 0 ? (
                <div className="llm-empty">还没有模型，点「+ 新增模型」添加。</div>
              ) : (
                value.llmProfiles.map((p) => (
                  <div className="llm-row" key={p.id}>
                    <div>
                      <div className="llm-row-name">
                        {modelDisplayName(p, value.llmProfiles)}
                        <span className="llm-badge">{p.isLocal ? "Ollama" : "云端"}</span>
                      </div>
                      <div className="llm-row-sub">
                        模型 {p.model || "—"} · {p.baseUrl}
                      </div>
                    </div>
                    <div className="llm-caps">
                      <label className="llm-cap">
                        <input
                          type="checkbox"
                          checked={p.capTranslation}
                          onChange={(e) => toggleCap(p.id, "capTranslation", e.target.checked)}
                        />
                        可翻译
                      </label>
                      <label className="llm-cap">
                        <input
                          type="checkbox"
                          checked={p.capSummary}
                          onChange={(e) => toggleCap(p.id, "capSummary", e.target.checked)}
                        />
                        可提炼
                      </label>
                    </div>
                    <div className="llm-ops">
                      <button type="button" className="llm-icon-btn" title="编辑" onClick={() => setEditProfile(p)}>
                        ✏️
                      </button>
                      <button type="button" className="llm-icon-btn" title="删除" onClick={() => deleteProfile(p.id)}>
                        🗑️
                      </button>
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>,
          document.body
        )}

      {editProfile &&
        createPortal(
          <LlmEditModal
            initial={editProfile}
            exists={value.llmProfiles.some((x) => x.id === editProfile.id)}
            onCancel={() => setEditProfile(null)}
            onSave={(p) => {
              upsertProfile(p);
              setEditProfile(null);
            }}
          />,
          document.body
        )}
    </div>
  );
}
