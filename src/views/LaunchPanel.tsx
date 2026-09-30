/**
 * 启动面板：按文件夹选模型、表单式启动参数（按模型文件夹持久化）、托管启停、状态与日志。
 *
 * 一个模型 = 一个文件夹：里面是同一模型的不同量化（*.gguf）与可选 mmproj 视觉投影。
 * 扫描结果按文件夹聚合；选中文件夹即带出该模型持久化的启动参数表单，换模型即换参数。
 * 消费两层快照：连接（runtime，网络层）与进程（host，本机层）。
 */
import React from "react";
import type { AtelyxCtx } from "../ctx";
import type { LlamaRuntime } from "../runtime";
import {
  normalizeDir,
  paramsFor,
  type LlamaSettings,
  type ModelLaunchParams,
} from "../settings";
import { type HostController, type HostSnapshot } from "../host/controller";
import {
  dirNameOf,
  joinModelPath,
  listModels,
  type ModelEntry,
} from "../host/models";
import {
  SCROLL_LIST_CLASS,
  SCROLLBAR_CSS,
  SearchIcon,
  PlayIcon,
  SquareIcon,
  RefreshIcon,
  ImageIcon,
  Button,
  Checkbox,
  Chip,
  Empty,
  Field,
  Notice,
  SectionTitle,
  Select,
  StatusDot,
  TextInput,
  Toolbar,
  accent,
  bgPrimary,
  bgSecondary,
  border,
  hover,
  textMuted,
  textPrimary,
  FONT_SM,
} from "./ui";

interface LaunchPanelProps {
  ctx: AtelyxCtx;
  runtime: LlamaRuntime;
  host: HostController;
  settings: LlamaSettings;
  onSettingsChanged: (next: LlamaSettings) => void;
}

/** 一个模型文件夹：同目录的量化文件与视觉投影聚在一起。 */
interface ModelFolder {
  /** 相对 modelsDir 的目录路径。 */
  rel: string;
  /** 全路径（/ 分隔）。 */
  full: string;
  quants: ModelEntry[];
  mms: ModelEntry[];
}

/** 从全路径取文件名（兼容正/反斜杠）。 */
function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() ?? "";
}

/**
 * 扫描条目名相对模型根目录（自带文件夹前缀），聚合进文件夹后须剥掉前缀才是
 * 相对所选文件夹的文件路径——直接拿去拼全路径会得到中段重复的死路径。
 */
function folderRelName(folder: ModelFolder, entry: ModelEntry): string {
  return folder.rel ? entry.name.slice(folder.rel.length + 1) : entry.name;
}

/**
 * 字节 → 人类可读大小，十进制（GB = 10⁹ 字节），与 HuggingFace 等模型发布页的标注
 * 惯例一致，方便直接对数量化文件体积。宿主给的是字节（ListDirEntry.size）。
 */
function formatSize(bytes: number | undefined): string {
  if (bytes == null) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i += 1;
  }
  return `${i === 0 ? v : v.toFixed(1)} ${units[i]}`;
}

/**
 * 终端风格固定配色：日志体不跟随主题（终端必然深底），浅色主题下同样可读。
 */
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

/** 数字输入 → 可选整数；空/非法 = null（不传该参数）。 */
function optionalInt(v: string, min: number): number | null {
  const n = parseInt(v, 10);
  return Number.isInteger(n) && n >= min ? n : null;
}

/** 模型栏与参数栏并排所需的最小宽度；低于它改为上下堆叠。 */
const TWO_COLUMN_MIN_WIDTH = 640;

/**
 * 按面板自身宽度决定上下两栏是并排还是堆叠：并排需要横向空间，窄到放不下时
 * 参数表单里的两列输入会被压成读不成的窄条，还不如上下堆叠。观察的是面板宽度
 * 而不是视口宽度——同一台机器上面板可停靠在不同宽度的布局里（窄面板与手机端）。
 * 首帧用 layout effect 量一次，避免窄面板先闪一帧并排。
 */
function useStackedLayout(): { ref: { current: HTMLDivElement | null }; isStacked: boolean } {
  const ref = React.useRef<HTMLDivElement | null>(null);
  const [isStacked, setIsStacked] = React.useState(false);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = (): void => setIsStacked(el.clientWidth < TWO_COLUMN_MIN_WIDTH);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return { ref, isStacked };
}

