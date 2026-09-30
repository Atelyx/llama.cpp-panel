/**
 * curl 调用面：模型库与下载共用一个执行器。
 *
 * 走 curl 而非宿主 `ctx.http`：后者限本机/局域网且响应上限 1MB，公网域名与大文件都过不去；
 * 进程面只放行 cmd.exe / sh，故实际执行是 `cmd.exe /C curl ...` 与 `sh -c '...'`。
 *
 * URL 与全部选项写进 `--config` 文件，命令行上只留配置文件路径与 `--output` 路径：cmd 的转义
 * 规则自相矛盾（`%` 只在无引号时转义生效，含空格的路径又必须加引号），URL 直接上命令行无法同时
 * 满足；`--output` 同理不进配置文件——不带引号会被空格切成多个文件名，带引号则把引号算进文件名。
 */
import type { AtelyxCtx, ShellProcessHandle, ShellStreamHandlers } from "../ctx";
import { resolvePlatform } from "./process";

export interface CurlRequest {
  url: string;
  /** 缺省 GET；HEAD 用于取远端文件大小（不下载正文）。 */
  method?: "GET" | "HEAD";
  /** 每个数组项一行 `Name: Value`，仅允许 ASCII（curl 要求）。 */
  headers?: string[];
  /** 输出文件路径；省略则正文进 stdout。 */
  output?: string;
  /** 落盘目录层级不存在时由 curl 补建（下载到「发布者/模型名」这类新目录用）。 */
  createDirs?: boolean;
  /** 断点续传（curl -C -，按已存在文件的长度接着下）。 */
  resume?: boolean;
  /** 传输失败时的重试次数（curl --retry）。 */
  retries?: number;
  /** 传输停滞判定（curl --speed-limit/--speed-time），长下载用。 */
  stall?: { limit: number; seconds: number };
}

export interface CurlResult {
  /** 进程退出码；被宿主结束时可能为 null。 */
  code: number | null;
  stdout: string;
  stderr: string;
}

/** curl 未安装时的可操作提示。 */
export const CURL_MISSING_HINT =
  "找不到 curl：Windows 10 1803+ 与主流 Linux 自带 curl；" +
  "若确实缺失，Windows 可在「设置 → 应用 → 可选功能」安装，Linux 用包管理器装 curl。";

/** 单次取数（API / 大小探测）的超时；抓不动模型分类也不该挂住面板。 */
const DATA_TIMEOUT_SEC = 25;
/** 建立连接的超时：站点只连不答时靠它尽早失败，不拖满总超时。 */
const CONNECT_TIMEOUT_SEC = 15;
/**
 * 一次取数的响应上限。超过它是整个请求失败、不是截断，而仓库文件树在文件多的仓库里
 * 可能很大，取小了会让详情页整个打不开，故给足余量。
 */
const DATA_MAX_BYTES = 5_000_000;
/** 插件私有目录里的临时配置文件前缀。 */
const CONFIG_PREFIX = "curl-";

/**
 * 调用序号：配置文件按调用各自独立。
 *
 * 下载是长驻进程，curl 在启动时读一次配置；若其余请求复用同一个文件名，会把正在跑的
 * 下载配置覆盖掉。因此每次调用用独立文件。
 */
let configSeq = 0;

/**
 * 在用中的配置文件，清理旧文件时跳过——否则并发调用（详情页同时取文件树与仓库信息）会互相
 * 删掉对方刚写好的配置，对方的 curl 随即报「打不开配置文件」。
 */
const liveConfigs = new Set<string>();

