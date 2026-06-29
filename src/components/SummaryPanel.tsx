import { useEffect, useRef, useState } from "react";
import { Pencil, Wand2, Check, X, AlertCircle, FolderOpen } from "lucide-react";

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

/// 重点提炼面板：流式显示本次会话译文的结构化摘要（Markdown 文本，按行简易渲染）。
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
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

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

  return (
    <div className="summary-panel" data-editing={editing ? "true" : undefined}>
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
                    : "重新提炼需 LLM 引擎（OpenAI / Ollama）"
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
            text.trim().split("\n").map((line, i) => {
              if (line.startsWith("## ")) {
                return <h4 key={i}>{line.slice(3)}</h4>;
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
            <div className="summary-loading">正在提炼重点…</div>
          )}
        </div>
      )}
    </div>
  );
}
