import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Play, Square, X } from "lucide-react";
import { SubtitleEvent, StatusEvent } from "../types";

/// 悬浮字幕窗（QQ 歌词风）：默认只显示带描边的文字、背景透明；鼠标悬停才出背景条+控制按钮。
/// 原文/译文两种颜色。■停止 / ▶启动（启动发事件给主窗口用其设置开始）/ ✕关闭。
export function Overlay() {
  const [sub, setSub] = useState<SubtitleEvent | null>(null);
  // 初始未运行：显示 ▶ 开始。实际运行态由 status 事件流同步（若打开时已在翻译，下一个 status 事件即翻转为运行中）
  const [running, setRunning] = useState(false);

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
    return () => unsubs.forEach((u) => u());
  }, []);

  return (
    <div className="overlay" data-tauri-drag-region>
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
          <div className="ov-orig" data-tauri-drag-region>
            {sub.original}
          </div>
          {sub.translated && (
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