/** 把请求写进私有目录的独立配置文件并返回该路径。 */
async function writeConfig(ctx: AtelyxCtx, req: CurlRequest): Promise<string> {
  const dir = await ctx.fs.privateDir();
  const folder = dir.replace(/\\/g, "/").replace(/\/+$/, "");
  try {
    const listing = await ctx.fs.listDir(folder);
    for (const entry of listing.entries) {
      const path = `${folder}/${entry.name}`;
      if (entry.kind === "file" && entry.name.startsWith(CONFIG_PREFIX) && !liveConfigs.has(path)) {
        await ctx.fs.deleteFile(path).catch(() => undefined);
      }
    }
  } catch {
    // 清不掉旧文件不影响本次调用
  }
  configSeq += 1;
  const path = `${folder}/${CONFIG_PREFIX}${configSeq}.conf`;
  // 先登记再落盘：落盘期间并发调用清理时不会把它当成旧文件删掉
  liveConfigs.add(path);
  try {
    await ctx.fs.writeFile(path, curlConfig(req));
  } catch (err) {
    liveConfigs.delete(path);
    throw err;
  }
  return path;
}

/** 用完释放配置文件：从在用集合摘除并删除（删不掉由下次调用清理）。 */
async function releaseConfig(ctx: AtelyxCtx, path: string): Promise<void> {
  liveConfigs.delete(path);
  try {
    await ctx.fs.deleteFile(path);
  } catch {
    // 删不掉不影响本次调用的结果
  }
}

