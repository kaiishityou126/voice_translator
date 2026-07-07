import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AlertTriangle, Check, Mic, Headphones, Volume2 } from "lucide-react";
import {
  Settings,
  TargetLang,
  SourceLang,
  RELAY_LANG_LABEL,
  SOURCE_LANG_LABEL,
  KOKORO_TTS_LANGS,
  voicesForLang,
  relayMyLangOptions,
  relayPeerSpeakOptions,
} from "../types";

// 虚拟声卡信息按平台切换：Windows=VB-Cable / macOS=BlackHole
const CABLE_INFO: Record<string, { url: string; linkText: string; meetingMic: string }> = {
  windows: { url: "https://vb-audio.com/Cable/", linkText: "VB-Cable（免费版）", meetingMic: "CABLE Output" },
  macos: { url: "https://existential.audio/blackhole/", linkText: "BlackHole（免费）", meetingMic: "BlackHole 2ch" },
};

// 两层语言块用的超短标签（顶栏求紧凑）
const SHORT: Record<string, string> = {
  auto: "自", zh: "中", en: "EN", ja: "日", ko: "한", yue: "粤",
  fr: "法", es: "西", de: "德", ru: "俄", hi: "印", it: "意", pt: "葡",
};
const short = (code: string) => SHORT[code] ?? code;

