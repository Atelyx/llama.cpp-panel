/**
 * 界面基元：面板共用的样式与小组件，对齐宿主设计系统（docs/plugins/styling.md）。
 * 只用内联 style + 宿主 CSS 变量：token 只读、不判主题；字号走 --fs-* 字阶，随应用字体大小设置缩放。
 * 需要伪类的规则（悬停底色、聚焦环）集中在本文件的样式表里。
 */
import React from "react";

export const textPrimary = "var(--text-primary)";
export const textSecondary = "var(--text-secondary)";
export const textMuted = "var(--text-muted)";
export const border = "var(--border)";
export const borderStrong = "var(--border-strong)";
export const bgPrimary = "var(--bg-primary)";
export const bgSecondary = "var(--bg-secondary)";
export const bgTertiary = "var(--bg-tertiary)";
export const bgSunken = "var(--bg-sunken)";
export const hover = "var(--hover)";
export const accent = "var(--accent)";
export const accentFg = "var(--accent-fg)";
export const accentSoft = "var(--accent-soft)";
export const focusRing = "var(--focus-ring)";
export const success = "var(--success)";
export const warning = "var(--warning)";
export const danger = "var(--danger)";

/** 字阶：面板 UI 文字 / 次要说明 / 徽标，随应用字体大小设置缩放。 */
export const fontUi = "var(--fs-ui)";
export const fontCaption = "var(--fs-caption)";
export const fontMicro = "var(--fs-micro)";
export const fontBody = "var(--fs-body)";
export const fontH2 = "var(--fs-h2)";

/** 滚动区类名（滚动条配色由宿主全局规则接管）。 */
export const SCROLL_LIST_CLASS = "lp-scroll-list";
/** 模型条目类名：悬停底色只能走样式表——行内 background 会压过 `:hover`。 */
export const MODEL_ROW_CLASS = "lp-model-row";
/** 内联 style 表达不了的规则：悬停底色、键盘聚焦环、加载动画。 */
export const PANEL_CSS = `
.lp-model-row:hover { background: var(--hover); }
.lp-focusable:focus-visible { outline: none; box-shadow: var(--focus-ring); }
@keyframes lp-spin { to { transform: rotate(360deg); } }
`;

/** 语义状态 → 圆点/方点 + 颜色：状态不靠颜色单独表达，形状是第二重编码。 */
function statusShape(tone: "ok" | "warn" | "bad" | "idle"): { color: string; round: boolean } {
  if (tone === "ok") return { color: success, round: true };
  if (tone === "warn") return { color: warning, round: true };
  if (tone === "bad") return { color: danger, round: false };
  return { color: textMuted, round: false };
}

export function StatusDot(props: { tone: "ok" | "warn" | "bad" | "idle"; label: string; title?: string }): unknown {
  const { color, round } = statusShape(props.tone);
  return (
    <span title={props.title} style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: fontMicro }}>
      <span
        style={{
          width: 7,
          height: 7,
          borderRadius: round ? 4 : 1,
          background: color,
          flexShrink: 0,
        }}
      />
      <span style={{ color: textSecondary }}>{props.label}</span>
    </span>
  );
}

/** 图标基座（lucide 的 24×24 线性风格）；不引图标库，图标即几条 path。 */
function Svg(props: { size?: number; children?: unknown }): unknown {
  return (
    <svg
      width={props.size ?? 14}
      height={props.size ?? 14}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      {props.children}
    </svg>
  );
}

export function CloseIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <path d="M18 6L6 18" />
      <path d="M6 6l12 12" />
    </Svg>
  );
}

export function PlayIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <path d="M6 4l13 8-13 8V4z" />
    </Svg>
  );
}

export function SquareIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <rect x="6" y="6" width="12" height="12" rx="1" />
    </Svg>
  );
}

export function RefreshIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
      <path d="M21 3v5h-5" />
    </Svg>
  );
}

export function SearchIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <circle cx="11" cy="11" r="8" />
      <path d="M21 21l-4.35-4.35" />
    </Svg>
  );
}

export function ImageIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="8.5" cy="8.5" r="1.5" />
      <path d="M21 15l-5-5L5 21" />
    </Svg>
  );
}

