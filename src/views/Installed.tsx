/**
 * 「已装模型」页签：按文件夹列出模型并选中，顶部给出启停与连接/运行状态。
 * 一个模型 = 一个文件夹（内含各量化与 mmproj）；选中态写进设置的 modelFile，启动参数与启停都依它。
 */
import React from "react";
import type { AtelyxCtx } from "../ctx";
import type { LlamaRuntime } from "../runtime";
import type { LlamaSettings, ModelLaunchParams } from "../settings";
import type { HostController, HostSnapshot } from "../host/controller";
import { dirNameOf, groupByFolder, listModelsCached, type ModelEntry, type ModelFolder } from "../host/models";
import { fileNameOf, formatSize } from "../host/types";
import { ParamsForm, selectedFolderOf, selectFolderSettings, updateParamsSettings } from "./launchParams";
import {
  Button,
  Chip,
  Empty,
  ImageIcon,
  MODEL_ROW_CLASS,
  Notice,
  PANEL_CSS,
  PlayIcon,
  RefreshIcon,
  SCROLL_LIST_CLASS,
  SearchIcon,
  SectionTitle,
  SquareIcon,
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

interface InstalledProps {
  ctx: AtelyxCtx;
  runtime: LlamaRuntime;
  host: HostController;
  settings: LlamaSettings;
  onSettingsChanged: (next: LlamaSettings) => void;
}

/** 模型栏宽度：并排时参数栏至少也要这么宽，故两栏并排需要面板宽度达到它的两倍。 */
const MODELS_PANE_WIDTH = 340;
const TWO_COLUMN_MIN_WIDTH = MODELS_PANE_WIDTH * 2;

/** 按面板自身宽度决定上下两栏是并排还是堆叠（窄面板下并排会把输入压成窄条）。 */
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

export function InstalledView(props: InstalledProps): unknown {
  const { ctx, runtime, host, settings, onSettingsChanged } = props;
  const { ref: layoutRef, isStacked } = useStackedLayout();
  const runtimeSnapshot = React.useSyncExternalStore(runtime.subscribe, runtime.getSnapshot);
  const hostSnapshot: HostSnapshot = React.useSyncExternalStore(host.subscribe, host.getSnapshot);

  const online = runtimeSnapshot.channel === "direct";
  const isExternal = settings.processMode === "external";
  const canStart = !isExternal && !hostSnapshot.running && !hostSnapshot.starting;
  const canStop = !isExternal && (hostSnapshot.running || hostSnapshot.starting);
  const status = ((): { tone: "ok" | "warn" | "bad" | "idle"; label: string; title: string } => {
    if (hostSnapshot.starting)
      return { tone: "warn", label: "启动中", title: "进程已拉起，正在等待 llama-server 接口就绪" };
    if (hostSnapshot.running) return { tone: "ok", label: "运行中", title: "本插件启动的 llama-server 正在运行" };
    if (hostSnapshot.error) return { tone: "bad", label: "启动失败", title: hostSnapshot.error };
    if (isExternal) return { tone: "idle", label: "未接管", title: "外部模式：进程归你管，只看连接状态" };
    return { tone: "idle", label: "已停止", title: "未运行" };
  })();

  const [models, setModels] = React.useState<ModelEntry[]>([]);
  const [projectors, setProjectors] = React.useState<ModelEntry[]>([]);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState("");
  const [filter, setFilter] = React.useState("");
  const scanSeq = React.useRef(0);

  /** 扫描：`force` 用于用户点「扫描」时绕过缓存（下载完成后缓存已失效，重扫即可见新模型）。 */
  const runScan = React.useCallback(
    async (force = false): Promise<void> => {
      const mySeq = ++scanSeq.current;
      setLoading(true);
      setError("");
      const result = await listModelsCached(ctx, settings.modelsDir, force);
      if (mySeq !== scanSeq.current) return; // 被更新的扫描取代，丢弃结果
      setLoading(false);
      if (result.ok) {
        setModels(result.models);
        setProjectors(result.projectors);
        if (result.capped) setError("模型较多，只返回了一部分；用过滤器收窄。");
      } else {
        setError(result.error);
      }
    },
    [ctx, settings.modelsDir],
  );

  React.useEffect(() => {
    void runScan();
  }, [runScan]);

  // 按文件夹聚合：文件夹 = 含至少一个非 mmproj 的 gguf；投影挂到同目录文件夹
  const folders = React.useMemo<ModelFolder[]>(
    () => groupByFolder({ models, projectors }, settings.modelsDir),
    [models, projectors, settings.modelsDir],
  );

  const filtered = React.useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return folders;
    return folders.filter((f) => f.rel.toLowerCase().includes(needle));
  }, [folders, filter]);

  const selected = selectedFolderOf(folders, settings);

  const selectFolder = (folder: ModelFolder): void => {
    onSettingsChanged(selectFolderSettings(settings, folder));
  };

  const clearSelection = (): void => onSettingsChanged({ ...settings, modelFile: "", mmprojFile: "" });

  const updateParams = (changes: Partial<ModelLaunchParams>): void => {
    if (selected) onSettingsChanged(updateParamsSettings(settings, selected, changes));
  };

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden", background: bgPrimary, color: textPrimary, fontSize: FONT_SM, boxSizing: "border-box" }}>
      <style>{PANEL_CSS}</style>
      <Toolbar>
        <StatusDot
          tone={online ? "ok" : "idle"}
          label={online ? `已连接 · ${settings.host}:${settings.port}` : `未连接 · ${settings.host}:${settings.port}`}
          title={online ? "探测到 llama-server 的 HTTP API" : "未探测到服务（服务未起或地址/端口不对）"}
        />
        <StatusDot tone={status.tone} label={status.label} title={status.title} />
        <span style={{ flex: 1 }} />
        {!isExternal && (
          <>
            <Button primary={canStart} disabled={!canStart} onClick={() => void host.start(runtime)} title={canStart ? "启动 llama-server" : "启动中或已在运行"}>
              <PlayIcon size={12} /> 启动
            </Button>
            <Button tone="danger" disabled={!canStop} onClick={() => void host.stop()} title={canStop ? "停止 llama-server" : "没有运行中的进程"}>
              <SquareIcon size={12} /> 停止
            </Button>
          </>
        )}
      </Toolbar>
      <div ref={layoutRef} style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: isStacked ? "column" : "row" }}>
        <div
          style={{
            flex: isStacked ? "2 1 0" : `0 1 ${MODELS_PANE_WIDTH}px`,
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
                {settings.modelsDir ? <Chip title={settings.modelsDir}>{fileNameOf(settings.modelsDir)}</Chip> : null}
              </span>
            }
          >
            模型
          </SectionTitle>
          {error && !loading && (
            <Notice tone="error" onClose={() => setError("")}>
              {error}
            </Notice>
          )}
          {settings.modelsDir && (
            <div style={{ display: "flex", gap: 6, padding: "4px 10px" }}>
              <div style={{ flex: 1, minWidth: 0, position: "relative" }}>
                <div style={{ position: "absolute", left: 6, top: 6, color: textMuted, pointerEvents: "none" }}>
                  <SearchIcon size={12} />
                </div>
                <TextInput
                  value={filter}
                  onChange={setFilter}
                  placeholder={`过滤 ${filtered.length}/${folders.length} 个模型`}
                  title="按文件夹名过滤模型"
                  style={{ paddingLeft: 32 }}
                />
              </div>
              {selected ? (
                <Button onClick={clearSelection} title="清除当前模型选择">
                  清除
                </Button>
              ) : null}
              <Button onClick={() => void runScan(true)} disabled={loading} title="重新扫描模型目录（忽略缓存）">
                <RefreshIcon size={12} /> 扫描
              </Button>
            </div>
          )}

          <div
            style={{ flex: 1, minHeight: 0, overflowY: "auto", scrollbarGutter: "stable", display: "flex", flexDirection: "column", gap: 4, padding: "4px 10px 8px" }}
            className={SCROLL_LIST_CLASS}
          >
            {loading ? (
              <div style={{ padding: 12, color: textMuted }}>扫描中…</div>
            ) : !settings.modelsDir ? (
              <Empty>
                未设置模型目录。
                <div style={{ fontSize: 11 }}>到本插件的设置页选择模型目录；扫描会递归找出里面的 .gguf 模型文件夹。</div>
              </Empty>
            ) : folders.length === 0 ? (
              error ? null : (
                <Empty>
                  未发现含 .gguf 的模型文件夹。
                  <div style={{ fontSize: 11 }}>可在「模型库」页签搜索并下载模型，或确认目录里确有 gguf 文件。</div>
                </Empty>
              )
            ) : filtered.length === 0 ? (
              <Empty>无匹配模型（过滤器 {filter}）</Empty>
            ) : (
              filtered.map((folder) => {
                const isSelected = folder === selected;
                const dir = dirNameOf(folder.rel);
                const base = dir ? folder.rel.slice(dir.length + 1) : folder.rel;
                const totalBytes = folder.quants.reduce((sum, q) => sum + (q.size ?? 0), 0);
                const hasSize = folder.quants.some((q) => q.size != null);
                return (
                  <div
                    key={folder.rel}
                    onClick={() => selectFolder(folder)}
                    title={folder.full}
                    className={MODEL_ROW_CLASS}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      padding: "6px 8px",
                      borderRadius: 6,
                      cursor: "pointer",
                      border: `1px solid ${isSelected ? accent : border}`,
                      background: isSelected ? hover : undefined,
                      transition: "background-color 120ms ease",
                      color: textPrimary,
                      fontSize: FONT_SM,
                      lineHeight: 1.6,
                    }}
                  >
                    <span style={{ flexShrink: 0, color: isSelected ? accent : textMuted }}>
                      <ImageIcon size={14} />
                    </span>
                    <span style={{ flex: 1, minWidth: 0 }}>
                      <span style={{ display: "block", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontWeight: isSelected ? 600 : 400 }}>
                        {dir ? <span style={{ color: textMuted }}>{`${dir}/`}</span> : null}
                        {base}
                      </span>
                      <span style={{ display: "block", marginTop: 2, color: textMuted, fontSize: 11, lineHeight: 1.4, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {`${folder.quants.length} 个量化${folder.mms.length > 0 ? ` · ${folder.mms.length} 投影` : ""}${hasSize ? ` · ${formatSize(totalBytes)}` : ""}`}
                      </span>
                    </span>
                  </div>
                );
              })
            )}
          </div>
        </div>

        <div style={{ flex: isStacked ? "3 1 0" : 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column", background: bgSecondary }}>
          <SectionTitle
            right={
              <span style={{ display: "flex", alignItems: "center", gap: 8, flex: 1, minWidth: 0, justifyContent: "flex-end" }}>
                {selected ? (
                  <span title={selected.full} style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", textTransform: "none", letterSpacing: 0 }}>
                    {fileNameOf(selected.rel)}
                  </span>
                ) : null}
                <span style={{ flexShrink: 0 }}>按模型保存</span>
              </span>
            }
          >
            启动参数
          </SectionTitle>
          {selected ? (
            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "8px 10px 4px", display: "flex", flexDirection: "column", gap: 4 }} className={SCROLL_LIST_CLASS}>
              <ParamsForm settings={settings} folder={selected} onChange={updateParams} />
            </div>
          ) : (
            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "8px 10px", color: textMuted, lineHeight: 1.6 }} className={SCROLL_LIST_CLASS}>
              在左侧点选一个模型文件夹，即可在此设置它的量化文件、视觉投影、GPU 层数（-ngl）、上下文（--ctx-size）与附加参数。
              <div style={{ marginTop: 8 }}>参数按模型文件夹保存：换模型即带出各自的设置；设好后点顶部「启动」拉起服务。</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

