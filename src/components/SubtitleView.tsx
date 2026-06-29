import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Copy, RefreshCw, Search, ChevronUp, ChevronDown, X, ArrowDown } from "lucide-react";
import { SubtitleEvent, TargetLang, DisplayMode, TARGET_LANG_LABEL } from "../types";

interface Props {
  items: SubtitleEvent[];
  targetLang: TargetLang;
  // 字幕显示模式：双语 / 仅译文 / 仅原文
  displayMode?: DisplayMode;
  liveState?: string;
  vadLabel?: string;
  detectedLangLabel?: string;
  // 「重译此句」回调；引擎为 none(纯字幕)时不传 → 不显示重译按钮
  onRetranslate?: (item: SubtitleEvent) => void;
  // 正在重译的段落 id 集合
  retranslatingIds?: Set<number>;
  // 只读回看模式：隐藏 live 状态/重译，仅展示历史记录
  readOnly?: boolean;
}

// 复制到剪贴板：优先用 navigator.clipboard，失败回退到 textarea + execCommand
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } finally {
      document.body.removeChild(ta);
    }
  }
}

// 时间戳(epoch 毫秒)→ MM-DD HH:MM:SS
function fmtTime(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(
    d.getMinutes()
  )}:${p(d.getSeconds())}`;
}

// 把命中子串包成 <mark>；q 为空原样返回。大小写不敏感，CJK 直接子串匹配。
function highlight(text: string, q: string): React.ReactNode {
  if (!q) return text;
  const lower = text.toLowerCase();
  const needle = q.toLowerCase();
  let hit = lower.indexOf(needle);
  if (hit < 0) return text;
  const out: React.ReactNode[] = [];
  let from = 0;
  let key = 0;
  while (hit >= 0) {
    if (hit > from) out.push(text.slice(from, hit));
    out.push(
      <mark className="sub-hl" key={key++}>
        {text.slice(hit, hit + needle.length)}
      </mark>
    );
    from = hit + needle.length;
    hit = lower.indexOf(needle, from);
  }
  if (from < text.length) out.push(text.slice(from));
  return out;
}

export function SubtitleView({
  items,
  targetLang,
  displayMode = "bilingual",
  liveState,
  vadLabel,
  detectedLangLabel,
  onRetranslate,
  retranslatingIds,
  readOnly = false,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  // 原文/译文是否显示：仅译文隐原文、仅原文隐译文
  const showOrig = displayMode !== "transOnly";
  const showTrans = displayMode !== "origOnly";

  // 搜索：是否展开、查询词、仅显示匹配、当前命中指针
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [onlyMatch, setOnlyMatch] = useState(false);
  const [matchPtr, setMatchPtr] = useState(0);
  const searchInputRef = useRef<HTMLInputElement>(null);

  // 是否贴底（直播态自动滚到底）；用户上滚回看时置 false，新字幕不再抢滚动
  const stickRef = useRef(true);
  const [stick, setStick] = useState(true);
  // 离开底部后累计的新字幕数（用于「↓ N 条新字幕」药丸）
  const [newCount, setNewCount] = useState(0);
  const baselineRef = useRef(items.length);

  const q = query.trim();
  const searching = q.length > 0;

  // 命中的行下标（搜原文+译文，大小写不敏感）
  const matchedIdx = useMemo(() => {
    if (!searching) return [];
    const needle = q.toLowerCase();
    const res: number[] = [];
    items.forEach((it, i) => {
      if (
        it.original.toLowerCase().includes(needle) ||
        (it.translated && it.translated.toLowerCase().includes(needle))
      ) {
        res.push(i);
      }
    });
    return res;
  }, [items, q, searching]);

  // 仅显示匹配时，渲染行收敛到命中项；否则渲染全部
  const rows = useMemo(
    () => (searching && onlyMatch ? matchedIdx.map((i) => items[i]) : items),
    [items, matchedIdx, onlyMatch, searching]
  );

  const liveId = !readOnly && items.length > 0 ? items[items.length - 1].id : null;

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => containerRef.current,
    estimateSize: () => 92,
    overscan: 6,
    getItemKey: (i) => rows[i].id,
  });

  const scrollToBottom = (smooth: boolean) => {
    const el = containerRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  };

  // 贴底检测：滚动时判断是否在底部附近，决定后续是否自动跟随
  const onScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const atBottom = dist < 80;
    if (atBottom !== stickRef.current) {
      stickRef.current = atBottom;
      setStick(atBottom);
      baselineRef.current = items.length;
      if (atBottom) setNewCount(0);
    }
  };

  // 新字幕到达：贴底且非搜索态 → 跟随到底；否则累计「新字幕数」给药丸
  useEffect(() => {
    if (searching) return;
    if (stickRef.current) {
      requestAnimationFrame(() => scrollToBottom(true));
      baselineRef.current = items.length;
      setNewCount(0);
    } else {
      setNewCount(Math.max(0, items.length - baselineRef.current));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [items, searching]);

  // 容器被设置/提炼面板挤压而尺寸变化时，贴底态下重新滚到底
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (stickRef.current && !searching) {
        requestAnimationFrame(() => scrollToBottom(false));
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [searching]);

  // 查询词变化：命中指针归零并跳到第一个命中
  useEffect(() => {
    setMatchPtr(0);
    if (!searching) return;
    const target = onlyMatch ? 0 : matchedIdx[0];
    if (target !== undefined) {
      requestAnimationFrame(() => virtualizer.scrollToIndex(target, { align: "center" }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);

  // 搜索框展开时自动聚焦
  useEffect(() => {
    if (searchOpen) requestAnimationFrame(() => searchInputRef.current?.focus());
  }, [searchOpen]);

  const total = onlyMatch ? rows.length : matchedIdx.length;
  const gotoMatch = (dir: 1 | -1) => {
    if (total === 0) return;
    const next = (matchPtr + dir + total) % total;
    setMatchPtr(next);
    const rowIdx = onlyMatch ? next : matchedIdx[next];
    virtualizer.scrollToIndex(rowIdx, { align: "center" });
  };

  const closeSearch = () => {
    setSearchOpen(false);
    setQuery("");
    setOnlyMatch(false);
  };

  // Ctrl+F 打开搜索；Esc 关闭
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setSearchOpen(true);
      } else if (e.key === "Escape" && searchOpen) {
        closeSearch();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchOpen]);

  if (items.length === 0) {
    return (
      <div className="subs-region">
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
      </div>
    );
  }

  const vItems = virtualizer.getVirtualItems();

  return (
    <div className="subs-region">
      {/* 搜索栏：浮在字幕区顶部，不占滚动流 */}
      {searchOpen ? (
        <div className="sub-search">
          <Search size={14} strokeWidth={1.9} className="sub-search-ic" />
          <input
            ref={searchInputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") gotoMatch(e.shiftKey ? -1 : 1);
            }}
            placeholder="搜索原文 / 译文…"
            spellCheck={false}
          />
          <span className="sub-search-count">
            {searching ? (total === 0 ? "无匹配" : `${matchPtr + 1}/${total}`) : ""}
          </span>
          <button
            className="sub-search-btn"
            title="上一个 (Shift+Enter)"
            disabled={total === 0}
            onClick={() => gotoMatch(-1)}
          >
            <ChevronUp size={15} strokeWidth={2} />
          </button>
          <button
            className="sub-search-btn"
            title="下一个 (Enter)"
            disabled={total === 0}
            onClick={() => gotoMatch(1)}
          >
            <ChevronDown size={15} strokeWidth={2} />
          </button>
          <label className="sub-search-only" title="仅显示匹配的段落">
            <input
              type="checkbox"
              checked={onlyMatch}
              onChange={(e) => setOnlyMatch(e.target.checked)}
            />
            仅匹配
          </label>
          <button className="sub-search-btn" title="关闭 (Esc)" onClick={closeSearch}>
            <X size={15} strokeWidth={2} />
          </button>
        </div>
      ) : (
        <button
          className="sub-search-fab"
          title="搜索转写 (Ctrl+F)"
          onClick={() => setSearchOpen(true)}
        >
          <Search size={16} strokeWidth={1.9} />
        </button>
      )}

      <div className="subtitles" ref={containerRef} onScroll={onScroll}>
        <div className="subs-virt" style={{ height: virtualizer.getTotalSize() }}>
          {vItems.map((v) => {
            const it = rows[v.index];
            const live = it.id === liveId && !(searching && onlyMatch);
            const retranslating = retranslatingIds?.has(it.id) ?? false;
            const isCurMatch =
              searching && (onlyMatch ? v.index === matchPtr : v.index === matchedIdx[matchPtr]);
            return (
              <div
                key={v.key}
                data-index={v.index}
                ref={virtualizer.measureElement}
                className="subs-row"
                style={{ transform: `translateY(${v.start}px)` }}
              >
                <div
                  className={`sub-card${live ? " live" : ""}${isCurMatch ? " match-cur" : ""}`}
                >
                  {showOrig && <div className="orig">{highlight(it.original, q)}</div>}
                  {showTrans &&
                    (it.translated ? (
                      <div className="trans">
                        {highlight(it.translated, q)}
                        {it.pending && <span className="stream-caret">▍</span>}
                      </div>
                    ) : it.pending ? (
                      <div className="trans pending">翻译中…</div>
                    ) : null)}
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
                        {detectedLangLabel && <span className="tag">· {detectedLangLabel}</span>}
                      </>
                    ) : (
                      <>
                        <span className="tag">{fmtTime(it.ts)}</span>
                        <span className="card-actions">
                          <button
                            className="card-act"
                            title="复制原文"
                            onClick={() => copyText(it.original)}
                          >
                            <Copy size={13} strokeWidth={1.8} />
                          </button>
                          {it.translated && (
                            <button
                              className="card-act"
                              title="复制译文"
                              onClick={() => copyText(it.translated)}
                            >
                              <Copy size={13} strokeWidth={1.8} />
                              <span className="card-act-tag">译</span>
                            </button>
                          )}
                          {onRetranslate && !readOnly && (
                            <button
                              className={`card-act${retranslating ? " busy" : ""}`}
                              title={retranslating ? "重译中…" : "重译此句"}
                              disabled={retranslating}
                              onClick={() => onRetranslate(it)}
                            >
                              <RefreshCw size={13} strokeWidth={1.8} />
                            </button>
                          )}
                        </span>
                      </>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {/* 离开底部且有新字幕：药丸点回直播 */}
      {!stick && !searching && newCount > 0 && (
        <button
          className="subs-newpill"
          onClick={() => {
            stickRef.current = true;
            setStick(true);
            setNewCount(0);
            baselineRef.current = items.length;
            scrollToBottom(true);
          }}
        >
          <ArrowDown size={14} strokeWidth={2.2} />
          {newCount} 条新字幕
        </button>
      )}
    </div>
  );
}
