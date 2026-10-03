/** 「模型库」页签：检索模型、看文件清单与模型卡（markdown 渲染）、选镜像站下载。取数走 host/hf，下载交给 Downloader。 */
import React from "react";
import type { AtelyxCtx } from "../ctx";
import type { Downloader } from "../host/downloader";
import { formatSize, shortDate, type DownloadTask, type ModelSummary, type RepoDetail, type RepoFile } from "../host/types";
import { fetchRepoDetail, searchModels, type SearchSort } from "../host/hf";
import { DEFAULT_MIRROR_ID, MIRRORS, mirrorById } from "../host/mirrors";
import type { LlamaSettings } from "../settings";
import { DownloadCard } from "./DownloadCard";
import {
  Button,
  Chip,
  DownloadIcon,
  Empty,
  Notice,
  SearchIcon,
  SectionTitle,
  Select,
  Spinner,
  TextInput,
  Toolbar,
  bgSecondary,
  border,
  fontCaption,
  fontH2,
  fontUi,
  textMuted,
  textPrimary,
  useSplitLayout,
} from "./ui";

interface LibraryProps {
  ctx: AtelyxCtx;
  downloader: Downloader;
  settings: LlamaSettings;
  /** 镜像站选择随设置持久化。 */
  onSettingsChanged: (next: LlamaSettings) => void;
}

const PAGE_SIZE = 30;
const SORT_OPTIONS: Array<{ value: SearchSort; label: string }> = [
  { value: "downloads", label: "下载量" },
  { value: "likes", label: "点赞数" },
  { value: "lastModified", label: "最近更新" },
];

