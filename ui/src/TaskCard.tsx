import { memo, useState } from "react";
import type { Task } from "./types";
import {
  formatBytes,
  formatEta,
  formatPercent,
  formatSpeed,
  fileKind,
  STATE_KEY,
  cx,
} from "./format";
import { useI18n } from "./i18n";
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
  const { t, lang } = useI18n();
  const [confirmDel, setConfirmDel] = useState(false);
  const kind = task.kind === "bt" ? "other" : fileKind(task.file_name);
  const Icon = KIND_ICON[kind];
  const pct = task.size ? Math.min(1, task.downloaded / task.size) : 0;
  const isActive = task.state === "downloading" || task.state === "probing";
  const isDone = task.state === "completed";
  const isBt = task.kind === "bt";
  const isHls = task.kind === "hls";

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
              {(isBt || isHls) && (
                <span className={cx("kind-badge", task.kind)}>
                  {isBt ? "BT" : "HLS"}
                </span>
              )}
              {task.file_name}
            </div>
            <span className={cx("task-state-badge", stateClass)}>
              <span className="dot" />
              {t(STATE_KEY[task.state] ?? task.state)}
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
              </>
            )}
            {isBt && task.upload_speed > 0 && (
              <>
                <span className="sep" />
                <span className="speed">
                  {t("state.upload")} {formatSpeed(task.upload_speed)}
                </span>
              </>
            )}
            {isBt && task.peers > 0 && (
              <>
                <span className="sep" />
                <span>
                  {task.peers} {t("state.peers")}
                </span>
              </>
            )}
            {isActive && !isBt && !isHls && task.speed > 0 && (
              <>
                <span className="sep" />
                <span>
                  {t("state.eta")} {formatEta(task.downloaded, task.size, task.speed, lang)}
                </span>
              </>
            )}
            {task.state === "paused" && (
              <>
                <span className="sep" />
                <span>
                  {task.connections > 0
                    ? `${task.connections} ${t("state.threads")} · `
                    : ""}
                  {t("state.resumable")}
                </span>
              </>
            )}
            {task.mirrors.length > 0 && (
              <>
                <span className="sep" />
                <span title={task.mirrors.join("\n")}>
                  +{task.mirrors.length} {t("state.mirrors")}
                </span>
              </>
            )}
            {task.state === "scheduled" && task.scheduled_at && (
              <>
                <span className="sep" />
                <span>
                  {new Date(task.scheduled_at).toLocaleString(lang === "zh" ? "zh-CN" : "en-US", {
                    month: "2-digit",
                    day: "2-digit",
                    hour: "2-digit",
                    minute: "2-digit",
                  })}{" "}
                  {t("task.startAt")}
                </span>
              </>
            )}
            {isDone && !isBt && !isHls && task.connections > 0 && (
              <>
                <span className="sep" />
                <span>
                  {task.connections} {t("state.threads")}
                </span>
              </>
            )}
          </div>

          {task.error && <div className="task-error-msg">⚠ {task.error}</div>}

          {/* 进度区：一条进度条搞定。下载中时用分片刻度线画出「多线程并行」的细节 */}
          {!isDone && task.state !== "error" && (
            <div className="progress-wrap">
              <div className="progress-track">
                <div
                  className={cx("progress-fill", !isActive && "paused")}
                  style={{ width: `${Math.max(pct * 100, pct > 0 ? 1.5 : 0)}%` }}
                />
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
                          title={`${t("state.segment")} ${seg.index + 1}: ${(sp * 100).toFixed(0)}%`}
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
              title={t("task.pause")}
              aria-label={t("task.pause")}
            >
              <IconPause />
            </button>
          )}
          {(task.state === "paused" || task.state === "error" || task.state === "queued" || task.state === "scheduled") && (
            <button
              className="icon-btn"
              onClick={() => onResume(task.id)}
              title={t("task.start")}
              aria-label={t("task.start")}
            >
              <IconPlay />
            </button>
          )}
          {isDone && (
            <>
              <button
                className="icon-btn"
                onClick={() => onOpen(task)}
                title={t("task.open")}
                aria-label={t("task.open")}
              >
                <IconDownload />
              </button>
              <button
                className="icon-btn"
                onClick={() => onReveal(task)}
                title={t("task.folder")}
                aria-label={t("task.folder")}
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
            title={confirmDel ? t("task.delConfirm") : t("task.del")}
            aria-label={t("task.del")}
          >
            {confirmDel ? <IconX /> : <IconTrash />}
          </button>
        </div>
      </div>
    </div>
  );
}

export const TaskCard = memo(TaskCardInner);