export function DownloadIcon(props: { size?: number }): unknown {
  return (
    <Svg size={props.size}>
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M7 10l5 5 5-5" />
      <path d="M12 15V3" />
    </Svg>
  );
}

/** 顶部工具条：状态在左，动作在右（工具条底色按契约用 --bg-secondary）。 */
export function Toolbar(props: { children: unknown }): unknown {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        flexWrap: "wrap",
        padding: "8px 10px",
        borderBottom: `1px solid ${border}`,
        background: bgSecondary,
        flexShrink: 0,
      }}
    >
      {props.children}
    </div>
  );
}

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

/** 变体 → 底色 / 文字 / 悬停底色（对齐宿主 Button：强调色只给主动作，常规动作用中性）。 */
function buttonStyle(variant: ButtonVariant, isHover: boolean): Record<string, unknown> {
  const base: Record<string, unknown> = {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    gap: 6,
    height: 28,
    padding: "0 12px",
    borderRadius: "var(--radius-sm)",
    fontSize: fontUi,
    fontFamily: "inherit",
    border: "1px solid transparent",
    cursor: "pointer",
    whiteSpace: "nowrap",
    transition: "background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease)",
  };
  if (variant === "primary") {
    return { ...base, background: isHover ? "var(--accent-hover)" : accent, color: accentFg };
  }
  if (variant === "ghost") {
    return { ...base, background: isHover ? hover : "transparent", color: isHover ? textPrimary : textSecondary };
  }
  if (variant === "danger") {
    return {
      ...base,
      background: isHover ? "color-mix(in srgb, var(--danger) 12%, transparent)" : "transparent",
      color: danger,
    };
  }
  return { ...base, background: isHover ? hover : bgTertiary, color: textPrimary, border: `1px solid ${border}` };
}

export function Button(props: {
  children: unknown;
  onClick?: () => void;
  disabled?: boolean;
  title?: string;
  primary?: boolean;
  tone?: "danger";
  /** 小档（24 高）用于工具条密集场景；默认 md（28 高）。 */
  size?: "sm" | "md";
}): unknown {
  const [isHover, setHover] = React.useState(false);
  const variant: ButtonVariant = props.tone === "danger" ? "danger" : props.primary ? "primary" : "secondary";
  const style = buttonStyle(variant, isHover && !props.disabled);
  return (
    <button
      type="button"
      title={props.title}
      disabled={props.disabled}
      onClick={props.disabled ? undefined : props.onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      className="lp-focusable"
      style={{
        ...style,
        ...(props.size === "sm" ? { height: 24, padding: "0 8px", fontSize: fontCaption } : {}),
        ...(props.disabled ? { opacity: 0.5, cursor: "not-allowed" } : {}),
      }}
    >
      {props.children}
    </button>
  );
}

/** 输入框统一取值：下沉底 + 专用边框 + 聚焦环。 */
function inputStyle(focused: boolean): Record<string, unknown> {
  return {
    boxSizing: "border-box",
    width: "100%",
    padding: "4px 8px",
    borderRadius: "var(--radius-sm)",
    border: `1px solid ${focused ? accent : "var(--input-border)"}`,
    boxShadow: focused ? focusRing : undefined,
    background: "var(--input-bg)",
    color: textPrimary,
    fontSize: fontUi,
    fontFamily: "inherit",
    outline: "none",
  };
}

export function TextInput(props: {
  value: string;
  onChange: (value: string) => void;
  /** 回车提交（如检索框）。 */
  onEnter?: () => void;
  placeholder?: string;
  type?: string;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  title?: string;
  style?: Record<string, unknown>;
}): unknown {
  const [focused, setFocused] = React.useState(false);
  return (
    <input
      type={props.type ?? "text"}
      value={props.value}
      placeholder={props.placeholder}
      min={props.min}
      max={props.max}
      step={props.step}
      disabled={props.disabled}
      title={props.title}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
      onChange={(e: { target: { value: string } }) => props.onChange(e.target.value)}
      onKeyDown={(e: { key: string }) => {
        if (e.key === "Enter") props.onEnter?.();
      }}
      className="lp-focusable"
      style={{ ...inputStyle(focused), ...(props.style ?? {}) }}
    />
  );
}

export function Select(props: {
  value: string;
  onChange: (value: string) => void;
  options: Array<{ value: string; label: string }>;
  disabled?: boolean;
  title?: string;
  style?: Record<string, unknown>;
}): unknown {
  const [focused, setFocused] = React.useState(false);
  return (
    <select
      value={props.value}
      disabled={props.disabled}
      title={props.title}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
      onChange={(e: { target: { value: string } }) => props.onChange(e.target.value)}
      className="lp-focusable"
      style={{
        ...inputStyle(focused),
        width: undefined,
        height: 28,
        padding: "0 6px",
        cursor: "pointer",
        ...(props.style ?? {}),
      }}
    >
      {props.options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </select>
  );
}

export function Checkbox(props: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: unknown;
  disabled?: boolean;
  title?: string;
}): unknown {
  return (
    <label
      title={props.title}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        fontSize: fontUi,
        color: textSecondary,
        cursor: props.disabled ? "not-allowed" : "pointer",
        opacity: props.disabled ? 0.6 : 1,
      }}
    >
      <input
        type="checkbox"
        checked={props.checked}
        disabled={props.disabled}
        onChange={(e: { target: { checked: boolean } }) => props.onChange(e.target.checked)}
        style={{ accentColor: accent, cursor: "inherit" }}
      />
      {props.label}
    </label>
  );
}