export function LibraryView(props: LibraryProps): unknown {
  const { ctx, downloader, settings, onSettingsChanged } = props;
  const task = React.useSyncExternalStore(downloader.subscribe, downloader.getSnapshot);
  const { ref: splitRef, stacked } = useSplitLayout(640);

  const [input, setInput] = React.useState("");
  const [sort, setSort] = React.useState<SearchSort>("downloads");
  /** 已提交检索的关键词：输入框即输即改，检索只认提交过的值。 */
  const [query, setQuery] = React.useState("");
  const [results, setResults] = React.useState<ModelSummary[]>([]);
  const [searching, setSearching] = React.useState(false);
  const [listError, setListError] = React.useState("");
  const [detail, setDetail] = React.useState<RepoDetail | null>(null);
  const [detailLoading, setDetailLoading] = React.useState(false);
  const [detailError, setDetailError] = React.useState("");
  const [linkInput, setLinkInput] = React.useState("");
  const [linkError, setLinkError] = React.useState("");
  /** 简写地址解析出的仓库（下载开始时显示，供核对下载目标）。 */
  const [resolved, setResolved] = React.useState("");
  /** 同名文件已存在时的改名征询：值 = 触发重复的那次输入（整条地址或 owner/repo/文件名）。 */
  const [renameAsk, setRenameAsk] = React.useState<string | null>(null);
  /** 已取回的结果条数（触底自动按页累加）。 */
  const [limit, setLimit] = React.useState(PAGE_SIZE);
  const seq = React.useRef(0);
  const detailSeq = React.useRef(0);
  const listRef = React.useRef<HTMLDivElement | null>(null);

  const mirrorId = settings.downloadMirror || DEFAULT_MIRROR_ID;
  const mirror = mirrorById(mirrorId);
  const modelsDir = settings.modelsDir.trim();

  const runSearch = React.useCallback(
    async (query: string, sortBy: SearchSort, size: number): Promise<void> => {
      const mySeq = ++seq.current;
      setSearching(true);
      setListError("");
      try {
        const found = await searchModels(ctx, mirrorById(settings.downloadMirror || DEFAULT_MIRROR_ID), query, sortBy, size);
        if (mySeq !== seq.current) return;
        setResults(found);
      } catch (err) {
        if (mySeq !== seq.current) return;
        setResults([]);
        setListError(err instanceof Error ? err.message : String(err));
      } finally {
        if (mySeq === seq.current) setSearching(false);
      }
    },
    [ctx, settings.downloadMirror],
  );

  // 检索由「提交的关键词 + 排序 + 条数」驱动；换镜像站会重查，顺序号防慢响应覆盖新结果
  React.useEffect(() => {
    void runSearch(query, sort, limit);
  }, [runSearch, query, sort, limit]);

  /**
   * 瀑布流加载：接近底部时追加下一页；已取条数少于一页说明取完了，不再请求。
   * 首页结果不满一屏时滚动条不出现在，故渲染后再查一次是否需要续页。
   */
  const loadMore = React.useCallback((): void => {
    if (searching || results.length < limit) return;
    setLimit(limit + PAGE_SIZE);
  }, [searching, results.length, limit]);

  const onListScroll = React.useCallback(
    (e: { currentTarget: { scrollTop: number; clientHeight: number; scrollHeight: number } }): void => {
      const el = e.currentTarget;
      if (el.scrollHeight - el.scrollTop - el.clientHeight > 120) return;
      loadMore();
    },
    [loadMore],
  );

  React.useEffect(() => {
    const el = listRef.current;
    if (!el || searching || results.length < limit) return;
    if (el.scrollHeight <= el.clientHeight + 4) loadMore();
  }, [results, searching, limit, loadMore]);

  const openDetail = React.useCallback(
    async (repo: string): Promise<void> => {
      // 连点两个仓库时旧响应可能后到：序号不符即丢弃，避免详情与最后点击的不一致
      const mySeq = ++detailSeq.current;
      setDetailLoading(true);
      setDetailError("");
      setDetail(null);
      try {
        const loaded = await fetchRepoDetail(ctx, mirror, repo);
        if (mySeq !== detailSeq.current) return;
        setDetail(loaded);
      } catch (err) {
        if (mySeq !== detailSeq.current) return;
        setDetailError(err instanceof Error ? err.message : String(err));
      } finally {
        if (mySeq === detailSeq.current) setDetailLoading(false);
      }
    },
    [ctx, mirror],
  );

  /**
   * 启动一次下载：预检确认落盘名与远端体积，再交给下载器。
   *
   * 预检失败只有「同名文件已存在」才记下输入供「改名保留」重试——地址写错、断网这类
   * 失败改名也救不了，摆出按钮只会误导。
   */
  const beginDownload = React.useCallback(
    async (input: string, mode: "skip" | "rename" = "skip"): Promise<DownloadTask | null> => {
      setLinkError("");
      setRenameAsk(null);
      const prepared = await downloader.prepare({ input, dir: modelsDir, mirrorId }, mode);
      if (!prepared.ok) {
        setLinkError(prepared.error);
        if (prepared.existingPath) setRenameAsk(input);
        return null;
      }
      await downloader.start(prepared.task);
      return prepared.task;
    },
    [downloader, mirrorId, modelsDir],
  );

  /** 直接粘贴整条地址下载（不用先检索）；简写地址看不出仓库是谁，下载前把解析结果摆出来核对。 */
  const startFromLink = React.useCallback(async (): Promise<void> => {
    setResolved("");
    const task = await beginDownload(linkInput);
    if (task) setResolved(`将下载 ${task.repo} / ${task.file}`);
  }, [beginDownload, linkInput]);

  /** 带序号落盘重试（用户确认保留同名旧文件时）。 */
  const downloadRenamed = React.useCallback(async (): Promise<void> => {
    if (renameAsk) await beginDownload(renameAsk, "rename");
  }, [beginDownload, renameAsk]);

  const listPane = (
    <div
      style={{
        flex: stacked ? "1 1 0" : `0 1 340px`,
        minWidth: stacked ? 0 : 260,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
        borderRight: stacked ? "1px solid var(--border)" : `1px solid ${border}`,
        borderBottom: stacked ? `1px solid ${border}` : undefined,
        overflow: "hidden",
      }}
    >
      <SectionTitle
        right={results.length > 0 ? <Chip mono>{`${results.length} 条`}</Chip> : null}
      >
        检索结果
      </SectionTitle>
      {listError && <Notice tone="error">{listError}</Notice>}
      <div
        ref={listRef}
        onScroll={onListScroll}
        style={{
          flex: 1,
          minHeight: 0,
          overflowY: "auto",
          scrollbarGutter: "stable",
          padding: "4px 10px 12px",
          display: "flex",
          flexDirection: "column",
          gap: 4,
        }}
      >
        {searching && results.length === 0 ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8, padding: 12, color: textMuted, fontSize: fontUi }}>
            <Spinner /> 检索中…
          </div>
        ) : results.length === 0 ? (
          <Empty icon={<SearchIcon size={18} />} hint={`当前镜像：${mirror.label}。检索不到时先确认网络能访问该镜像。`}>
            没有结果，换个关键词试试
          </Empty>
        ) : (
          <>
            {results.map((item) => (
              <ResultRow
                key={item.repo}
                item={item}
                selected={detail?.summary.repo === item.repo}
                onOpen={() => void openDetail(item.repo)}
              />
            ))}
            {searching ? (
              <div style={{ display: "flex", justifyContent: "center", padding: 8 }}>
                <Spinner />
              </div>
            ) : null}
          </>
        )}
      </div>
    </div>
  );

  const detailPane = (
    <div style={{ flex: "3 1 0", minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column", overflow: "hidden" }}>
      {detailLoading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, padding: 16, color: textMuted, fontSize: fontUi }}>
          <Spinner /> 读取模型信息…
        </div>
      ) : detailError ? (
        <Notice tone="error">{detailError}</Notice>
      ) : detail ? (
        <DetailBody ctx={ctx} detail={detail} disabled={task.status === "downloading"} onDownload={(name) => void beginDownload(`${detail.summary.repo}/${name}`)} />
      ) : (
        <Empty icon={<SearchIcon size={18} />} hint="检索后在左侧点选仓库，这里展示文件清单与模型简介。">
          选择一个模型仓库
        </Empty>
      )}
    </div>
  );

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden", background: "var(--bg-primary)", color: textPrimary, fontSize: fontUi, boxSizing: "border-box" }}>
      <Toolbar>
        <div style={{ flex: "1 1 220px", maxWidth: 420, minWidth: 160, position: "relative", display: "flex", alignItems: "center" }}>
          <div style={{ position: "absolute", left: 8, top: "50%", transform: "translateY(-50%)", color: textMuted, pointerEvents: "none" }}>
            <SearchIcon size={13} />
          </div>
          <TextInput
            value={input}
            onChange={setInput}
            onEnter={() => {
              setLimit(PAGE_SIZE);
              setQuery(input);
            }}
            placeholder="搜索模型"
            title="按关键词检索模型仓库，回车开始搜索"
            style={{ paddingLeft: 30 }}
          />
        </div>
        <Button
          primary
          disabled={searching}
          onClick={() => {
            setLimit(PAGE_SIZE);
            setQuery(input);
          }}
          title="检索模型仓库"
        >
          搜索
        </Button>
        <Select
          value={sort}
          onChange={(v) => setSort(v as SearchSort)}
          options={SORT_OPTIONS}
          title="结果排序"
          style={{ width: 96, flexShrink: 0 }}
        />
        <Select
          value={mirrorId}
          onChange={(v) => onSettingsChanged({ ...settings, downloadMirror: v })}
          options={MIRRORS.map((m) => ({ value: m.id, label: m.label }))}
          title={mirror.note}
          style={{ flexShrink: 0 }}
        />
      </Toolbar>

      <div ref={splitRef} style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: stacked ? "column" : "row" }}>
        {listPane}
        {detailPane}
      </div>

      {renameAsk ? (
        <Notice tone="warn">
          <div style={{ marginBottom: 8 }}>{linkError}</div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            <Button size="sm" onClick={() => void downloadRenamed()} title="保留同名旧文件，新文件加序号落盘">
              改名保留
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setRenameAsk(null);
                setLinkError("");
              }}
            >
              取消
            </Button>
          </div>
        </Notice>
      ) : null}

      <div style={{ borderTop: `1px solid ${border}`, padding: "8px 10px", background: bgSecondary, display: "flex", flexDirection: "column", gap: 6 }}>
        {task.status === "idle" ? (
          <>
            <div style={{ display: "flex", gap: 6 }}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <TextInput
                  value={linkInput}
                  onChange={setLinkInput}
                  placeholder="或直接粘贴下载地址：https://hf-mirror.com/owner/repo/resolve/main/model.gguf"
                  title="支持整条下载地址或 owner/repo/文件名"
                />
              </div>
              <Button onClick={() => void startFromLink()} disabled={!linkInput.trim() || !modelsDir} title="按当前镜像站解析并下载">
                下载
              </Button>
            </div>
            {linkError && <div style={{ color: "var(--danger)", fontSize: fontCaption }}>{linkError}</div>}
            {resolved && !linkError ? <div style={{ color: textMuted, fontSize: fontCaption }}>{resolved}</div> : null}
            {!modelsDir ? <div style={{ color: textMuted, fontSize: fontCaption }}>未设置模型目录：先到设置页选择模型目录，下载才有落点。</div> : null}
          </>
        ) : (
          <DownloadCard
            task={task}
            onCancel={() => void downloader.cancel()}
            onResume={() => void downloader.start(task)}
            onDismiss={() => void downloader.dismiss()}
          />
        )}
      </div>
    </div>
  );
}

