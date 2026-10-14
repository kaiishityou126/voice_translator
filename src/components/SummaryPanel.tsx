import { useEffect, useRef, useState } from "react";
import { Pencil, Wand2, Check, X, AlertCircle, FolderOpen } from "lucide-react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

/// 二级及以上归为小节标题（h4）；模型若没按要求用了三级标题，一律渲染成圆点条目，
/// 保证整篇只有「小节标题 + 圆点条目」两种形态。
const mdComponents: Components = {
  h1: ({ children }) => <h4>{children}</h4>,
  h2: ({ children }) => <h4>{children}</h4>,
  h3: ({ children }) => <div className="md-alt-li">{children}</div>,
  h4: ({ children }) => <div className="md-alt-li">{children}</div>,
  h5: ({ children }) => <div className="md-alt-li">{children}</div>,
  h6: ({ children }) => <div className="md-alt-li">{children}</div>,
  table: ({ children }) => (
    <div className="summary-table-wrap">
      <table>{children}</table>
    </div>
  ),
};

/// 抽屉默认宽度，同时作为拖拽下限（只允许往宽拉，避免拉窄后表格/标题挤得没法看）
const DEFAULT_WIDTH = 410;

interface Props {
  text: string;
  error?: boolean;
  pending: boolean;
  visible: boolean;
  canResummarize: boolean;
  onResummarize: () => void;
  onSave: (content: string) => void;
  // 打开存档文件夹
  onOpenDir: () => void;
}

/// 重点提炼面板：流式显示本次会话译文的结构化摘要。
/// 流式期间按行简易渲染（每 ~100ms 全量重渲，不承担完整 Markdown 解析），完成后改用 Markdown 渲染。
/// 支持手动编辑保存与重新提炼；流式生成期间禁用编辑/重新提炼。
export function SummaryPanel({
  text,
  error,
  pending,
  visible,
  canResummarize,
  onResummarize,
  onSave,
  onOpenDir,
}: Props) {
  const bodyRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const widthRef = useRef(DEFAULT_WIDTH);
  const dragRef = useRef<{ startX: number; startW: number } | null>(null);

  // 流式更新时把内容容器自身滚到底（非编辑态）。
  // 不用 scrollIntoView：它会逐个滚动可滚动祖先，首次渲染会把整个面板/头部一起带跑。
  useEffect(() => {
    if (!editing) {
      const el = bodyRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    }
  }, [text, editing]);

  // 重新开始提炼时退出编辑态，避免草稿覆盖新结果
  useEffect(() => {
    if (pending) setEditing(false);
  }, [pending]);

  if (!visible) return null;

  const startEdit = () => {
    setDraft(text);
    setEditing(true);
  };
  const save = () => {
    onSave(draft);
    setEditing(false);
  };

  // 左边缘拖拽改宽：pointer capture 保证拖出面板后仍能收到事件；
  // 手柄必须是 .summary-panel 的子元素，否则会被「点面板外关闭」逻辑误伤。
  // 宽度走 CSS 变量直接写 DOM：摘要渲染后是上百个节点，逐帧 setState 会拖卡。
  const onResizeDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = { startX: e.clientX, startW: widthRef.current };
  };
  const onResizeMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const next = Math.min(
      Math.max(d.startW + (d.startX - e.clientX), DEFAULT_WIDTH),
      window.innerWidth * 0.9
    );
    widthRef.current = next;
    panelRef.current?.style.setProperty("--summary-w", `${next}px`);
  };
  const onResizeUp = (e: React.PointerEvent<HTMLDivElement>) => {
    dragRef.current = null;
    e.currentTarget.releasePointerCapture(e.pointerId);
  };

  return (
    <div
      className="summary-panel"
      ref={panelRef}
      data-editing={editing ? "true" : undefined}
      style={{ width: `min(var(--summary-w, ${DEFAULT_WIDTH}px), 100%)` }}
    >
      <div
        className="summary-resizer"
        onPointerDown={onResizeDown}
        onPointerMove={onResizeMove}
        onPointerUp={onResizeUp}
        onPointerCancel={onResizeUp}
      />
      <div className="summary-head">
        <span className="summary-title">✦ 本次重点</span>
        {pending && <span className="stream-caret">▍</span>}
        <div className="summary-actions">
          {editing ? (
            <>
              <button className="iconbtn" onClick={save} data-tip="保存" data-tip-pos="end">
                <Check size={17} strokeWidth={1.8} />
              </button>
              <button
                className="iconbtn"
                onClick={() => setEditing(false)}
                data-tip="取消"
                data-tip-pos="end"
              >
                <X size={17} strokeWidth={1.8} />
              </button>
            </>
          ) : (
            <>
              <button
                className="iconbtn"
                onClick={startEdit}
                disabled={pending || error || !text}
                data-tip="手动编辑重点内容"
                data-tip-pos="end"
              >
                <Pencil size={16} strokeWidth={1.8} />
              </button>
              <button
                className="iconbtn"
                onClick={onResummarize}
                disabled={pending || !canResummarize}
                data-tip={
                  canResummarize
                    ? "再次调用 LLM 重新提炼"
                    : "需先在设置的「模型库管理」里配置一个 LLM 并选为提炼模型"
                }
                data-tip-pos="end"
              >
                <Wand2 size={16} strokeWidth={1.8} />
              </button>
              <button className="iconbtn" onClick={onOpenDir} data-tip="打开存档文件夹" data-tip-pos="end">
                <FolderOpen size={16} strokeWidth={1.8} />
              </button>
            </>
          )}
        </div>
      </div>
      {editing ? (
        <textarea
          className="summary-edit"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          autoFocus
        />
      ) : (
        <div className="summary-body" ref={bodyRef}>
          {error ? (
            <div className="summary-error">
              <AlertCircle size={16} strokeWidth={2} />
              <span>{text}</span>
            </div>
          ) : text.trim() ? (
            pending ? (
              text.trim().split("\n").map((line, i) => {
                const h = /^(#{1,6})\s+(.*)$/.exec(line);
                if (h) {
                  return h[1].length <= 2 ? (
                    <h4 key={i}>{h[2]}</h4>
                  ) : (
                    <div key={i} className="md-alt-li">
                      {h[2]}
                    </div>
                  );
                }
                if (line.startsWith("- ")) {
                  return (
                    <div key={i} className="summary-li">
                      • {line.slice(2)}
                    </div>
                  );
                }
                if (line.trim() === "") return <div key={i} style={{ height: 6 }} />;
                return <div key={i}>{line}</div>;
              })
            ) : (
              <ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>
                {text.trim()}
              </ReactMarkdown>
            )
          ) : (
            <div className="summary-loading">正在提炼重点…</div>
          )}
        </div>
      )}
    </div>
  );
}
