/**
 * 自动启动待命：服务未运行时用占位进程监听服务端口——llama-server 不在时外部只会收到连接拒绝，
 * 占住端口是看见外部调用的唯一途径。占位进程 = 随应用分发的脚本运行时跑 watchScripts 的 Node 载荷，
 * 触发判定由占位进程过滤（仅真实 API 调用），待命期间本插件对端口的探测挂起：占位进程不对外服务，
 * 探测只会得到关闭的连接，纯无谓流量。触发契约见 watchScripts：stdout 不带前缀的首行到达即触发；
 * 带 NOISE_PREFIX 前缀的行是待命期间收到的非触发请求，经 onNoise 落日志；触发后占位进程转为
 * 请求转发器自灭，句柄就此摘除，disarm 不再取消它；触发前的异常退出 = 待命异常。
 */
import type { AtelyxCtx, BundledRuntimeInfo, ProcessHandle, ProcessStreamHandlers } from "../ctx";
import type { LlamaSettings } from "../settings";
import { NOISE_PREFIX, NODE_SCRIPT } from "./watchScripts";

/** 脚本运行时缺失时的可操作提示。 */
const RUNTIME_MISSING_HINT =
  "未找到随应用分发的脚本运行时，外部调用自动启动不可用（更新 Atelyx 后重试）";

export interface WatchHooks {
  /** 外部 API 调用触发；requestLine = 触发请求的首行，userAgent = 调用方自报身份（日志定位用）。 */
  onTrigger(requestLine: string, userAgent: string): void;
  /** 待命期间收到的非触发请求（噪音）：首行已去前缀，仅落日志，不触发启动。 */
  onNoise(line: string): void;
  /** 待命异常结束（绑定失败等），带可读原因。 */
  onBroken(reason: string): void;
}

/** arm 的结果：ok=false 时 error 说明原因（触发也返回 ok=true，启动由 onTrigger 接手）。 */
export interface ArmOutcome {
  ok: boolean;
  error?: string;
}

/** watchScripts 在请求行后拼接的调用方标识前缀。 */
const UA_SUFFIX = " | ua=";

export class ApiCallWatcher {
  private readonly ctx: AtelyxCtx;
  private handle: ProcessHandle | null = null;
  private host = "";
  private port = 0;
  /** 每次 arm/disarm 自增；占位进程的退出事件按代过滤，上一代的迟到事件直接忽略。 */
  private gen = 0;
  /** arm/disarm 串行队列：并发调用按序执行，防「 disarm 先跑、arm 后落地」留下该退出的进程。 */
  private queue: Promise<unknown> = Promise.resolve();
  /** 占位进程的最后一段错误输出：绑定失败等原因都在末尾。 */
  private stderrTail = "";
  /** 触发请求的首行：占位进程经 stdout 带回（不含 ua 后缀），日志与通知用它指明来源。 */
  private triggerLine = "";
  private scriptPath: string | null = null;
  private runtime: BundledRuntimeInfo | null = null;

  constructor(ctx: AtelyxCtx) {
    this.ctx = ctx;
  }

  isArmed(): boolean {
    return this.handle !== null;
  }

  arm(settings: LlamaSettings, hooks: WatchHooks): Promise<ArmOutcome> {
    return this.enqueue(() => this.doArm(settings, hooks));
  }

  /** 解除待命。返回是否真的解除了一个在待命的进程（供调用方决定要不要记日志）。 */
  disarm(): Promise<boolean> {
    return this.enqueue(() => this.doDisarm());
  }

  private enqueue<T>(task: () => Promise<T>): Promise<T> {
    // 前序任务失败也继续跑：队列只保序，不传播失败（失败经返回值/异常给到调用方）
    const run = this.queue.then(task, task);
    this.queue = run.catch(() => undefined);
    return run;
  }

