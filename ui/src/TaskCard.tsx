import { memo, useState } from "react";
import type { Task } from "./types";
import {
  formatBytes,
  formatEta,
  formatPercent,
  formatSpeed,
  fileKind,
  fileExt,
  STATE_LABEL,
  cx,
} from "./format";
import {
  IconArchive,
  IconCheck,
  IconCode,
  IconDownload,
  IconFile,
  IconFolder,
  IconImage,
  IconMusic,
  IconPause,
  IconPlay,
  IconTrash,
  IconVideo,
  IconX,
  IconZap,
} from "./icons";

interface Props {
  task: Task;
  onPause: (id: string) => void;
  onResume: (id: string) => void;
  onRemove: (id: string) => void;
  onOpen: (task: Task) => void;
  onReveal: (task: Task) => void;
}

const KIND_ICON = {
  video: IconVideo,
  audio: IconMusic,
  image: IconImage,
  archive: IconArchive,
  doc: IconFile,
  app: IconZap,
  code: IconCode,
  other: IconFile,
} as const;

function TaskCardInner({ task, onPause, onResume, onRemove, onOpen, onReveal }: Props) {
  const [confirmDel, setConfirmDel] = useState(false);
  const kind = fileKind(task.file_name);
  const Icon = KIND_ICON[kind];
  const pct = task.size ? Math.min(1, task.downloaded / task.size) : 0;
  const isActive = task.state === "downloading" || task.state === "probing";
  const isDone = task.state === "completed";

  const stateClass =
    task.state === "completed"
      ? "state-completed"
      : task.state === "paused"
        ? "state-paused"
        : task.state === "error"
          ? "state-error"
          : task.state === "downloading"
            ? "state-downloading"
            : "state-queued";

  return (
    <div className={cx("task-card", isActive && "active", task.state === "error" && "error")}>
      <div className="task-row">
        <div className={cx("file-badge", kind)}>
          {isDone ? <IconCheck size={18} /> : <Icon size={17} />}
        </div>

        <div className="task-main">
          <div className="task-head">
            <div className="task-name" title={task.file_name}>
              {task.file_name}
            </div>
            <span className={cx("task-state-badge", stateClass)}>
              <span className="dot" />
              {STATE_LABEL[task.state] ?? task.state}
            </span>
          </div>

          <div className="task-meta">
            <span>
              {formatBytes(task.downloaded)}
              {task.size ? ` / ${formatBytes(task.size)}` : ""}
            </span>
            {isActive && task.speed > 0 && (
              <>
                <span className="sep" />
                <span className="speed">{formatSpeed(task.speed)}</span>
                <span className="sep" />
                <span>剩余 {formatEta(task.downloaded, task.size, task.speed)}</span>
              </>
            )}
            {task.state === "paused" && (
              <>
                <span className="sep" />
                <span>{task.connections} 线程 · 可续传</span>
              </>
            )}
            {task.state === "scheduled" && task.scheduled_at && (
              <>
                <span className="sep" />
                <span>
                  {new Date(task.scheduled_at).toLocaleString("zh-CN", {
                    month: "2-digit",
                    day: "2-digit",
                    hour: "2-digit",
                    minute: "2-digit",
                  })}{" "}
                  开始
                </span>
              </>
            )}
            {isDone && (
              <>
                <span className="sep" />
                <span>{task.connections} 线程</span>
              </>
            )}
          </div>

          {task.error && <div className="task-error-msg">⚠ {task.error}</div>}

          {/* 进度区：一条进度条搞定。下载中时用分片刻度线画出「多线程并行」的细节，
              既保留信息又不制造第二根进度条 */}
          {!isDone && task.state !== "error" && (
            <div className="progress-wrap">
              <div className="progress-track">
                <div
                  className={cx("progress-fill", !isActive && "paused")}
                  style={{ width: `${Math.max(pct * 100, pct > 0 ? 1.5 : 0)}%` }}
                />
                {/* 分片分隔刻度：贴在进度条上，随分片完成度点亮 */}
                {isActive && task.segments.length > 1 && (
                  <div className="segment-ticks">
                    {task.segments.map((seg) => {
                      const len = seg.end - seg.start + 1;
                      const sp = len > 0 ? Math.min(1, seg.done / len) : 0;
                      return (
                        <div
                          key={seg.index}
                          className={cx("tick", sp >= 1 && "full")}
                          style={{ flex: len || 1 }}
                          title={`分片 ${seg.index + 1}: ${(sp * 100).toFixed(0)}%`}
                        />
                      );
                    })}
                  </div>
                )}
              </div>
              <span className="progress-pct">{formatPercent(pct)}</span>
            </div>
          )}

        </div>

        <div className="task-actions">
          {isActive && (
            <button
              className="icon-btn"
              onClick={() => onPause(task.id)}
              title="暂停"
              aria-label="暂停"
            >
              <IconPause />
            </button>
          )}
          {(task.state === "paused" || task.state === "error" || task.state === "queued" || task.state === "scheduled") && (
            <button
              className="icon-btn"
              onClick={() => onResume(task.id)}
              title="开始下载"
              aria-label="开始下载"
            >
              <IconPlay />
            </button>
          )}
          {isDone && (
            <>
              <button
                className="icon-btn"
                onClick={() => onOpen(task)}
                title="打开文件"
                aria-label="打开文件"
              >
                <IconDownload />
              </button>
              <button
                className="icon-btn"
                onClick={() => onReveal(task)}
                title="打开所在文件夹"
                aria-label="打开所在文件夹"
              >
                <IconFolder />
              </button>
            </>
          )}
          <button
            className={cx("icon-btn", confirmDel && "danger")}
            onClick={() => {
              if (confirmDel) {
                onRemove(task.id);
                setConfirmDel(false);
              } else {
                setConfirmDel(true);
                setTimeout(() => setConfirmDel(false), 2600);
              }
            }}
            title={confirmDel ? "再次点击确认删除" : "删除任务"}
            aria-label="删除任务"
          >
            {confirmDel ? <IconX /> : <IconTrash />}
          </button>
        </div>
      </div>
    </div>
  );
}

export const TaskCard = memo(TaskCardInner);