/** 区块标题：小字距 + 大写（字号走 --fs-micro）。 */
export function SectionTitle(props: { children: unknown; right?: unknown }): unknown {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        gap: 8,
        padding: "6px 10px",
        fontSize: fontMicro,
        letterSpacing: 0.4,
        textTransform: "uppercase",
        color: textMuted,
        flexShrink: 0,
      }}
    >
      {/* 左标签锁 flexShrink：右侧放长内容时标题不许被挤成竖排 */}
      <span style={{ flexShrink: 0 }}>{props.children}</span>
      {props.right}
    </div>
  );
}

/** 徽标 tone：语义底色一律 color-mix 半透明，border 为同色加深。 */
type BadgeTone = "default" | "accent" | "ok" | "warn" | "danger";

function badgeStyle(tone: BadgeTone): { background: string; color: string; borderColor: string } {
  if (tone === "accent") {
    return { background: accentSoft, color: accent, borderColor: "color-mix(in srgb, var(--accent) 35%, transparent)" };
  }
  const semantic = tone === "ok" ? success : tone === "warn" ? warning : tone === "danger" ? danger : null;
  if (semantic) {
    return {
      background: `color-mix(in srgb, ${semantic} 12%, transparent)`,
      color: semantic,
      borderColor: `color-mix(in srgb, ${semantic} 32%, transparent)`,
    };
  }
  return { background: bgTertiary, color: textSecondary, borderColor: border };
}

/** 徽标：计数、状态、作用域标记等短文本；数字传 mono 便于纵向扫读。 */
export function Chip(props: { children: unknown; title?: string; tone?: BadgeTone; mono?: boolean }): unknown {
  const t = badgeStyle(props.tone ?? "default");
  return (
    <span
      title={props.title}
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 4,
        padding: "1px 6px",
        borderRadius: "var(--radius-xs)",
        border: `1px solid ${t.borderColor}`,
        background: t.background,
        fontSize: fontMicro,
        lineHeight: 1.7,
        color: t.color,
        whiteSpace: "nowrap",
        overflow: "hidden",
        textOverflow: "ellipsis",
        flexShrink: 0,
        ...(props.mono ? { fontFamily: "var(--font-mono)" } : {}),
      }}
    >
      {props.children}
    </span>
  );
}

