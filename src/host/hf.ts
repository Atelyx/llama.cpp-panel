/**
 * HuggingFace 数据面：搜索、仓库文件树、模型卡。
 * 走 `/api/models` 系接口（ModelScope 结构不同，不支持）；体积一次从 tree 接口取全——搜索接口
 * 不带体积，逐个探测会产生几十次请求。网络失败一律翻成可读原因抛出，不兜底静默。
 */
import type { AtelyxCtx } from "../ctx";
import { fetchJson, runCurl } from "./curl";
import { repoApiUrl, readmeUrl, searchApiUrl, type MirrorSource } from "./mirrors";
import { num, type ModelSummary, type RepoDetail, type RepoFile } from "./types";

/** 检索结果的排序方式。 */
export type SearchSort = "downloads" | "likes" | "lastModified";

/** 模型卡简介的展示上限（够看出用途即可，过长会把详情页挤满）。 */
const README_MAX_CHARS = 4000;

interface RawModel {
  id?: unknown;
  modelId?: unknown;
  downloads?: unknown;
  likes?: unknown;
  lastModified?: unknown;
  gated?: unknown;
}

function toSummary(raw: RawModel): ModelSummary | null {
  const id = typeof raw.modelId === "string" ? raw.modelId : typeof raw.id === "string" ? raw.id : "";
  if (!id) return null; // 无 id 的条目无法拼地址，直接丢弃
  return {
    repo: id,
    downloads: num(raw.downloads),
    likes: num(raw.likes),
    updatedAt: typeof raw.lastModified === "string" ? raw.lastModified : "",
  };
}

/** 搜索模型仓库。 */
export async function searchModels(
  ctx: AtelyxCtx,
  mirror: MirrorSource,
  query: string,
  sort: SearchSort,
  limit: number,
): Promise<ModelSummary[]> {
  const raw = await fetchJson<unknown>(ctx, searchApiUrl(mirror, query, sort, limit));
  if (!Array.isArray(raw)) throw new Error("检索接口返回的不是列表");
  return raw
    .map((item) => toSummary((item ?? {}) as RawModel))
    .filter((item): item is ModelSummary => item !== null);
}

interface RawTreeEntry {
  path?: unknown;
  size?: unknown;
  lfs?: { size?: unknown } | null;
}

/** 仓库文件条目 → 字节大小：普通文件取 size，LFS 指针文件取 lfs.size。 */
function entrySize(entry: RawTreeEntry): number | undefined {
  const direct = typeof entry.size === "number" ? entry.size : undefined;
  const lfs = entry.lfs && typeof entry.lfs.size === "number" ? entry.lfs.size : undefined;
  return lfs ?? direct;
}

/** 仓库详情：文件树（含体积）+ 模型卡简介。 */
export async function fetchRepoDetail(
  ctx: AtelyxCtx,
  mirror: MirrorSource,
  repo: string,
): Promise<RepoDetail> {
  const treeUrl = repoApiUrl(mirror, repo, "/tree/main?recursive=true&expand=true");
  const [treeRaw, detailRaw] = await Promise.all([
    fetchJson<unknown>(ctx, treeUrl),
    fetchJson<unknown>(ctx, repoApiUrl(mirror, repo, "")),
  ]);
  const summary = toSummary((detailRaw ?? {}) as RawModel);
  if (!summary) throw new Error(`仓库不存在或不可访问：${repo}`);

  const entries = Array.isArray(treeRaw) ? (treeRaw as RawTreeEntry[]) : [];
  const files: RepoFile[] = entries
    .filter((entry): entry is RawTreeEntry & { path: string } => typeof entry.path === "string")
    .filter((entry) => /\.gguf$/i.test(entry.path))
    .map((entry) => ({ name: entry.path, size: entrySize(entry) }))
    // 小的量化在前：挑模型时先看到能塞进显存的
    .sort((a, b) => (a.size ?? Number.MAX_SAFE_INTEGER) - (b.size ?? Number.MAX_SAFE_INTEGER));

  return {
    summary,
    files,
    readme: await fetchReadme(ctx, mirror, repo),
    gated: (detailRaw as RawModel).gated === true,
  };
}

/** 取模型卡简介。取不到不算失败——简介是附加信息，不该挡住文件清单。 */
async function fetchReadme(ctx: AtelyxCtx, mirror: MirrorSource, repo: string): Promise<string> {
  try {
    // `/raw/` 回的是 Markdown 正文而不是 JSON，按纯文本取；缺失时服务端给 404 + --fail 非零码
    const result = await runCurl(ctx, { url: readmeUrl(mirror, repo) });
    if (result.code !== 0) return "";
    return stripMarkdown(result.stdout);
  } catch {
    return "";
  }
}

/**
 * 探取下载地址对应的真实文件大小；拿不到返回 null——体积只用于显示百分比，不是下载的前置条件。
 * 用 HEAD 穿透重定向读最终响应的 Content-Length：HF 第一跳是签名重定向，跟到最后一跳才有总长度。
 */
export async function fetchRemoteInfo(ctx: AtelyxCtx, url: string): Promise<number | null> {
  // 探测本身失败（写配置失败、网络、镜像不支持 HEAD）也按「体积未知」处理，不把下载挡在门外
  const result = await runCurl(ctx, { url, method: "HEAD" }).catch(() => null);
  if (!result || result.code !== 0) return null;
  const sizes = [...result.stdout.matchAll(/^content-length:\s*(\d+)\s*$/gim)].map((m) => Number(m[1]));
  const size = sizes.length > 0 ? sizes[sizes.length - 1] : null;
  return size && size > 0 ? size : null;
}

/**
 * 剥掉 Markdown 记号并截断（详情页按纯文本展示）。只去成对的强调记号：量化名里的下划线
 * （`Q4_K_M`）是有效字符，全文标点清洗会把最有用的信息弄坏。
 */
export function stripMarkdown(text: string): string {
  return text
    .replace(/^\uFEFF/, "")
    .replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "") // YAML front matter（许可证、语言等元数据）
    .replace(/```[\s\S]*?```/g, " ") // 代码块
    .replace(/<[^>]+>/g, " ") // HTML 标签（模型卡的徽章、表格残留）
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ") // 图片
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1") // 链接保留文字
    .replace(/^#{1,6}\s*/gm, "") // 标题井号
    .replace(/^\s*[-*+]\s+/gm, "· ") // 列表项
    .replace(/\*\*([^*]+)\*\*/g, "$1") // 加粗
    .replace(/(^|\W)__([^_]+)__/g, "$1$2")
    .replace(/(^|\W)\*([^*\n]+)\*/g, "$1$2") // 斜体
    .replace(/`([^`]*)`/g, "$1") // 行内代码保留内容
    .replace(/[ \t]+/g, " ")
    .replace(/\n{3,}/g, "\n\n")
    .trim()
    .slice(0, README_MAX_CHARS);
}