  private async doArm(settings: LlamaSettings, hooks: WatchHooks): Promise<ArmOutcome> {
    const host = settings.host.trim() || "127.0.0.1";
    const port = settings.port;
    if (this.handle && this.host === host && this.port === port) return { ok: true };
    await this.doDisarm();
    // 回环地址与 llama-server 同绑 127.0.0.1；局域网地址绑 0.0.0.0：绑特定 IP 收不到本机回环调用，DHCP 换址也会失效
    const bindHost = host === "127.0.0.1" || host.toLowerCase() === "localhost" ? "127.0.0.1" : "0.0.0.0";
    // llama-server 将绑定的地址：就绪探测与转发指向它
    const serverHost = host === "0.0.0.0" ? "127.0.0.1" : host;

    const runtime = this.runtime ?? (this.runtime = await this.ctx.process.bundledRuntime().catch(() => null));
    if (!runtime) return { ok: false, error: RUNTIME_MISSING_HINT };
    const scriptPath = await this.writeScript();

    const gen = ++this.gen;
    this.stderrTail = "";
    this.triggerLine = "";
    let ended = false;
    let broken = "";
    const fail = (reason: string): void => {
      if (ended) return;
      ended = true;
      broken = reason;
      hooks.onBroken(reason);
    };
    const trigger = (requestLine: string, userAgent: string): void => {
      if (ended) return;
      ended = true;
      hooks.onTrigger(requestLine, userAgent);
    };
    const raw: ProcessStreamHandlers = {
      chunk: ({ stream, data }) => {
        if (gen !== this.gen) return;
        if (stream === "stderr" && data.trim()) this.stderrTail = data.trim();
        if (stream !== "stdout" || this.triggerLine) return;
        // 逐行消费：噪音行与触发行可能合并在同一段输出里；不带前缀的首行即触发，
        // 格式 =「请求行 + " | ua=" + User-Agent」，后缀由 watchScripts 拼接，解析在此收口。
        // 只去尾部空白：前缀自带尾随空格，整体 trim 会让空载荷的噪音行（首行为空的垃圾请求）
        // 判不上前缀而误成触发
        for (const rawLine of data.split("\n")) {
          const line = rawLine.trimEnd();
          if (!line.trim()) continue;
          if (line.startsWith(NOISE_PREFIX)) {
            const text = line.slice(NOISE_PREFIX.length).trim();
            if (text) hooks.onNoise(text);
            continue;
          }
          const first = line.trim();
          const sep = first.lastIndexOf(UA_SUFFIX);
          this.triggerLine = sep >= 0 ? first.slice(0, sep).trim() : first;
          trigger(this.triggerLine, sep >= 0 ? first.slice(sep + UA_SUFFIX.length).trim() : "");
          this.handle = null;
          break;
        }
      },
      end: ({ code }) => {
        if (gen !== this.gen || ended) return;
        // 兜底：没有 stdout 首行的正常退出仍按触发处理
        if (code === 0) trigger(this.triggerLine, "");
        else fail(`待命进程异常退出（退出码 ${code ?? "未知"}）${clip(this.stderrTail)}`);
      },
      error: (message) => {
        if (gen !== this.gen) return;
        fail(message);
      },
    };

    let handle: ProcessHandle;
    try {
      handle = await this.ctx.process.spawn(
        { command: runtime.path, args: [scriptPath, bindHost, String(port), serverHost] },
        raw,
      );
    } catch (err) {
      fail(`待命进程未能启动：${err instanceof Error ? err.message : String(err)}`);
      return { ok: false, error: broken };
    }
    // 句柄落地前进程就已退出：绑定失败等立即报错走这条路，不能再把句柄当「在待命」
    if (ended) {
      return broken ? { ok: false, error: broken } : { ok: true };
    }
    this.handle = handle;
    this.host = host;
    this.port = port;
    return { ok: true };
  }

  private async doDisarm(): Promise<boolean> {
    this.gen += 1;
    const handle = this.handle;
    this.handle = null;
    this.host = "";
    this.port = 0;
    if (!handle) return false;
    try {
      await handle.cancel();
    } catch {
      // 进程已经不在（被宿主收尾等）：解除的目的已达成
    }
    return true;
  }

  /** 待命脚本写进私有目录（一次即可，跨多次 arm 复用）。 */
  private async writeScript(): Promise<string> {
    if (this.scriptPath) return this.scriptPath;
    const dir = await this.ctx.fs.privateDir();
    const path = `${dir}/autostart-watch.mjs`;
    const result = await this.ctx.fs.writeFile(path, NODE_SCRIPT);
    if (!result.ok) throw new Error(`写待命脚本失败：${result.summary}`);
    this.scriptPath = path;
    return path;
  }
}

/** 错误输出只取最后一段非空行：绑定失败等原因都在末尾。 */
function lastLine(text: string): string {
  const lines = text.split("\n").map((line) => line.trim()).filter(Boolean);
  return lines.length > 0 ? lines[lines.length - 1] : "";
}

function clip(text: string, max = 160): string {
  const line = lastLine(text);
  return line ? `：${line.length > max ? `${line.slice(0, max)}…` : line}` : "";
}
