/** 「模型库」页签：检索模型、看文件清单、选镜像站下载。取数走 host/hf，下载交给 Downloader。 */
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
  Empty,
  Notice,
  SCROLL_LIST_CLASS,
  SearchIcon,
  SectionTitle,
  Select,
  TextInput,
  Toolbar,
  bgPrimary,
  bgSecondary,
  border,
  textMuted,
  textPrimary,
  FONT_SM,
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
  /** 已取回的结果条数（「更多」按页累加）。 */
  const [limit, setLimit] = React.useState(PAGE_SIZE);
  const seq = React.useRef(0);
  const detailSeq = React.useRef(0);

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

  return (
    <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", minWidth: 0, overflow: "hidden", background: bgPrimary, color: textPrimary, fontSize: FONT_SM, boxSizing: "border-box" }}>
      <Toolbar>
        <div style={{ flex: 1, minWidth: 160, position: "relative" }}>
          <div style={{ position: "absolute", left: 6, top: 6, color: textMuted, pointerEvents: "none" }}>
            <SearchIcon size={12} />
          </div>
          <TextInput
            value={input}
            onChange={setInput}
            onEnter={() => {
              setLimit(PAGE_SIZE);
              setQuery(input);
            }}
            placeholder="搜索模型，如 qwen2.5-7b-instruct gguf"
            title="按关键词检索模型仓库，回车开始搜索"
            style={{ paddingLeft: 32 }}
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
          style={{ width: 110 }}
        />
        <Select
          value={mirrorId}
          onChange={(v) => onSettingsChanged({ ...settings, downloadMirror: v })}
          options={MIRRORS.map((m) => ({ value: m.id, label: m.label }))}
          title={mirror.note}
          style={{ width: 190 }}
        />
      </Toolbar>

      <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column" }}>
        {detail || detailLoading ? (
          <>
            <SectionTitle
              right={
                <Button onClick={() => { setDetail(null); setDetailError(""); }} title="返回检索结果">
                  返回列表
                </Button>
              }
            >
              {detail ? fileNameOfRepo(detail.summary.repo) : "模型详情"}
            </SectionTitle>
            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "4px 10px 12px" }} className={SCROLL_LIST_CLASS}>
              {detailLoading ? (
                <div style={{ padding: 12, color: textMuted }}>读取模型信息…</div>
              ) : detailError ? (
                <Notice tone="error">{detailError}</Notice>
              ) : detail ? (
                <>
                  <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
                    <Chip>{detail.summary.repo}</Chip>
                    <Chip>{`${detail.summary.downloads.toLocaleString()} 次下载`}</Chip>
                    <Chip>{`${detail.summary.likes} 赞`}</Chip>
                    {detail.summary.updatedAt ? <Chip>{`更新于 ${shortDate(detail.summary.updatedAt)}`}</Chip> : null}
                    {detail.gated ? <Chip tone="warn">需授权（门控模型）</Chip> : null}
                  </div>
                  {detail.gated && (
                    <Notice tone="warn">
                      这是门控模型：需要到模型页同意条款后才能下载，本站无法代下，请用浏览器手动下载后放进模型目录。
                    </Notice>
                  )}
                  <SectionTitle>{`文件（${detail.files.length} 个 gguf）`}</SectionTitle>
                  {detail.files.length === 0 ? (
                    <Empty>这个仓库里没有 .gguf 文件，可能只提供原始权重。</Empty>
                  ) : (
                    detail.files.map((file) => (
                      <FileRow
                        key={file.name}
                        file={file}
                        disabled={task.status === "downloading"}
                        onDownload={() => void beginDownload(`${detail.summary.repo}/${file.name}`)}
                      />
                    ))
                  )}
                  {detail.readme ? (
                    <>
                      <SectionTitle>模型简介</SectionTitle>
                      <div style={{ whiteSpace: "pre-wrap", lineHeight: 1.7, color: textMuted, fontSize: 11 }}>{detail.readme}</div>
                    </>
                  ) : null}
                </>
              ) : null}
            </div>
          </>
        ) : (
          <>
            <SectionTitle
              right={
                results.length > 0 ? (
                  <>
                    <Chip>{`${results.length} 条`}</Chip>
                    <Button onClick={() => setLimit(limit + PAGE_SIZE)} disabled={searching} title="再多取一页">
                      更多
                    </Button>
                  </>
                ) : null
              }
            >
              检索结果
            </SectionTitle>
            {listError && <Notice tone="error">{listError}</Notice>}
            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "4px 10px 12px", display: "flex", flexDirection: "column", gap: 4 }} className={SCROLL_LIST_CLASS}>
              {searching ? (
                <div style={{ padding: 12, color: textMuted }}>检索中…</div>
              ) : results.length === 0 ? (
                <Empty hint={`当前镜像：${mirror.label}。检索不到时先确认网络能访问该镜像。`}>没有结果，换个关键词试试</Empty>
              ) : (
                results.map((item) => (
                  <ResultRow key={item.repo} item={item} onOpen={() => void openDetail(item.repo)} />
                ))
              )}
            </div>
          </>
        )}
      </div>

      {renameAsk ? (
        <Notice tone="warn">
          <div style={{ marginBottom: 8 }}>{linkError}</div>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            <Button onClick={() => void downloadRenamed()} title="保留同名旧文件，新文件加序号落盘">
              改名保留
            </Button>
            <Button
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
            {linkError && <div style={{ color: "#e5534b" }}>{linkError}</div>}
            {resolved && !linkError ? <div style={{ color: textMuted }}>{resolved}</div> : null}
            {!modelsDir ? <div style={{ color: textMuted }}>未设置模型目录：先到设置页选择模型目录，下载才有落点。</div> : null}
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

function ResultRow(props: { item: ModelSummary; onOpen: () => void }): unknown {
  const [hover, setHover] = React.useState(false);
  const { item } = props;
  return (
    <div
      onClick={props.onOpen}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={item.repo}
      style={{
        padding: "6px 8px",
        borderRadius: 6,
        cursor: "pointer",
        border: `1px solid ${border}`,
        background: hover ? "var(--hover)" : undefined,
        lineHeight: 1.6,
      }}
    >
      <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{item.repo}</div>
      <div style={{ marginTop: 2, color: textMuted, fontSize: 11 }}>
        {`${item.downloads.toLocaleString()} 次下载 · ${item.likes} 赞${item.updatedAt ? ` · 更新于 ${shortDate(item.updatedAt)}` : ""}`}
      </div>
    </div>
  );
}

function FileRow(props: { file: RepoFile; disabled: boolean; onDownload: () => void }): unknown {
  const { file } = props;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "5px 8px", borderBottom: `1px solid ${border}`, lineHeight: 1.6 }}>
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={file.name}>
        {file.name}
      </span>
      <span style={{ flexShrink: 0, color: textMuted, fontSize: 11 }}>{formatSize(file.size)}</span>
      <Button onClick={props.onDownload} disabled={props.disabled} title={props.disabled ? "已有下载任务进行中" : `下载 ${file.name}`}>
        下载
      </Button>
    </div>
  );
}

