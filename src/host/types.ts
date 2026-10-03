/**
 * 模型库/下载共用的数据结构与格式化。
 * 体积一律十进制（GB = 10⁹ 字节）：与模型发布页的标注口径一致，便于用户核对。
 */

/** 模型仓库摘要（搜索列表与详情页共用）。 */
export interface ModelSummary {
  /** 仓库全名，形如 `Qwen/Qwen2.5-7B-Instruct-GGUF`。 */
  repo: string;
  downloads: number;
  likes: number;
  /** ISO 时间串；空串表示仓库未给出。 */
  updatedAt: string;
}

/** 仓库里的一个文件条目。 */
export interface RepoFile {
  /** 仓库内相对路径（可能含子目录）。 */
  name: string;
  /** 字节大小；仓库未给出时缺省。 */
  size?: number;
}

/** 仓库详情：摘要 + gguf 文件清单 + 模型卡简介。 */
export interface RepoDetail {
  summary: ModelSummary;
  /** 仅 `.gguf`，按体积升序（小的量化在前，便于按显存挑）。 */
  files: RepoFile[];
  /** 模型卡 README 原文（front matter 已去；空串 = 没有可展示的简介），详情页经宿主 markdown 内核渲染。 */
  readme: string;
  /** 是否为门控模型：需要授权才能下载，只能到浏览器登录后手动下载。 */
  gated: boolean;
}

/** 下载任务状态。`paused` 表示已取消但 `.part` 保留，可继续。 */
export type DownloadStatus = "idle" | "downloading" | "paused" | "done" | "error";

export interface DownloadTask {
  status: DownloadStatus;
  /** 仓库全名。 */
  repo: string;
  /**
   * 落盘文件名（纯文件名，不含目录）。仓库里的文件可能带子目录，落盘时压平成单段名——
   * 同一仓库的量化与视觉投影都平铺在「发布者/模型名」文件夹里，模型扫描才能按文件夹配上 mmproj。
   */
  file: string;
  /** 最终落盘全路径（/ 分隔，形如 `模型目录/发布者/模型名/文件名`）。 */
  target: string;
  /** 完整下载地址（含所选镜像域名）。 */
  url: string;
  /** 总字节数；来源未给出时为 null。 */
  totalBytes: number | null;
  /** 已写入的字节数（轮询临时文件实测，含断点续传已存在的部分）。 */
  bytes: number;
  statusText: string;
}

/** 无效字节数统一按 0 处理（来源缺字段时不该让合计变成 NaN）。 */
export function num(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

/** 取路径的文件名部分（正/反斜杠都认）。 */
export function fileNameOf(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() ?? "";
}

/** 字节 → 人类可读大小（十进制单位；无值返回空串）。 */
export function formatSize(bytes: number | undefined | null): string {
  if (bytes == null) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let i = 0;
  while (value >= 1000 && i < units.length - 1) {
    value /= 1000;
    i += 1;
  }
  return `${i === 0 ? value : value.toFixed(1)} ${units[i]}`;
}

/** 将 YYYY-MM-DD 开头的 ISO 时间串压成日期；解析不出返回原样。 */
export function shortDate(iso: string): string {
  const m = /^(\d{4}-\d{2}-\d{2})/.exec(iso);
  return m ? m[1] : iso;
}
