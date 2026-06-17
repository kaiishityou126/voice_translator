import { useEffect, useRef } from "react";
import { SubtitleEvent, TargetLang, TARGET_LANG_LABEL } from "../types";

interface Props {
  items: SubtitleEvent[];
  targetLang: TargetLang;
  liveState?: string;
  vadLabel?: string;
}

// 时间戳(epoch 毫秒)→ MM-DD HH:MM:SS
function fmtTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(
    d.getMinutes()
  )}:${p(d.getSeconds())}`;
}

export function SubtitleView({ items, targetLang, liveState, vadLabel }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);

  // 直接把滚动容器拉到底（比 scrollIntoView 在“被面板挤压”场景更可靠）
  const scrollToBottom = (smooth: boolean) => {
    const el = containerRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  };

  useEffect(() => {
    scrollToBottom(true);
  }, [items]);

  // 容器被设置/提炼面板挤压而尺寸变化时，重新滚到底，避免最新字幕被挤出可视区。
  // 用 rAF 等布局/展开动画 settle 后再滚，确保滚到真正的底部。
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      requestAnimationFrame(() => scrollToBottom(false));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  if (items.length === 0) {
    return (
      <div className="subtitles">
        <div className="empty">
          <h2>等待音频…</h2>
          <p>
            选择音频来源、目标语言和翻译引擎（云 API 需填 key；Google 免费 / 纯字幕免 key），点「开始」。
            播放任意带人声的音频（视频 / 会议 / 网页），这里会实时显示
            <b> 原文 + {TARGET_LANG_LABEL[targetLang]}译文</b>。
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="subtitles" ref={containerRef}>
      {items.map((it, i) => {
        const live = i === items.length - 1;
        return (
          <div key={it.id} className={`sub-card${live ? " live" : ""}`}>
            <div className="orig">{it.original}</div>
            {it.translated ? (
              <div className="trans">
                {it.translated}
                {it.pending && <span className="stream-caret">▍</span>}
              </div>
            ) : it.pending ? (
              <div className="trans pending">翻译中…</div>
            ) : null}
            <div className="meta">
              {live && liveState ? (
                <>
                  <span className="wave" aria-hidden="true">
                    <i />
                    <i />
                    <i />
                    <i />
                    <i />
                  </span>
                  <span className="live-state">{liveState}</span>
                  <span className="tag">· {fmtTime(it.ts)}</span>
                  {vadLabel && <span className="tag">· {vadLabel}</span>}
                </>
              ) : (
                <span className="tag">{fmtTime(it.ts)}</span>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
