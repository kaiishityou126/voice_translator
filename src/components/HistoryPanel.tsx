import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { History, FolderOpen, RefreshCw } from "lucide-react";
import { SessionMeta } from "../types";

interface Props {
  visible: boolean;
  // 当前是否运行中：运行时不允许进入回看，避免与直播字幕冲突
  running: boolean;
  // 选中某历史会话回看
  onOpen: (s: SessionMeta) => void;
  // 打开存档文件夹
  onOpenDir: () => void;
}

// 会话 stem「YYYY-MM-DD_HHmmss」→ 友好显示「YYYY-MM-DD HH:MM」
function fmtStem(stem: string): string {
  const m = stem.match(/^(\d{4}-\d{2}-\d{2})_(\d{2})(\d{2})(\d{2})$/);
  if (!m) return stem;
  return `${m[1]} ${m[2]}:${m[3]}`;
}

/// 历史会话面板：列出所有 .jsonl 转写会话，点击进入只读回看。
export function HistoryPanel({ visible, running, onOpen, onOpenDir }: Props) {
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    setLoading(true);
    setError(null);
    invoke<SessionMeta[]>("list_sessions")
      .then((list) => setSessions(list))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  // 面板打开时拉取列表
  useEffect(() => {
    if (visible) refresh();
  }, [visible]);

  if (!visible) return null;

  return (
    <div className="history-panel">
      <div className="history-head">
        <span className="history-title">
          <History size={16} strokeWidth={1.9} /> 历史会话
        </span>
        <div className="history-actions">
          <button className="iconbtn" onClick={refresh} data-tip="刷新" data-tip-pos="end">
            <RefreshCw size={16} strokeWidth={1.8} />
          </button>
          <button className="iconbtn" onClick={onOpenDir} data-tip="打开存档文件夹" data-tip-pos="end">
            <FolderOpen size={16} strokeWidth={1.8} />
          </button>
        </div>
      </div>

      <div className="history-body">
        {running && (
          <div className="history-hint">运行中无法回看，停止后再查看历史会话。</div>
        )}
        {loading && <div className="history-empty">加载中…</div>}
        {error && <div className="history-empty err">读取失败：{error}</div>}
        {!loading && !error && sessions.length === 0 && (
          <div className="history-empty">还没有历史会话记录。</div>
        )}
        {!loading &&
          !error &&
          sessions.map((s) => (
            <button
              key={s.path}
              className="history-item"
              disabled={running}
              onClick={() => onOpen(s)}
              title={running ? "运行中无法回看" : "回看此会话"}
            >
              <div className="history-item-top">
                <span className="history-item-date">{fmtStem(s.stem)}</span>
                <span className="history-item-count">{s.count} 段</span>
              </div>
              {s.preview && <div className="history-item-prev">{s.preview}</div>}
            </button>
          ))}
      </div>
    </div>
  );
}
