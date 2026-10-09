/**
 * 镜像站表与地址解析。
 * 镜像是整站内容镜像（仓库与文件名一致），换镜像 = 换域名：取用户输入的路径与所选域名重新拼装；
 * ModelScope 地址结构不同，不做域名替换。
 */

/** URL 路径段编码（保留 `/` 由调用方按段拼；`!` 转义以免历史展开）。 */
function anyToSegment(value: string): string {
  return encodeURIComponent(value).replace(/[!'()*]/g, (ch) => `%${ch.charCodeAt(0).toString(16).toUpperCase()}`);
}

/** 查询串参数值编码（空格编码为 %20，避免被解释为 `+`）。 */
function queryValue(value: string): string {
  return encodeURIComponent(value);
}

/** 镜像站（HF 通用镜像，纯域名替换）。 */
export interface MirrorSource {
  id: string;
  label: string;
  /** 站点根地址（无尾斜杠）。 */
  origin: string;
  note: string;
}

export const MIRRORS: MirrorSource[] = [
  {
    id: "hf-mirror",
    label: "hf-mirror.com（国内镜像）",
    origin: "https://hf-mirror.com",
    note: "HuggingFace 整站镜像，国内可直连。",
  },
  {
    id: "hf",
    label: "huggingface.co（官方）",
    origin: "https://huggingface.co",
    note: "官方站点；国内直连通常失败。",
  },
];

export const DEFAULT_MIRROR_ID = MIRRORS[0].id;

export function mirrorById(id: string): MirrorSource {
  return MIRRORS.find((m) => m.id === id) ?? MIRRORS[0];
}

/** 仓库与文件名。 */
export interface RepoTarget {
  /** 仓库全名，形如 `owner/name`。 */
  repo: string;
  /** 文件名（不含子目录）。 */
  file: string;
}

export type ParseTargetResult = { ok: true; target: RepoTarget } | { ok: false; error: string };

/**
 * 从用户输入解析仓库与文件名：接受整条下载地址（`/resolve|blob/<分支>/<路径>`）或
 * `owner/repo/文件名` 简写，不猜其它形态。`resolve`/`blob` 后那一段是分支名，须跳过。
 */
export function parseTarget(input: string): ParseTargetResult {
  const text = input.trim();
  if (!text) return { ok: false, error: "请填写模型下载地址或 owner/repo/文件名" };

  let path = text;
  try {
    path = new URL(text).pathname;
  } catch {
    // 非完整地址：按 `owner/repo/文件名` 简写解析（下面按段数校验）
  }

  const segments = path.split("/").map(decodeSegment).filter((s) => s.length > 0);
  const marker = segments.findIndex((s) => s === "resolve" || s === "blob");
  // 分支名段只在标记后面确实还有文件时才算数（`.../resolve/main` 这种残地址按文件名处理）
  const fileStart = marker >= 0 && marker + 2 < segments.length ? marker + 2 : 0;
  const file = segments[segments.length - 1] ?? "";
  const repo = segments.slice(0, fileStart > 0 ? marker : segments.length - 1).slice(-2).join("/");

  if (!repo.includes("/") || !file) {
    return { ok: false, error: "地址里缺少仓库或文件名：需要 owner/repo/文件名" };
  }
  if (!/\.gguf$/i.test(file)) return { ok: false, error: `只能下载 .gguf 模型文件（收到「${file}」）` };
  return { ok: true, target: { repo, file } };
}

/** 片段解码：粘贴的地址可能带 %20 一类转义；解不开就按原样用。 */
function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

/** 仓库文件下载地址（镜像域名 + 仓库路径；每段单独编码）。 */
export function downloadUrl(mirror: MirrorSource, repo: string, file: string, revision = "main"): string {
  const repoPath = repo.split("/").map(anyToSegment).join("/");
  return `${mirror.origin}/${repoPath}/resolve/${anyToSegment(revision)}/${anyToSegment(file)}`;
}

/** 仓库 API 地址（tree / 详情 / 模型卡）。 */
export function repoApiUrl(mirror: MirrorSource, repo: string, suffix: string): string {
  const repoPath = repo.split("/").map(anyToSegment).join("/");
  return `${mirror.origin}/api/models/${repoPath}${suffix}`;
}

/** 模型检索地址。 */
export function searchApiUrl(
  mirror: MirrorSource,
  query: string,
  sort: string,
  limit: number,
): string {
  const parts = [
    "full=false",
    `limit=${limit}`,
    `sort=${queryValue(sort)}`,
    "direction=-1",
  ];
  if (query.trim()) parts.splice(1, 0, `search=${queryValue(query.trim())}`);
  return `${mirror.origin}/api/models?${parts.join("&")}`;
}

/** 模型卡（README）原文地址。 */
export function readmeUrl(mirror: MirrorSource, repo: string, revision = "main"): string {
  const repoPath = repo.split("/").map(anyToSegment).join("/");
  return `${mirror.origin}/${repoPath}/raw/${anyToSegment(revision)}/README.md`;
}

/** 目标目录里找一个不冲突的文件名：已存在时插 `(1)`、`(2)`… */
export function uniqueName(existing: Set<string>, name: string): string {
  if (!existing.has(name.toLowerCase())) return name;
  const dot = name.toLowerCase().lastIndexOf(".gguf");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  for (let i = 1; i < 1000; i += 1) {
    const candidate = `${stem} (${i})${ext}`;
    if (!existing.has(candidate.toLowerCase())) return candidate;
  }
  return `${stem}-${Date.now()}${ext}`;
}