/** curl 配置文件里的一行 `名字 = "值"`（值里的 `"` 与 `\` 要转义；仅 URL 与请求头用）。 */
function quotedLine(key: string, value: string): string {
  return `${key} = "${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

/**
 * 组装 curl 配置文件内容。总时长与体积上限只加在「取接口正文」上——下载不设这两条：
 * 模型动辄数 GB、耗时以小时计，带上限会让真实文件必然以超时或超体积失败。
 */
export function curlConfig(req: CurlRequest): string {
  const lines = ["silent", "show-error", "fail", "location", `connect-timeout = ${CONNECT_TIMEOUT_SEC}`];
  if (req.method === "HEAD") {
    lines.push("head", `max-time = ${DATA_TIMEOUT_SEC}`);
  } else if (!req.output) {
    lines.push(`max-time = ${DATA_TIMEOUT_SEC}`, `max-filesize = ${DATA_MAX_BYTES}`);
  }
  if (req.retries != null) lines.push(`retry = ${req.retries}`, "retry-delay = 2");
  if (req.stall) lines.push(`speed-limit = ${req.stall.limit}`, `speed-time = ${req.stall.seconds}`);
  // 下载目录（发布者/模型名）首次下载时还不存在，交给 curl 补建，省去自己造目录的绕路
  if (req.createDirs) lines.push("create-dirs");
  // 断点续传只对取正文成立；对 HEAD 传它会让头部探测去比对本地文件长度
  if (req.resume && req.method !== "HEAD") lines.push('continue-at = "-"');
  for (const header of req.headers ?? []) lines.push(quotedLine("header", header));
  lines.push(quotedLine("url", req.url));
  return `${lines.join("\n")}\n`;
}

/** 参数里出现空白或 shell 元字符时加单引号（Unix 的 `sh -c` 串用）。 */
function shQuote(value: string): string {
  return /[\s"\\$`&|<>()*?]/.test(value) ? `'${value.replace(/'/g, `'\\''`)}'` : value;
}

/**
 * 按平台把 curl 调用拼成一次进程参数。
 *
 * 命令行上只有三个东西：配置文件路径（插件私有目录，无特殊字符）、`--output` 与落盘路径。
 * 落盘路径可能含空格（模型目录），必须加引号——cmd 与 sh 都会按引号把整段当一个参数。
 */
export function shellInvocation(
  platform: "windows" | "unix",
  configPath: string,
  output?: string,
): { args: string[] } {
  const tail = output ? ["--output", output] : [];
  if (platform === "windows") return { args: ["/C", "curl.exe", "--config", configPath, ...tail] };
  return { args: ["-c", ["curl", "--config", shQuote(configPath), ...tail.map(shQuote)].join(" ")] };
}

/** URL 路径段编码（保留 `/` 由调用方按段拼；`!` 转义以免历史展开）。 */
export function anyToSegment(value: string): string {
  return encodeURIComponent(value).replace(/[!'()*]/g, (ch) => `%${ch.charCodeAt(0).toString(16).toUpperCase()}`);
}

/** 查询串参数值编码（空格编码为 %20，避免被解释为 `+`）。 */
export function queryValue(value: string): string {
  return encodeURIComponent(value);
}

/** 执行一次 curl 取数。进程启动失败（curl 缺失等）抛出可读原因。 */
export async function runCurl(ctx: AtelyxCtx, req: CurlRequest): Promise<CurlResult> {
  const platform = await resolvePlatform(ctx);
  let configPath: string;
  try {
    configPath = await writeConfig(ctx, req);
  } catch (err) {
    throw new Error(`无法写入请求配置：${err instanceof Error ? err.message : String(err)}`);
  }
  try {
    const { args } = shellInvocation(platform, configPath, req.output);
    let result;
    try {
      result = await ctx.shell.exec({ command: platform === "windows" ? "cmd.exe" : "sh", args });
    } catch (err) {
      throw new Error(`${CURL_MISSING_HINT}（${err instanceof Error ? err.message : String(err)}）`);
    }
    return {
      code: result?.code ?? null,
      stdout: result?.stdout ?? "",
      stderr: result?.stderr ?? "",
    };
  } finally {
    // curl 读完配置就关，进程结束后即可回收
    await releaseConfig(ctx, configPath);
  }
}

/** 启动一次长时 curl（下载）：进程中途输出只用于留错误尾巴，进度靠轮询落盘文件。 */
export async function spawnCurl(
  ctx: AtelyxCtx,
  req: CurlRequest,
  handlers: { onStderr(line: string): void; onExit(code: number | null): void; onError(message: string): void },
): Promise<ShellProcessHandle> {
  const platform = await resolvePlatform(ctx);
  const configPath = await writeConfig(ctx, req);
  const { args } = shellInvocation(platform, configPath, req.output);
  const raw: ShellStreamHandlers = {
    chunk: ({ stream, data }) => {
      if (stream === "stderr") handlers.onStderr(data);
    },
    // 进程结束即回收配置：下载配置要活到 curl 退出，不能提前删
    end: ({ code }) => {
      void releaseConfig(ctx, configPath);
      handlers.onExit(code);
    },
    error: (message) => {
      void releaseConfig(ctx, configPath);
      handlers.onError(message);
    },
  };
  try {
    return await ctx.shell.spawn({ command: platform === "windows" ? "cmd.exe" : "sh", args }, raw);
  } catch (err) {
    await releaseConfig(ctx, configPath);
    throw err;
  }
}

/** 判断 stderr 是否表明 curl 缺失。 */
export function isMissingCurl(stderr: string): boolean {
  return /not recognized|command not found|No such file/i.test(stderr);
}

/**
 * 取数并把正文解析成 JSON。
 *
 * curl `--fail` 命中 HTTP 错误时正文里是错误说明而不是 JSON，这里统一翻成可读原因。
 */
export async function fetchJson<T>(ctx: AtelyxCtx, url: string, headers?: string[]): Promise<T> {
  const result = await runCurl(ctx, { url, headers });
  if (result.code !== 0) throw new Error(describeCurlFailure(result, url));
  const text = result.stdout.trim();
  if (!text) throw new Error(`接口没有返回内容（${url}）`);
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error(`接口返回的不是 JSON（${url}）：${text.slice(0, 120)}`);
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

/** 一次取数失败的说明（截断 stderr 尾部，留最后一条 curl 错误）。 */
export function describeCurlFailure(result: CurlResult, url: string): string {
  const tail = result.stderr.trim().split(/\r?\n/).filter(Boolean).pop() ?? "";
  if (isMissingCurl(result.stderr)) return CURL_MISSING_HINT;
  if (tail) return `请求失败：${tail}`;
  return `${describeCurlExit(result.code, result.stderr)}（${url}）`;
}
