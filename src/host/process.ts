/**
 * 启动命令组装：把设置拼成 llama-server 的进程参数。
 *
 * 两条约束决定实现：
 * 1. 可执行程序只有 cmd.exe（Windows）/ sh（Unix）登记，启动命令必须写成整行经
 *    /C、-c 转达，不能直接指定 llama-server 本身。
 * 2. 包装层是 cmd.exe/sh —— 只结束包装进程会把真正的服务留成孤儿，因此经 spawn
 *    拿到的句柄 cancel() 结束整棵进程树，而不是按命令行特征找进程。
 *
 * 用户选的是 llama-server 所在文件夹而不是可执行文件本身：同一份发布里可执行文件名
 * 随平台变（Windows 带 .exe），让代码按平台名去文件夹里找，用户换平台不必重选。
 */
import type { AtelyxCtx, ListDirResult, ShellStreamHandlers, ShellProcessHandle } from "../ctx";
import { paramsFor, type LlamaSettings } from "../settings";
import { describeFsError } from "./models";

export type Platform = "windows" | "unix";

/** 平台决定命令写法，判错就启不动，因此取 Atelyx 给的值而不是猜 UA。 */
export function toPlatform(raw: string): Platform {
  return raw.toLowerCase().startsWith("windows") ? "windows" : "unix";
}

export async function resolvePlatform(ctx: AtelyxCtx): Promise<Platform> {
  try {
    return toPlatform(await ctx.app.platform());
  } catch {
    // 取不到时按 Unix 处理，不会误用 Windows 专有写法
    return "unix";
  }
}

/** 附加启动参数切分：空白分隔，双引号内保留空白并去掉引号。 */
export function splitArgs(text: string): string[] {
  const out: string[] = [];
  let current = "";
  let started = false;
  let quoted = false;
  for (const ch of text) {
    if (ch === '"') {
      quoted = !quoted;
      started = true;
      continue;
    }
    if (!quoted && /\s/.test(ch)) {
      if (started) out.push(current);
      current = "";
      started = false;
      continue;
    }
    current += ch;
    started = true;
  }
  if (started) out.push(current);
  return out;
}

/** 平台对应的 llama-server 可执行文件名。 */
export function executableName(platform: Platform): string {
  return platform === "windows" ? "llama-server.exe" : "llama-server";
}

/**
 * 在所选文件夹里找 llama-server：先按平台标准名精确匹配（大小写不敏感），再退到
 * 同前缀的改名构建（如 llama-server-cuda.exe）；找不到返回 null，由调用方给提示。
 */
export async function findServerExecutable(
  ctx: AtelyxCtx,
  platform: Platform,
  serverDir: string,
): Promise<string | null> {
  const folder = serverDir.trim().replace(/\\+/g, "/").replace(/\/+$/, "");
  if (!folder) return null;
  let listing: ListDirResult;
  try {
    listing = await ctx.fs.listDir(folder);
  } catch (err: unknown) {
    throw new Error(describeFsError(err, "llama-server 文件夹"));
  }
  const files = listing.entries.filter((entry) => entry.kind === "file");
  const want = executableName(platform);
  const exact = files.find((entry) => entry.name.toLowerCase() === want.toLowerCase());
  if (exact) return `${folder}/${exact.name}`;
  const renamed = files
    .map((entry) => entry.name)
    .filter((name) => {
      const stem = name.replace(/\.exe$/i, "").toLowerCase();
      // Windows 只认 .exe：同目录还可能有 dll，前缀相同也不该被当成程序
      return stem.startsWith("llama-server") && (platform === "unix" || /\.exe$/i.test(name));
    })
    .sort((a, b) => a.localeCompare(b));
  return renamed.length > 0 ? `${folder}/${renamed[0]}` : null;
}

/** 找不到可执行文件时的可操作提示。 */
export function missingExecutableMessage(serverDir: string, platform: Platform): string {
  const folder = serverDir.trim();
  if (!folder) return "未配置 llama-server 文件夹";
  return `在 ${folder} 里没找到 ${executableName(platform)}`;
}

