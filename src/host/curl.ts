/**
 * curl 下载执行面（模型下载专用）。
 *
 * 下载走系统 curl 而非宿主 `ctx.http`：后者响应上限 1MB，模型动辄数 GB；断点续传与停滞检测
 * 也依赖 curl 的 `-C -`/`--speed-*`。取数（检索/文件树/模型卡/体积探测）走 `ctx.http`，见 hf.ts。
 *
 * 参数经宿主进程面直传操作系统（不经 shell 转述），URL 与含空格的落盘路径都作为独立参数
 * 原样到达 curl。
 */
import type { AtelyxCtx, ProcessHandle, ProcessStreamHandlers } from "../ctx";

/** 下载请求。 */
export interface CurlDownloadRequest {
  url: string;
  /** 落盘文件路径。 */
  output: string;
  /** 落盘目录层级不存在时由 curl 补建（下载到「发布者/模型名」这类新目录用）。 */
  createDirs?: boolean;
  /** 断点续传（curl -C -，按已存在文件的长度接着下）。 */
  resume?: boolean;
  /** 传输失败时的重试次数（curl --retry）。 */
  retries?: number;
  /** 传输停滞判定（curl --speed-limit/--speed-time），长下载用。 */
  stall?: { limit: number; seconds: number };
}

/** curl 未安装时的可操作提示。 */
export const CURL_MISSING_HINT =
  "找不到 curl：Windows 10 1803+ 与主流 Linux 自带 curl；" +
  "若确实缺失，Windows 可在「设置 → 应用 → 可选功能」安装，Linux 用包管理器装 curl。";

/** 建立连接的超时：站点只连不答时靠它尽早失败。 */
const CONNECT_TIMEOUT_SEC = 15;

/**
 * 组装 curl 参数。下载不设总时长与体积上限：模型再大也要能下完，带上限会让真实文件
 * 必然以超时或超体积失败。
 */
function curlArgs(req: CurlDownloadRequest): string[] {
  const args = ["--silent", "--show-error", "--fail", "--location", "--connect-timeout", String(CONNECT_TIMEOUT_SEC)];
  if (req.createDirs) args.push("--create-dirs");
  if (req.resume) args.push("--continue-at", "-");
  if (req.retries != null) args.push("--retry", String(req.retries), "--retry-delay", "2");
  if (req.stall) {
    args.push("--speed-limit", String(req.stall.limit), "--speed-time", String(req.stall.seconds));
  }
  args.push("--output", req.output, req.url);
  return args;
}

/**
 * 启动一次长时下载：进程中途输出只用于留错误尾巴，进度靠轮询落盘文件。
 * curl 缺失时 spawn 直接失败，翻成可操作的安装提示抛出。
 */
export async function spawnCurl(
  ctx: AtelyxCtx,
  req: CurlDownloadRequest,
  handlers: { onStderr(line: string): void; onExit(code: number | null): void; onError(message: string): void },
): Promise<ProcessHandle> {
  const raw: ProcessStreamHandlers = {
    chunk: ({ stream, data }) => {
      if (stream === "stderr") handlers.onStderr(data);
    },
    end: ({ code }) => handlers.onExit(code),
    error: handlers.onError,
  };
  try {
    return await ctx.process.spawn({ command: "curl", args: curlArgs(req) }, raw);
  } catch (err) {
    throw new Error(`${CURL_MISSING_HINT}（${err instanceof Error ? err.message : String(err)}）`);
  }
}

/** curl 退出码 → 可操作的中文说明（对照 curl 官方退出码表）。 */
export function describeCurlExit(code: number | null, stderr = ""): string {
  if (code === null) return "已中断";
  const byCode: Record<number, string> = {
    1: "curl 用法错误",
    3: "地址格式不对",
    5: "无法解析代理地址",
    6: "域名解析失败：检查网络或换个镜像站",
    7: "连不上服务器：地址不通或被防火墙拦截",
    18: "传输中断：服务端提前结束响应",
    22: "服务器返回了错误状态（文件不存在或没有权限）",
    23: "写文件失败：检查目标目录是否存在、是否有写权限、磁盘是否已满",
    28: "网络超时：站点无响应或速度过慢",
    33: "服务器不支持分段下载（无法断点续传）",
    35: "TLS 握手失败：可能被网络中间设备拦截，换个镜像站试试",
    36: "下载的数据不完整",
    47: "重定向次数过多",
    55: "发送请求失败",
    56: "接收数据失败：连接被中断",
    60: "证书校验失败",
    63: "文件大小超出预期",
  };
  const known = byCode[code];
  if (known) return known;
  const tail = stderr.trim().split(/\r?\n/).filter(Boolean).pop();
  return tail ? `下载失败：${tail}` : `下载失败（curl 退出码 ${code}）`;
}