export function Notice(props: { tone: "info" | "warn" | "error"; children: unknown; onClose?: () => void }): unknown {
  const semantic = props.tone === "error" ? danger : props.tone === "warn" ? warning : textSecondary;
  return (
    <div
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: 8,
        margin: "4px 10px 8px",
        padding: "8px 10px",
        borderRadius: "var(--radius-sm)",
        border: `1px solid color-mix(in srgb, ${semantic} 40%, transparent)`,
        background: `color-mix(in srgb, ${semantic} 10%, transparent)`,
        fontSize: fontCaption,
        color: textPrimary,
        lineHeight: 1.5,
      }}
    >
      <div style={{ flex: 1, minWidth: 0, wordBreak: "break-word" }}>{props.children}</div>
      {props.onClose ? (
        <button
          type="button"
          onClick={props.onClose}
          title="关闭"
          style={{
            display: "inline-flex",
            border: "none",
            background: "transparent",
            color: textMuted,
            cursor: "pointer",
            padding: 0,
          }}
        >
          <CloseIcon size={14} />
        </button>
      ) : null}
    </div>
  );
}

/** 空态：图标块 + 标题 + 说明——说明承载「为什么空」与「下一步做什么」。 */
export function Empty(props: { children: unknown; hint?: unknown; icon?: unknown }): unknown {
  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 8,
        padding: 24,
        textAlign: "center",
        color: textMuted,
        fontSize: fontUi,
        lineHeight: 1.6,
      }}
    >
      {props.icon ? (
        <div
          style={{
            width: 40,
            height: 40,
            borderRadius: "var(--radius-md)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            background: bgTertiary,
            color: textMuted,
          }}
        >
          {props.icon}
        </div>
      ) : null}
      <div style={{ color: textPrimary, fontWeight: 500 }}>{props.children}</div>
      {props.hint ? <div style={{ fontSize: fontCaption, maxWidth: "42ch" }}>{props.hint}</div> : null}
    </div>
  );
}

/** 加载指示：不确定进度。 */
export function Spinner(props: { size?: number }): unknown {
  const size = props.size ?? 14;
  return (
    <span
      aria-hidden
      style={{
        display: "inline-block",
        width: size,
        height: size,
        borderRadius: "50%",
        border: `${Math.max(1.5, Math.round(size / 8))}px solid ${border}`,
        borderTopColor: accent,
        animation: "lp-spin 800ms linear infinite",
        flexShrink: 0,
      }}
    />
  );
}

/** 确定进度条（0–100）；总量未知时不要用它，文字进度已能反映推进。 */
export function ProgressBar(props: { value: number }): unknown {
  const pct = Math.max(0, Math.min(100, props.value));
  return (
    <div style={{ width: "100%", height: 4, borderRadius: 2, overflow: "hidden", background: bgSunken }}>
      <div
        style={{
          width: `${pct}%`,
          height: "100%",
          background: accent,
          transition: "width var(--dur-base) var(--ease)",
        }}
      />
    </div>
  );
}

export function Field(props: { label?: string; hint?: unknown; children: unknown }): unknown {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 12 }}>
      <div style={{ fontSize: fontUi, color: textPrimary }}>{props.label}</div>
      {props.hint ? (
        <div style={{ fontSize: fontCaption, color: textMuted, lineHeight: 1.5 }}>{props.hint}</div>
      ) : null}
      {props.children}
    </div>
  );
}

export function Card(props: { title: string; children: unknown; actions?: unknown }): unknown {
  return (
    <div
      style={{
        border: `1px solid ${border}`,
        borderRadius: "var(--radius-md)",
        background: "var(--bg-card)",
        padding: 12,
        marginBottom: 12,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
          marginBottom: 10,
        }}
      >
        <div style={{ fontSize: "var(--fs-h2)", color: textPrimary, fontWeight: 600 }}>{props.title}</div>
        {props.actions}
      </div>
      {props.children}
    </div>
  );
}

/**
 * 面板内两栏（列表 + 详情）的响应式分栏：容器窄于两倍栏宽时上下堆叠。
 * 测量挂在使用方的容器 ref 上，ResizeObserver 驱动。
 */
export function useSplitLayout(collapseAt: number): { ref: { current: HTMLDivElement | null }; stacked: boolean } {
  const ref = React.useRef<HTMLDivElement | null>(null);
  const [stacked, setStacked] = React.useState(false);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = (): void => setStacked(el.clientWidth < collapseAt);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [collapseAt]);
  return { ref, stacked };
}