/// 回灌顶栏控件：两层语言块（点开弹卡片）+「原声」开关。仅在回灌模式（ttsEnabled）渲染。
/// 三套语言集随识别引擎派生；运行中锁定（改语言需重启流水线，与音频来源一致）。
export function RelayBar({
  settings,
  update,
  running,
}: {
  settings: Settings;
  update: (s: Settings) => void;
  running: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [hasCable, setHasCable] = useState(true);
  const [platform, setPlatform] = useState("windows");
  const wrapRef = useRef<HTMLDivElement>(null);

  // 检测虚拟声卡 + 当前平台（进入回灌模式时查一次）
  useEffect(() => {
    invoke<boolean>("cable_exists").then(setHasCable).catch(() => {});
    invoke<string>("get_platform").then(setPlatform).catch(() => {});
  }, []);

  // 归位非法值：引擎/对方听变更后，我的语言、对方说、音色可能越界 → 拉回合法默认
  useEffect(() => {
    const myOpts = relayMyLangOptions(settings.asrEngine);
    const speakOpts = relayPeerSpeakOptions(settings.asrEngine);
    const patch: Partial<Settings> = {};
    if (!myOpts.includes(settings.relayMyLang)) patch.relayMyLang = myOpts[0];
    if (!speakOpts.includes(settings.relayInboundSourceLang)) patch.relayInboundSourceLang = "auto";
    if (!KOKORO_TTS_LANGS.includes(settings.ttsTargetLang)) patch.ttsTargetLang = "ja";
    const hearLang = patch.ttsTargetLang ?? settings.ttsTargetLang;
    const voices = voicesForLang(hearLang);
    if (voices.length && !voices.some((v) => v.id === settings.ttsVoice)) patch.ttsVoice = voices[0].id;
    if (Object.keys(patch).length) update({ ...settings, ...patch });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.asrEngine, settings.ttsTargetLang]);

  // 点击卡片外部即关闭
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Element | null;
      if (t && wrapRef.current && !wrapRef.current.contains(t)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const set = (patch: Partial<Settings>) => update({ ...settings, ...patch });
  const isQwen = settings.asrEngine === "qwen3Asr";
  const passthrough = settings.relayPassthrough;
  const myShort = short(settings.relayMyLang);
  const myOpts = relayMyLangOptions(settings.asrEngine);
  const speakOpts = relayPeerSpeakOptions(settings.asrEngine);
  const voices = voicesForLang(settings.ttsTargetLang);
  const cable = CABLE_INFO[platform] ?? CABLE_INFO.windows;

  return (
    <>
      {/* 两层语言块：我说→对方听 / 我读←对方说 */}
      <div className="relay-langwrap" ref={wrapRef}>
        <button
          className={`relay-langblock${open ? " open" : ""}`}
          disabled={running}
          title="回灌语言设置"
          onClick={() => setOpen((v) => !v)}
        >
          <span className={`relay-lrow${passthrough ? " raw" : ""}`}>
            <span>{myShort}</span>
            <span className="ar">→</span>
            <span className="peer">{passthrough ? "原声" : short(settings.ttsTargetLang)}</span>
          </span>
          <span className="relay-lrow">
            <span>{myShort}</span>
            <span className="ar">←</span>
            <span className="peer">{short(settings.relayInboundSourceLang)}</span>
          </span>
        </button>

        {open && (
          <div className="relay-card">
            <p className="relay-card-h">
              回灌语言 <span className="relay-eng">识别 · {isQwen ? "Qwen3" : "SenseVoice"}</span>
            </p>

            {/* 我的语言（我说 & 我读 共用，具体语种） */}
            <div className="relay-me">
              <div className="relay-me-lab">
                <b>我的语言</b>
                <i>我说 &amp; 我读 共用</i>
              </div>
              <select
                className="relay-pick"
                value={settings.relayMyLang}
                disabled={running}
                onChange={(e) => set({ relayMyLang: e.target.value as TargetLang })}
              >
                {myOpts.map((l) => (
                  <option key={l} value={l}>{RELAY_LANG_LABEL[l]}</option>
                ))}
              </select>
            </div>

            <div className="relay-div" />

            {/* 我说 → 对方听 */}
            <div className={`relay-dir${passthrough ? " dim" : ""}`}>
              <Mic size={14} strokeWidth={1.9} className="relay-dir-ico" />
              <span>我说</span>
              <b>{myShort}</b>
              <span className="ar">→</span>
              <span>对方听</span>
              <select
                className="relay-pick"
                value={settings.ttsTargetLang}
                disabled={running || passthrough}
                onChange={(e) => {
                  const lang = e.target.value as TargetLang;
                  const first = voicesForLang(lang)[0];
                  set({ ttsTargetLang: lang, ...(first ? { ttsVoice: first.id } : {}) });
                }}
              >
                {KOKORO_TTS_LANGS.map((l) => (
                  <option key={l} value={l}>{RELAY_LANG_LABEL[l]}</option>
                ))}
              </select>
            </div>
            {!passthrough && settings.relayMyLang === settings.ttsTargetLang && (
              <p className="relay-warn">
                <AlertTriangle size={12} strokeWidth={2} />
                我的语言=对方听，等于没翻译——对方听到的是机器复述我的话（LLM 引擎下会改写措辞）。
              </p>
            )}

            {/* 我读 ← 对方说（入站识别源；Qwen3 锁自动） */}
            <div className="relay-dir">
              <Headphones size={14} strokeWidth={1.9} className="relay-dir-ico" />
              <span>我读</span>
              <b>{myShort}</b>
              <span className="ar">←</span>
              <span>对方说</span>
              <select
                className="relay-pick"
                value={settings.relayInboundSourceLang}
                disabled={running || isQwen}
                title={isQwen ? "Qwen3 只能自动识别，不能单独指定" : undefined}
                onChange={(e) => set({ relayInboundSourceLang: e.target.value as SourceLang })}
              >
                {isQwen ? (
                  <option value="auto">自动检测（多语）</option>
                ) : (
                  speakOpts.map((l) => (
                    <option key={l} value={l}>{SOURCE_LANG_LABEL[l]}</option>
                  ))
                )}
              </select>
            </div>
            {settings.relayInboundSourceLang !== "auto" &&
              settings.relayInboundSourceLang === settings.relayMyLang && (
                <p className="relay-warn">
                  <AlertTriangle size={12} strokeWidth={2} />
                  「对方说」和我的语言相同，字幕等于没翻译（照搬原文）。
                </p>
              )}

            {/* 音色（原声模式停用） */}
            <div className={`relay-dir${passthrough ? " dim" : ""}`}>
              <Volume2 size={14} strokeWidth={1.9} className="relay-dir-ico" />
              <span>音色</span>
              <select
                className="relay-pick"
                value={settings.ttsVoice}
                disabled={running || passthrough}
                onChange={(e) => set({ ttsVoice: Number(e.target.value) || 0 })}
              >
                {voices.length ? (
                  voices.map((v) => (
                    <option key={v.id} value={v.id}>{v.label}</option>
                  ))
                ) : (
                  <option>（无音色）</option>
                )}
              </select>
            </div>

            {hasCable ? (
              <div className="relay-note">
                虚拟声卡已安装，在会议软件的「设置」里把<b>麦克风</b>（通常在「音频」或「设备」里）选成{" "}
                <b>{cable.meetingMic}</b>，即可让对方听到{passthrough ? "你的原声" : "译文"}。
              </div>
            ) : (
              <div className="relay-note warn">
                <AlertTriangle size={13} strokeWidth={2} className="relay-note-ico" />
                <span>
                  未检测到虚拟声卡。需先安装{" "}
                  <span
                    className="relay-link"
                    role="button"
                    tabIndex={0}
                    title={`点击可跳转到下载页面\n${cable.url}`}
                    onClick={() => openUrl(cable.url)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") openUrl(cable.url);
                    }}
                  >
                    {cable.linkText}
                  </span>
                  ，安装完可能需要重启才能生效，否则无法使用回灌。
                </span>
              </div>
            )}
          </div>
        )}
      </div>

      {/* 原声开关（常驻顶栏；默认不勾=译文） */}
      <button
        className={`relay-raw${passthrough ? " on" : ""}`}
        aria-pressed={passthrough}
        title="对方听到：译文 / 你的原声（可运行中切换）"
        onClick={() => set({ relayPassthrough: !passthrough })}
      >
        <span className="relay-raw-box">{passthrough && <Check size={11} strokeWidth={3} />}</span>
        原声
      </button>
    </>
  );
}
