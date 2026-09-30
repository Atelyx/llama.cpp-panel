/** 下载任务卡片：状态、进度条、速度与暂停/继续/收起。进度与速度由快照的字节数变化推算，非推送。 */
import React from "react";
import { formatSize, type DownloadTask } from "../host/types";
import { Button, accent, border, textMuted } from "./ui";

/** 进度百分比（总量未知时返回 null）。 */
function percentOf(bytes: number, total: number | null): number | null {
  if (!total || total <= 0) return null;
  return Math.min(100, Math.round((bytes / total) * 100));
}

function statusLabel(status: DownloadTask["status"]): string {
  if (status === "downloading") return "下载中";
  if (status === "paused") return "已暂停";
  if (status === "done") return "已完成";
  if (status === "error") return "下载失败";
  return "待下载";
}

/** 进度文案：已下/总量 + 百分比 + 速度。 */
function progressText(task: DownloadTask, speed: number): string {
  const total = task.totalBytes ? formatSize(task.totalBytes) : "未知大小";
  const pct = percentOf(task.bytes, task.totalBytes);
  const speedText = speed > 0 ? ` · ${formatSize(speed)}/s` : "";
  return `${formatSize(task.bytes)} / ${total}${pct === null ? "" : `（${pct}%）`}${speedText}`;
}

/** 两个采样点之间的平均速度。用 effect 而非渲染期写 ref——React 允许重复渲染同一帧，会算错速度。 */
function useSpeed(task: DownloadTask): number {
  const ref = React.useRef<{ bytes: number; at: number; speed: number }>({ bytes: 0, at: 0, speed: 0 });
  const [speed, setSpeed] = React.useState(0);
  React.useEffect(() => {
    const now = Date.now();
    const prev = ref.current;
    if (task.status !== "downloading") {
      ref.current = { bytes: task.bytes, at: now, speed: 0 };
      setSpeed(0);
      return;
    }
    // 第一个采样点只立基准：不写回基准点的话 prev.at 恒为 0，速度永远算不出来
    if (prev.at === 0) {
      ref.current = { bytes: task.bytes, at: now, speed: 0 };
      return;
    }
    const dt = (now - prev.at) / 1000;
    // 采样间隔过短时保留上次结果，避免把采样抖动放大成假速度
    if (dt < 0.3) return;
    const next = Math.max(0, (task.bytes - prev.bytes) / dt);
    ref.current = { bytes: task.bytes, at: now, speed: next };
    setSpeed(next);
  }, [task.bytes, task.status]);
  return speed;
}

export function DownloadCard(props: {
  task: DownloadTask;
  onCancel: () => void;
  onResume: () => void;
  onDismiss: () => void;
}): unknown {
  const { task } = props;
  const speed = useSpeed(task);
  const pct = percentOf(task.bytes, task.totalBytes);
  const isPaused = task.status === "paused";
  return (
    <>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={task.target}>
          {`${statusLabel(task.status)} · ${task.file}`}
        </span>
        {task.status === "downloading" ? (
          <Button tone="danger" onClick={props.onCancel} title="结束下载进程，已下载部分保留可续传">
            暂停
          </Button>
        ) : null}
        {isPaused || task.status === "error" ? (
          <Button primary onClick={props.onResume} title="从已下载的部分继续">
            继续
          </Button>
        ) : null}
        {/* 暂停时同时给「继续」与「放弃」两个互斥动作，不再叠一个等价的「收起」 */}
        {task.status === "done" || task.status === "error" ? (
          <Button onClick={props.onDismiss} title="收起这张卡片">
            收起
          </Button>
        ) : null}
        {isPaused ? (
          <Button tone="danger" onClick={props.onDismiss} title="收起卡片；已下载的临时文件留在模型目录，可再次下载续传">
            放弃
          </Button>
        ) : null}
      </div>
      {/* 总量未知时不画进度条：定死在 0% 会误导，下方的字节数文本已能反映推进 */}
      {(task.status === "downloading" || isPaused) && pct !== null ? (
        <div style={{ height: 6, borderRadius: 3, background: border, overflow: "hidden" }}>
          <div style={{ width: `${pct}%`, height: "100%", background: accent, transition: "width 300ms linear" }} />
        </div>
      ) : null}
      <div style={{ color: task.status === "error" ? "#e5534b" : textMuted, fontSize: 11, wordBreak: "break-word" }}>
        {task.status === "downloading" ? progressText(task, speed) : task.statusText}
      </div>
    </>
  );
}