function fileNameOfRepo(repo: string): string {
  return repo.split("/").pop() ?? repo;
}

/** 检索结果条目：仓库名 + 统计徽标。选中态用强调色边框标记（与「已装模型」一致）。 */
function ResultRow(props: { item: ModelSummary; selected: boolean; onOpen: () => void }): unknown {
  const { item } = props;
  return (
    <div
      onClick={props.onOpen}
      title={item.repo}
      className="lp-model-row"
      style={{
        padding: "6px 8px",
        borderRadius: "var(--radius-sm)",
        border: `1px solid ${props.selected ? "var(--accent)" : "var(--border)"}`,
        lineHeight: 1.6,
        cursor: "pointer",
      }}
    >
      <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: fontUi, color: textPrimary, fontWeight: props.selected ? 600 : 400 }}>
        {item.repo}
      </div>
      <div style={{ display: "flex", gap: 4, marginTop: 3, flexWrap: "wrap" }}>
        <Chip mono title="下载量">
          {`↓ ${item.downloads.toLocaleString()}`}
        </Chip>
        <Chip mono title="点赞数">
          {`♥ ${item.likes}`}
        </Chip>
        {item.updatedAt ? <Chip title="最近更新">{`更新于 ${shortDate(item.updatedAt)}`}</Chip> : null}
      </div>
    </div>
  );
}