/** llama-server 参数（不含包装层）。`--model` 是模型标志（llama.cpp server 惯用写法）。 */
export function buildStartTokens(settings: LlamaSettings, serverExe: string): string[] {
  const model = settings.modelFile.trim();
  if (!model) throw new Error("未选择模型文件");
  // --host 必须显式传：llama-server 默认只绑 127.0.0.1，局域网设备调不了 API
  const tokens = [
    serverExe,
    "--host",
    settings.host.trim() || "127.0.0.1",
    "--model",
    model,
    "--port",
    String(settings.port),
  ];
  // 多模态模型的视觉投影：配置了就挂上（面板选中文件夹时会自动带出）
  const mmproj = settings.mmprojFile.trim();
  if (mmproj) tokens.push("--mmproj", mmproj);
  // 启动参数按模型文件夹持久化：换模型即换参数。旗标拼写对照 llama-server 官方文档。
  const params = paramsFor(settings, dirname(model));
  if (params.ngl !== null) tokens.push("-ngl", String(params.ngl));
  if (params.ctxSize !== null) tokens.push("--ctx-size", String(params.ctxSize));
  if (params.threads !== null) tokens.push("--threads", String(params.threads));
  if (params.parallel !== null) tokens.push("--parallel", String(params.parallel));
  if (params.cacheTypeK) tokens.push("--cache-type-k", params.cacheTypeK);
  // flash-attn 在新版本里是三态旗标，显式传 on 避免缺省语义漂移；
  // mmap/mlock 已被上游合并进 --load-mode（旧旗标会打废弃警告）
  if (params.flashAttn) tokens.push("--flash-attn", "on");
  if (params.jinja) tokens.push("--jinja");
  if (params.noWebui) tokens.push("--no-webui");
  if (params.loadMode) tokens.push("--load-mode", params.loadMode);
  return [...tokens, ...splitArgs(params.extraArgs)];
}

/** 取路径的目录部分；无分隔符则空串。Windows 反斜杠/正斜杠都认。 */
function dirname(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx === -1 ? "" : norm.slice(0, idx);
}

/** 含空白或引号时加引号（仅用于展示与 Unix 的 -c 串）。 */
function shellQuote(value: string): string {
  return /[\s"\\]/.test(value) ? `"${value.replace(/"/g, '\\"')}"` : value;
}

/** 供界面展示的命令行（仅为可读，执行不走它）。 */
export function describeStartCommand(settings: LlamaSettings, serverExe: string): string {
  return buildStartTokens(settings, serverExe).map(shellQuote).join(" ");
}

/** 按平台组装执行参数。 */
function runArgs(platform: Platform, tokens: string[]): string[] {
  // Windows：/C 之后逐项给，引号交给 Atelyx 按需添加
  if (platform === "windows") return ["/C", ...tokens];
  // Unix：-c 只吃一个脚本文本，在这里拼成命令行
  return ["-c", tokens.map(shellQuote).join(" ")];
}

/** 启动参数。 */
export interface StartHandlers {
  onLog(line: string, stream: "stdout" | "stderr"): void;
  onExit(code: number | null): void;
  onError(message: string): void;
}

/**
 * 启动 llama-server 并返回句柄。
 *
 * 句柄须由调用方保存：llama-server 是长驻服务，只能靠它停下。工作目录取模型所在目录，
 * 让 llama-server 的相对路径（如缓存）落到模型目录旁。
 */
export async function startLlama(
  ctx: AtelyxCtx,
  platform: Platform,
  settings: LlamaSettings,
  serverExe: string,
  handlers: StartHandlers,
): Promise<ShellProcessHandle> {
  const command = platform === "windows" ? "cmd.exe" : "sh";
  const args = runArgs(platform, buildStartTokens(settings, serverExe));
  const cwd = dirname(settings.modelFile.trim());
  // 宿主只给原始流（chunk/end/error），我的上层回调（onLog/onExit/onError）在此映射
  const raw: ShellStreamHandlers = {
    chunk: ({ stream, data }) => handlers.onLog(data, stream),
    end: ({ code }) => handlers.onExit(code),
    error: (message) => handlers.onError(message),
  };
  return ctx.shell.spawn({ command, args, ...(cwd ? { cwd } : {}) }, raw);
}