export function LaunchPanel(props: LaunchPanelProps): unknown {
  const { ctx, runtime, host, settings, onSettingsChanged } = props;

  const runtimeSnapshot = React.useSyncExternalStore(runtime.subscribe, runtime.getSnapshot);
  const hostSnapshot: HostSnapshot = React.useSyncExternalStore(host.subscribe, host.getSnapshot);
  const { ref: layoutRef, isStacked } = useStackedLayout();

  const online = runtimeSnapshot.channel === "direct";
  const isExternal = settings.processMode === "external";

  // ---- 模型目录扫描（modelsDir 变化时重扫，按钮可手动重扫，seq 防乱序） ----
  const [models, setModels] = React.useState<ModelEntry[]>([]);
  const [projectors, setProjectors] = React.useState<ModelEntry[]>([]);
  const [modelsLoading, setModelsLoading] = React.useState(false);
  const [modelsError, setModelsError] = React.useState("");
  const [modelFilter, setModelFilter] = React.useState("");
  const scanSeq = React.useRef(0);

  const runScan = React.useCallback(async (): Promise<void> => {
    const mySeq = ++scanSeq.current;
    setModelsLoading(true);
    setModelsError("");
    const result = await listModels(ctx, settings.modelsDir);
    if (mySeq !== scanSeq.current) return; // 被更新的扫描取代，丢弃结果
    setModelsLoading(false);
    if (result.ok) {
      setModels(result.models);
      setProjectors(result.projectors);
      if (result.capped)
        setModelsError("模型较多，只返回了一部分；用过滤器收窄。");
    } else {
      setModelsError(result.error);
    }
  }, [ctx, settings.modelsDir]);

  React.useEffect(() => {
    runScan();
  }, [runScan]);

  // ---- 按文件夹聚合：文件夹 = 含至少一个非 mmproj 的 gguf；投影挂到同目录文件夹 ----
  const folders = React.useMemo<ModelFolder[]>(() => {
    const map = new Map<string, { quants: ModelEntry[]; mms: ModelEntry[] }>();
    const bucket = (dir: string): { quants: ModelEntry[]; mms: ModelEntry[] } => {
      let b = map.get(dir);
      if (!b) {
        b = { quants: [], mms: [] };
        map.set(dir, b);
      }
      return b;
    };
    for (const m of models) bucket(dirNameOf(m.name)).quants.push(m);
    for (const p of projectors) {
      const dir = dirNameOf(p.name);
      // 没有量化文件的目录不成为模型文件夹（孤零零的投影无处可挂）
      if (map.has(dir)) bucket(dir).mms.push(p);
    }
    return [...map.entries()]
      .map(([rel, b]) => ({
        rel,
        full: joinModelPath(settings.modelsDir, rel),
        quants: b.quants,
        mms: b.mms,
      }))
      .sort((a, b) => a.rel.localeCompare(b.rel));
  }, [models, projectors, settings.modelsDir]);

  const filtered = React.useMemo(() => {
    const needle = modelFilter.trim().toLowerCase();
    if (!needle) return folders;
    return folders.filter((f) => f.rel.toLowerCase().includes(needle));
  }, [folders, modelFilter]);

  // ---- 选中模型与参数表单 ----
  // 当前选中 = 启动字段（modelFile）反推的模型文件夹；表单读写该文件夹的持久化参数
  const selectedFolder =
    folders.find((f) => normalizeDir(f.full) === normalizeDir(dirNameOf(settings.modelFile))) ?? null;
  const params = selectedFolder ? paramsFor(settings, selectedFolder.full) : null;

  const selectFolder = (f: ModelFolder): void => {
    const prev = paramsFor(settings, f.full);
    // 量化/投影的持久化值相对文件夹；记录缺失或文件已不在时落到第一个
    const quant = f.quants.some((q) => folderRelName(f, q) === prev.quant)
      ? prev.quant
      : f.quants[0]
        ? folderRelName(f, f.quants[0])
        : "";
    const mmproj = f.mms.some((m) => folderRelName(f, m) === prev.mmproj)
      ? prev.mmproj
      : f.mms[0]
        ? folderRelName(f, f.mms[0])
        : "";
    onSettingsChanged({
      ...settings,
      modelFile: quant ? joinModelPath(f.full, quant) : "",
      mmprojFile: mmproj ? joinModelPath(f.full, mmproj) : "",
      modelParams: { ...settings.modelParams, [normalizeDir(f.full)]: { ...prev, quant, mmproj } },
    });
  };
  const clearSelection = (): void => {
    onSettingsChanged({ ...settings, modelFile: "", mmprojFile: "" });
  };
  const updateParams = (changes: Partial<ModelLaunchParams>): void => {
    if (!selectedFolder) return;
    const dirKey = normalizeDir(selectedFolder.full);
    const next: ModelLaunchParams = { ...paramsFor(settings, selectedFolder.full), ...changes };
    onSettingsChanged({
      ...settings,
      modelFile: next.quant ? joinModelPath(selectedFolder.full, next.quant) : settings.modelFile,
      mmprojFile: next.mmproj ? joinModelPath(selectedFolder.full, next.mmproj) : "",
      modelParams: { ...settings.modelParams, [dirKey]: next },
    });
  };

  // ---- 启停 ----
  const canStart = !isExternal && !hostSnapshot.running && !hostSnapshot.starting;
  const canStop = !isExternal && (hostSnapshot.running || hostSnapshot.starting);
  const start = React.useCallback(() => void host.start(runtime), [host, runtime]);
  const stop = React.useCallback(() => void host.stop(), [host]);

  // ---- 日志自动滚动 ----
  // 用累计行数（logSeq）而非 logs.length：裁剪到上限后 length 恒定，effect 会失灵；
  // 用户上翻阅读（离底部较远）时不拽回底部，避免阅读中被新日志拽走。
  const logRef = React.useRef<HTMLDivElement | null>(null);
  const lastLog = hostSnapshot.logSeq;
  React.useEffect(() => {
    const el = logRef.current;
    if (!el || lastLog === 0) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (atBottom) el.scrollTop = el.scrollHeight;
  }, [lastLog]);

  // ---- 状态文案 ----
  function processStatus(): { tone: "ok" | "warn" | "bad" | "idle"; label: string; title: string } {
    if (hostSnapshot.starting)
      return { tone: "warn", label: "启动中", title: "进程已拉起，正在等待 llama-server 接口就绪" };
    if (hostSnapshot.running)
      return { tone: "ok", label: "运行中", title: "本插件启动的 llama-server 正在运行" };
    if (hostSnapshot.error) return { tone: "bad", label: "启动失败", title: hostSnapshot.error };
    if (isExternal) return { tone: "idle", label: "未接管", title: "外部模式：进程归你管，只看连接状态" };
    return { tone: "idle", label: "已停止", title: "未运行" };
  }
  const procStatus = processStatus();

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden", background: bgPrimary, color: textPrimary, fontSize: FONT_SM, boxSizing: "border-box" }}>
      <style>{SCROLLBAR_CSS}</style>

      <Toolbar>
        <StatusDot
          tone={online ? "ok" : "idle"}
          label={online ? `已连接 · ${settings.host}:${settings.port}` : `未连接 · ${settings.host}:${settings.port}`}
          title={online ? "探测到 llama-server 的 HTTP API" : "未探测到服务（服务未起或地址/端口不对）"}
        />
        <StatusDot tone={procStatus.tone} label={procStatus.label} title={procStatus.title} />
        <span style={{ flex: 1 }} />
        {!isExternal && (
          <>
            <Button primary={canStart} disabled={!canStart} onClick={start} title={canStart ? "启动 llama-server" : "启动中或已在运行"}>
              <PlayIcon size={12} /> 启动
            </Button>
            <Button tone="danger" disabled={!canStop} onClick={stop} title={canStop ? "停止 llama-server" : "没有运行中的进程"}>
              <SquareIcon size={12} /> 停止
            </Button>
          </>
        )}
        <Button onClick={runScan} title="重新扫描模型目录" disabled={modelsLoading}>
          <RefreshIcon size={12} /> 扫描
        </Button>
      </Toolbar>

      <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
        {/* 上半区：左侧选模型、右侧调启动参数，两栏各自滚动；窄面板改为上下堆叠 */}
        <div ref={layoutRef} style={{ flex: 3, minHeight: 0, display: "flex", flexDirection: isStacked ? "column" : "row" }}>
          {/* 模型选择：堆叠时 2 份高度（基数为 0，两栏按 2:3 分上半区） */}
          <div
            style={{
              flex: isStacked ? "2 1 0" : "0 1 300px",
              minWidth: isStacked ? 0 : 220,
              minHeight: 0,
              display: "flex",
              flexDirection: "column",
              borderRight: isStacked ? undefined : `1px solid ${border}`,
              borderBottom: isStacked ? `1px solid ${border}` : undefined,
            }}
          >
            <SectionTitle
              right={
                <span style={{ display: "inline-flex", alignItems: "center", gap: 6, minWidth: 0 }}>
                  {folders.length > 0 ? <Chip>{`${folders.length} 个模型`}</Chip> : null}
                  {settings.mmprojFile ? <Chip title={settings.mmprojFile}>mmproj 已挂</Chip> : null}
                  {settings.modelsDir ? (
                    <Chip title={settings.modelsDir}>{fileNameFromPath(settings.modelsDir)}</Chip>
                  ) : null}
                </span>
              }
            >
              模型
            </SectionTitle>
            {modelsError && !modelsLoading && (
              <Notice tone="error" onClose={() => setModelsError("")}>
                {modelsError}
              </Notice>
            )}
            {settings.modelsDir && (
              <div style={{ display: "flex", gap: 6, padding: "4px 10px" }}>
                <div style={{ flex: 1, minWidth: 0, position: "relative" }}>
                  <div style={{ position: "absolute", left: 6, top: 6, color: textMuted, pointerEvents: "none" }}>
                    <SearchIcon size={12} />
                  </div>
                  <TextInput
                    value={modelFilter}
                    onChange={setModelFilter}
                    placeholder={`过滤 ${filtered.length}/${folders.length} 个模型文件夹`}
                    style={{ paddingLeft: 32 }}
                  />
                </div>
                {selectedFolder ? (
                  <Button onClick={clearSelection} title="清除当前模型选择">
                    清除
                  </Button>
                ) : null}
              </div>
            )}

            <div
              style={{ flex: 1, minHeight: 0, overflowY: "auto", scrollbarGutter: "stable", display: "flex", flexDirection: "column", gap: 4, padding: "4px 10px 8px" }}
              className={SCROLL_LIST_CLASS}
            >
              {modelsLoading ? (
                <div style={{ padding: 12, color: textMuted }}>扫描中…</div>
              ) : !settings.modelsDir ? (
                <Empty>
                  未设置模型目录。
                  <div style={{ fontSize: 11 }}>到本插件的设置页选择模型目录；扫描会递归找出里面的 .gguf 模型文件夹。</div>
                </Empty>
              ) : folders.length === 0 ? (
                modelsError ? null : (
                  <Empty>
                    未发现含 .gguf 的模型文件夹。
                    <div style={{ fontSize: 11 }}>一个模型一个文件夹（内含各量化与 mmproj）；确认目录存在且里面确有 gguf 模型。</div>
                  </Empty>
                )
              ) : filtered.length === 0 ? (
                <Empty>无匹配模型（过滤器 {modelFilter}）</Empty>
              ) : (
                filtered.map((f) => {
                  const selected = f === selectedFolder;
                  const dir = dirNameOf(f.rel);
                  const base = dir ? f.rel.slice(dir.length + 1) : f.rel;
                  // 量化体积合计（宿主缺 size 的条目按 0 计，全缺则不显示大小）
                  const totalBytes = f.quants.reduce((sum, q) => sum + (q.size ?? 0), 0);
                  const hasSize = f.quants.some((q) => q.size != null);
                  return (
                    <div
                      key={f.rel}
                      onClick={() => selectFolder(f)}
                      title={f.full}
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        padding: "6px 8px",
                        borderRadius: 6,
                        cursor: "pointer",
                        border: `1px solid ${selected ? accent : border}`,
                        background: selected ? hover : "transparent",
                        color: textPrimary,
                        fontSize: FONT_SM,
                        lineHeight: 1.6,
                      }}
                    >
                      <span style={{ flexShrink: 0, color: selected ? accent : textMuted }}>
                        <ImageIcon size={14} />
                      </span>
                      <span
                        style={{
                          flex: 1,
                          minWidth: 0,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                          fontWeight: selected ? 600 : 400,
                        }}
                      >
                        {dir ? <span style={{ color: textMuted }}>{`${dir}/`}</span> : null}
                        {base}
                      </span>
                      <span style={{ flexShrink: 0, color: textMuted, fontSize: 11 }}>
                        {`${f.quants.length} 个量化${f.mms.length > 0 ? ` · ${f.mms.length} 投影` : ""}${hasSize ? ` · ${formatSize(totalBytes)}` : ""}`}
                      </span>
                    </div>
                  );
                })
              )}
            </div>
          </div>

          {/* 启动参数设置：按模型文件夹持久化，未选中模型时给引导；堆叠时 3 份高度 */}
          <div style={{ flex: isStacked ? "3 1 0" : 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
            <SectionTitle
              right={
                <span style={{ display: "flex", alignItems: "center", gap: 8, flex: 1, minWidth: 0, justifyContent: "flex-end" }}>
                  {selectedFolder ? (
                    <span
                      title={selectedFolder.full}
                      style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", textTransform: "none", letterSpacing: 0 }}
                    >
                      {fileNameFromPath(selectedFolder.rel)}
                    </span>
                  ) : null}
                  <span style={{ flexShrink: 0 }}>按模型保存</span>
                </span>
              }
            >
              启动参数
            </SectionTitle>
            {selectedFolder && params ? (
              <div
                style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "8px 10px 4px", display: "flex", flexDirection: "column", gap: 4 }}
                className={SCROLL_LIST_CLASS}
              >
                <Field label="量化文件">
                  <Select
                    value={params.quant || (selectedFolder.quants[0] ? folderRelName(selectedFolder, selectedFolder.quants[0]) : "")}
                    onChange={(v) => updateParams({ quant: v })}
                    options={selectedFolder.quants.map((q) => ({
                      value: folderRelName(selectedFolder, q),
                      label: `${fileNameFromPath(q.name)}${q.size != null ? `（${formatSize(q.size)}）` : ""}`,
                    }))}
                  />
                </Field>
                <Field label="视觉投影（多模态模型）">
                  <Select
                    value={params.mmproj}
                    onChange={(v) => updateParams({ mmproj: v })}
                    options={[
                      { value: "", label: "不挂投影" },
                      ...selectedFolder.mms.map((m) => ({
                        value: folderRelName(selectedFolder, m),
                        label: fileNameFromPath(m.name),
                      })),
                    ]}
                  />
                </Field>
                <div style={{ display: "flex", gap: 8 }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <Field label="GPU 层数（-ngl）">
                      <TextInput
                        value={params.ngl === null ? "" : String(params.ngl)}
                        onChange={(v) => updateParams({ ngl: optionalInt(v, 0) })}
                        type="number"
                        min={0}
                        placeholder="不传"
                      />
                    </Field>
                  </div>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <Field label="上下文（--ctx-size）">
                      <TextInput
                        value={params.ctxSize === null ? "" : String(params.ctxSize)}
                        onChange={(v) => updateParams({ ctxSize: optionalInt(v, 1) })}
                        type="number"
                        min={1}
                        placeholder="默认"
                      />
                    </Field>
                  </div>
                </div>
                <div style={{ display: "flex", gap: 8 }}>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <Field label="线程数（--threads）">
                      <TextInput
                        value={params.threads === null ? "" : String(params.threads)}
                        onChange={(v) => updateParams({ threads: optionalInt(v, 1) })}
                        type="number"
                        min={1}
                        placeholder="自动"
                      />
                    </Field>
                  </div>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <Field label="并行槽位（--parallel）">
                      <TextInput
                        value={params.parallel === null ? "" : String(params.parallel)}
                        onChange={(v) => updateParams({ parallel: optionalInt(v, 1) })}
                        type="number"
                        min={1}
                        placeholder="1"
                      />
                    </Field>
                  </div>
                </div>
                <Field label="KV 缓存 K 量化" hint="量化 KV 缓存省显存。">
                  <Select
                    value={params.cacheTypeK}
                    onChange={(v) => updateParams({ cacheTypeK: v })}
                    options={[
                      { value: "", label: "默认（f16）" },
                      { value: "q8_0", label: "q8_0（显存减半，近无损）" },
                      { value: "q4_0", label: "q4_0（更省，质量略降）" },
                      { value: "q5_1", label: "q5_1" },
                      { value: "bf16", label: "bf16" },
                    ]}
                  />
                </Field>
                <Field label="载入模式">
                  <Select
                    value={params.loadMode}
                    onChange={(v) => updateParams({ loadMode: v })}
                    options={[
                      { value: "", label: "默认（auto）" },
                      { value: "auto", label: "auto（自动选择）" },
                      { value: "mmap", label: "mmap（内存映射）" },
                      { value: "mmap+mlock", label: "mmap+mlock（映射并锁定）" },
                      { value: "mlock", label: "mlock（锁定内存）" },
                      { value: "none", label: "none（直接读入）" },
                      { value: "dio", label: "dio（直接 I/O）" },
                    ]}
                  />
                </Field>
                <Field label="常用开关">
                  <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 16px" }}>
                    <Checkbox
                      checked={params.flashAttn}
                      onChange={(c) => updateParams({ flashAttn: c })}
                      label="Flash Attention"
                      title="--flash-attn on：注意力提速并省显存"
                    />
                    <Checkbox
                      checked={params.jinja}
                      onChange={(c) => updateParams({ jinja: c })}
                      label="Jinja 模板"
                      title="--jinja：使用模型自带的聊天模板"
                    />
                    <Checkbox
                      checked={params.noWebui}
                      onChange={(c) => updateParams({ noWebui: c })}
                      label="隐藏 WebUI"
                      title="--no-webui：不开放内置网页界面"
                    />
                  </div>
                </Field>
                <Field label="附加参数">
                  <TextInput
                    value={params.extraArgs}
                    onChange={(v) => updateParams({ extraArgs: v })}
                    placeholder="空白分隔，含空格的项用引号包裹"
                  />
                </Field>
              </div>
            ) : (
              <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "8px 10px", color: textMuted, lineHeight: 1.6 }} className={SCROLL_LIST_CLASS}>
                在左侧点选一个模型文件夹，即可在此设置它的量化文件、视觉投影、GPU 层数（-ngl）、上下文（--ctx-size）与附加参数。
              </div>
            )}
          </div>
        </div>

        {/* 日志区 */}
        <div style={{ flex: 2, minHeight: 0, display: "flex", flexDirection: "column", borderTop: `1px solid ${border}`, background: bgSecondary }}>
          <SectionTitle
            right={
              <span style={{ display: "flex", alignItems: "center", gap: 8, flex: 1, minWidth: 0 }}>
                {hostSnapshot.startLine ? (
                  <span
                    title={`启动命令：${hostSnapshot.startLine}`}
                    style={{
                      flex: 1,
                      minWidth: 0,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                      fontSize: 11,
                      color: textMuted,
                      textTransform: "none",
                      letterSpacing: 0,
                    }}
                  >
                    {hostSnapshot.startLine}
                  </span>
                ) : null}
                {hostSnapshot.logs.length > 0 ? (
                  <Button onClick={() => host.clearLogs()} title="清空日志显示">
                    清屏
                  </Button>
                ) : null}
              </span>
            }
          >
            日志
          </SectionTitle>
          {hostSnapshot.error && (
            <Notice tone="error">
              {hostSnapshot.error}
            </Notice>
          )}
          <div
            ref={logRef}
            style={{ flex: 1, minHeight: 0, overflowY: "auto", scrollbarGutter: "stable", padding: "8px 10px 12px", display: "flex", flexDirection: "column", gap: 2, background: TERM_BG }}
            className={SCROLL_LIST_CLASS}
          >
            {hostSnapshot.logs.length === 0 ? (
              <div style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: 6, color: TERM_DIM, fontSize: FONT_SM, lineHeight: 1.6 }}>
                <div>尚无日志</div>
                <div style={{ fontSize: 11 }}>启动或操作 llama-server 后，这里会滚出它的输出。</div>
              </div>
            ) : (
              hostSnapshot.logs.map((line, i) => {
                // 时间戳头哑色弱化，正文按级别着色；就绪行（listening on）给绿色作为直观信号
                const head = /^(!\s*\S+\s+)/.exec(line);
                const tone = logTone(line);
                const color =
                  tone === "error" ? TERM_ERROR : tone === "warn" ? TERM_WARN : line.includes("listening on") ? TERM_OK : TERM_TEXT;
                return (
                  <div key={i} style={{ fontFamily: TERM_FONT, fontSize: 11, whiteSpace: "pre-wrap", wordBreak: "break-word", color, lineHeight: 1.5 }}>
                    {head ? <span style={{ color: TERM_DIM }}>{head[1]}</span> : null}
                    <span>{head ? line.slice(head[1].length) : line}</span>
                  </div>
                );
              })
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
