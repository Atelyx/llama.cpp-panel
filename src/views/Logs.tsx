/** 「日志」页签：进程输出与启动命令预览。终端配色固定深底——跟随主题在浅色下会看不清输出。 */
import React from "react";
import type { HostController, HostSnapshot } from "../host/controller";
import { MAX_LOGS } from "../host/controller";
import { Button, Notice, SCROLL_LIST_CLASS, SectionTitle, bgSecondary, border, textMuted, FONT_SM } from "./ui";

const TERM_BG = "#0d1117";
const TERM_TEXT = "#c9d1d9";
const TERM_DIM = "#8b949e";
const TERM_WARN = "#e3b341";
const TERM_ERROR = "#f85149";
const TERM_OK = "#3fb950";
const TERM_FONT = "ui-monospace, SFMono-Regular, Menlo, monospace";

/**
 * 日志行配色：llama-server 把日志写 stderr，行头形如「! 时间戳 I|W|E 组件」，
 * 级别字母定色（E=错误红、W=警告黄、I/D=正常）；stdout 无级别头按正常。
 * 解析不出的 stderr 行按警告处理——它至少值得注意。
 */
function logTone(line: string): "normal" | "warn" | "error" {
  if (!line.startsWith("! ")) return "normal";
  const m = /^!\s+\S+\s+([IEWD])\b/.exec(line);
  if (!m) return "warn";
  if (m[1] === "E") return "error";
  return m[1] === "W" ? "warn" : "normal";
}

/** 日志行渲染（就绪行给绿色作为直观信号）。 */
function LogLine(props: { line: string }): unknown {
  const head = /^(!\s*\S+\s+)/.exec(props.line);
  const tone = logTone(props.line);
  const color =
    tone === "error"
      ? TERM_ERROR
      : tone === "warn"
        ? TERM_WARN
        : props.line.includes("listening on")
          ? TERM_OK
          : TERM_TEXT;
  return (
    <div style={{ fontFamily: TERM_FONT, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-word", color, lineHeight: 1.5 }}>
      {head ? <span style={{ color: TERM_DIM }}>{head[1]}</span> : null}
      <span>{head ? props.line.slice(head[1].length) : props.line}</span>
    </div>
  );
}

export function LogsView(props: { host: HostController }): unknown {
  const { host } = props;
  const snapshot: HostSnapshot = React.useSyncExternalStore(host.subscribe, host.getSnapshot);

  // 用累计行数（logSeq）而非 logs.length：裁剪到上限后 length 恒定，effect 会失灵；
  // 用户上翻阅读（离底部较远）时不拽回底部，避免阅读中被新日志拽走。
  // 页签每次打开都是新挂载，首帧必须无条件贴底（滚动位置在顶部，atBottom 判据会把用户留在最旧的日志上）。
  const logRef = React.useRef<HTMLDivElement | null>(null);
  const pinned = React.useRef(false);
  const lastLog = snapshot.logSeq;
  React.useEffect(() => {
    const el = logRef.current;
    if (!el || lastLog === 0) return;
    if (!pinned.current) {
      pinned.current = true;
      el.scrollTop = el.scrollHeight;
      return;
    }
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (atBottom) el.scrollTop = el.scrollHeight;
  }, [lastLog]);

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden", background: bgSecondary, color: textMuted, fontSize: FONT_SM, boxSizing: "border-box" }}>
      <SectionTitle
        right={
          <span style={{ display: "flex", alignItems: "center", gap: 8, flex: 1, minWidth: 0 }}>
            {snapshot.startLine ? (
              <span
                title={`启动命令：${snapshot.startLine}`}
                style={{
                  flex: 1,
                  minWidth: 0,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  fontFamily: TERM_FONT,
                  fontSize: 11,
                  color: textMuted,
                  textTransform: "none",
                  letterSpacing: 0,
                }}
              >
                {snapshot.startLine}
              </span>
            ) : null}
            {snapshot.logs.length > 0 ? (
              <Button onClick={() => host.clearLogs()} title="清空日志显示">
                清屏
              </Button>
            ) : null}
          </span>
        }
      >
        日志
      </SectionTitle>
      {snapshot.error && <Notice tone="error">{snapshot.error}</Notice>}
      <div
        ref={logRef}
        style={{ flex: 1, minHeight: 0, overflowY: "auto", scrollbarGutter: "stable", padding: "8px 10px 12px", display: "flex", flexDirection: "column", gap: 2, background: TERM_BG }}
        className={SCROLL_LIST_CLASS}
      >
        {snapshot.logs.length === 0 ? (
          <div style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: 6, color: TERM_DIM, fontSize: FONT_SM, lineHeight: 1.6 }}>
            <div>尚无日志</div>
            <div style={{ fontSize: 11 }}>启动或操作 llama-server 后，这里会滚出它的输出。</div>
          </div>
        ) : (
          snapshot.logs.map((line, i) => <LogLine key={i} line={line} />)
        )}
      </div>
      <div style={{ borderTop: `1px solid ${border}`, padding: "4px 10px", color: textMuted, fontSize: 11 }}>
        {`只保留最近 ${MAX_LOGS} 行`}
      </div>
    </div>
  );
}