/** 详情正文：头部（名称/统计）与文件、简介两个区块各自成区，边界清晰。 */
function DetailBody(props: {
  ctx: AtelyxCtx;
  detail: RepoDetail;
  disabled: boolean;
  onDownload: (name: string) => void;
}): unknown {
  const { ctx, detail } = props;
  // 已清洗 HTML（raw HTML 经宿主白名单过滤）；.markdown-body 的宿主全局样式让渲染与应用内一致
  const readmeHtml = React.useMemo(() => (detail.readme ? ctx.markdown.renderHtml(detail.readme) : ""), [ctx, detail.readme]);

  return (
    <div style={{ flex: 1, minHeight: 0, overflowY: "auto", scrollbarGutter: "stable", padding: "10px 12px 16px", cursor: "default" }}>
      <div style={{ paddingBottom: 10, borderBottom: `1px solid ${border}` }}>
        <div style={{ fontSize: fontH2, fontWeight: 600, color: textPrimary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={detail.summary.repo}>
          {fileNameOfRepo(detail.summary.repo)}
        </div>
        <div style={{ color: textMuted, fontSize: fontCaption, marginTop: 2 }}>{detail.summary.repo}</div>
        <div style={{ display: "flex", gap: 4, flexWrap: "wrap", marginTop: 8 }}>
          <Chip mono title="下载量">
            {`↓ ${detail.summary.downloads.toLocaleString()}`}
          </Chip>
          <Chip mono title="点赞数">
            {`♥ ${detail.summary.likes}`}
          </Chip>
          {detail.summary.updatedAt ? <Chip title="最近更新">{`更新于 ${shortDate(detail.summary.updatedAt)}`}</Chip> : null}
          {detail.gated ? <Chip tone="warn">需授权（门控模型）</Chip> : null}
        </div>
      </div>
      {detail.gated && (
        <Notice tone="warn">
          这是门控模型：需要到模型页同意条款后才能下载，本站无法代下，请用浏览器手动下载后放进模型目录。
        </Notice>
      )}
      <DetailSection title={`文件（${detail.files.length} 个 gguf）`}>
        {detail.files.length === 0 ? (
          <div style={{ border: `1px solid ${border}`, borderRadius: "var(--radius-sm)", padding: "10px 12px", color: textMuted, fontSize: fontCaption, background: "var(--bg-secondary)" }}>
            这个仓库里没有 .gguf 文件，可能只提供原始权重。
          </div>
        ) : (
          <div style={{ border: `1px solid ${border}`, borderRadius: "var(--radius-sm)", overflow: "hidden", background: "var(--bg-secondary)" }}>
            {detail.files.map((file) => (
              <FileRow
                key={file.name}
                file={file}
                disabled={props.disabled}
                onDownload={() => props.onDownload(file.name)}
              />
            ))}
          </div>
        )}
      </DetailSection>
      {detail.readme ? (
        <DetailSection title="模型简介">
          <div
            className="markdown-body"
            style={{ border: `1px solid ${border}`, borderRadius: "var(--radius-sm)", padding: "10px 12px", color: textPrimary, background: "var(--bg-secondary)" }}
            dangerouslySetInnerHTML={{ __html: readmeHtml }}
          />
        </DetailSection>
      ) : null}
    </div>
  );
}

/** 详情区块：加大的区块标题 + 独立内容容器，与头部区拉开层次。 */
function DetailSection(props: { title: string; children: unknown }): unknown {
  return (
    <div style={{ marginTop: 12 }}>
      <div style={{ fontSize: fontUi, fontWeight: 600, color: textPrimary, marginBottom: 6 }}>{props.title}</div>
      {props.children}
    </div>
  );
}

function FileRow(props: { file: RepoFile; disabled: boolean; onDownload: () => void }): unknown {
  const { file } = props;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 10px", borderBottom: `1px solid ${border}`, lineHeight: 1.6 }}>
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: fontUi }} title={file.name}>
        {file.name}
      </span>
      <Chip mono title="文件大小">
        {formatSize(file.size)}
      </Chip>
      <Button size="sm" onClick={props.onDownload} disabled={props.disabled} title={props.disabled ? "已有下载任务进行中" : `下载 ${file.name}`}>
        <DownloadIcon size={12} /> 下载
      </Button>
    </div>
  );
}
