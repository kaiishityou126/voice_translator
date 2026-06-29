import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Play, Square, X, Minus, Plus } from "lucide-react";
import { SubtitleEvent, StatusEvent, OverlayConfig } from "../types";

// 悬浮窗字号档位 → 缩放系数(悬浮窗可比主界面更醒目)
const OV_SCALE: Record<OverlayConfig["overlayFontScale"], number> = {
  sm: 0.8,
  md: 1,
  lg: 1.35,
};

const OV_SCALES: OverlayConfig["overlayFontScale"][] = ["sm", "md", "lg"];
const OV_SCALE_LABEL: Record<OverlayConfig["overlayFontScale"], string> = {
  sm: "小",
  md: "中",
  lg: "大",
};

/// 悬浮字幕窗（QQ 歌词风）：默认只显示带描边的文字、背景透明；鼠标悬停才出背景条+控制按钮。
/// 原文/译文两种颜色。■停止 / ▶启动（启动发事件给主窗口用其设置开始）/ ✕关闭。
/// 字号、透明度直接在窗上调（hover 显出控件），改动回写主窗口设置。
export function Overlay() {
  const [sub, setSub] = useState<SubtitleEvent | null>(null);
  // 初始未运行：显示 ▶ 开始。实际运行态由 status 事件流同步（若打开时已在翻译，下一个 status 事件即翻转为运行中）
  const [running, setRunning] = useState(false);
  // 显示配置由主窗口下发（悬浮窗读不到 Settings）；默认双语、中号、全透明描边风
  const [cfg, setCfg] = useState<OverlayConfig>({
    displayMode: "bilingual",
    overlayFontScale: "md",
    overlayOpacity: 0,
  });
  // 刚被打开时先闪一下全背景色+按钮，再淡到设定透明度
  const [flash, setFlash] = useState(false);
  const flashTimer = useRef<number | null>(null);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    // id 守卫：流式回填会乱序到达，丢弃比当前更旧段落的迟到帧，避免新句被旧句覆盖
    listen<SubtitleEvent>("subtitle", (e) =>
      setSub((prev) => (prev && e.payload.id < prev.id ? prev : e.payload))
    ).then((u) => unsubs.push(u));
    listen<StatusEvent>("status", (e) => {
      const s = e.payload.state;
      if (s === "idle" || s === "error") setRunning(false);
      else setRunning(true); // starting/listening/recognizing/translating
    }).then((u) => unsubs.push(u));
    listen<OverlayConfig>("overlay-config", (e) => setCfg(e.payload)).then((u) =>
      unsubs.push(u)
    );
    // 主窗口通知「悬浮窗刚被打开」→ 闪现全背景色+按钮约 1.2s，再淡回设定透明度
    listen("overlay-shown", () => {
      setFlash(true);
      if (flashTimer.current) window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlash(false), 1200);
    }).then((u) => unsubs.push(u));
    // 挂载后向主窗口要一次当前配置（首次打开时主窗口还没主动下发过）
    emit("overlay-ready").catch(() => {});
    return () => {
      unsubs.forEach((u) => u());
      if (flashTimer.current) window.clearTimeout(flashTimer.current);
    };
  }, []);

  const showOrig = cfg.displayMode !== "transOnly";
  const showTrans = cfg.displayMode !== "origOnly";
  // --ov-scale 缩放字号；--ov-bg-alpha 控制非悬停时的常驻背景不透明度
  const ovStyle = {
    "--ov-scale": OV_SCALE[cfg.overlayFontScale] ?? 1,
    "--ov-bg-alpha": Math.min(100, Math.max(0, cfg.overlayOpacity)) / 100,
  } as React.CSSProperties;

  // 透明度 = 100 - 不透明度；控件按透明度增减，回写不透明度
  const transparency = 100 - cfg.overlayOpacity;
  const setTransparency = (t: number) => {
    const tr = Math.min(100, Math.max(0, t));
    const overlayOpacity = 100 - tr;
    setCfg((c) => ({ ...c, overlayOpacity })); // 乐观更新，主窗口回灌会确认
    emit("overlay-config-change", { overlayOpacity }).catch(() => {});
  };
  const setScale = (next: OverlayConfig["overlayFontScale"]) => {
    setCfg((c) => ({ ...c, overlayFontScale: next })); // 乐观更新，主窗口回灌会确认
    emit("overlay-config-change", { overlayFontScale: next }).catch(() => {});
  };

  return (
    <div
      className={`overlay${flash ? " is-flash" : ""}`}
      data-tauri-drag-region
      style={ovStyle}
    >
      <div className="ov-ctrls ov-ctrls-left">
        <div className="ov-seg" title="悬浮窗字号">
          {OV_SCALES.map((s) => (
            <button
              key={s}
              className={`ov-btn${cfg.overlayFontScale === s ? " active" : ""}`}
              title={`字号：${OV_SCALE_LABEL[s]}`}
              onClick={() => setScale(s)}
            >
              {OV_SCALE_LABEL[s]}
            </button>
          ))}
        </div>
        <div className="ov-op" title="悬浮窗透明度">
          <button
            className="ov-btn"
            title="更不透明"
            onClick={() => setTransparency(transparency - 10)}
          >
            <Minus size={12} strokeWidth={2.4} />
          </button>
          <span className="ov-op-val">{transparency}%</span>
          <button
            className="ov-btn"
            title="更透明"
            onClick={() => setTransparency(transparency + 10)}
          >
            <Plus size={12} strokeWidth={2.4} />
          </button>
        </div>
      </div>
      <div className="ov-ctrls">
        {running ? (
          <button
            className="ov-btn"
            title="停止翻译"
            onClick={() => invoke("stop_translation").catch(() => {})}
          >
            <Square size={13} fill="currentColor" strokeWidth={0} />
          </button>
        ) : (
          <button
            className="ov-btn ov-btn-go"
            title="启动翻译"
            onClick={() => emit("overlay-start").catch(() => {})}
          >
            <Play size={13} fill="currentColor" strokeWidth={0} />
          </button>
        )}
        <button
          className="ov-btn"
          title="关闭悬浮窗"
          onClick={() => invoke("toggle_overlay").catch(() => {})}
        >
          <X size={14} strokeWidth={2.2} />
        </button>
      </div>

      {sub ? (
        <div className="ov-text" data-tauri-drag-region>
          {showOrig && (
            <div className="ov-orig" data-tauri-drag-region>
              {sub.original}
            </div>
          )}
          {showTrans && sub.translated && (
            <div className="ov-trans" data-tauri-drag-region>
              {sub.translated}
            </div>
          )}
        </div>
      ) : (
        <div className="ov-hint" data-tauri-drag-region>
          {running ? "聆听中…（悬停显示控制）" : "已停止 · 悬停点 ▶ 启动"}
        </div>
      )}
    </div>
  );
}
