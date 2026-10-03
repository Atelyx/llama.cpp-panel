/**
 * 自动启动待命：服务未运行时用占位进程监听服务端口——llama-server 不在时外部只会收到连接拒绝，
 * 占住端口是看见外部调用的唯一途径，因此待命期间本插件对端口的探测必须挂起，否则会自己触发自己。
 * 触发契约见 watchScripts：stdout 首行到达即触发；触发后占位进程转为请求转发器自灭，
 * 句柄就此摘除，disarm 不再取消它；触发前的异常退出 = 待命异常。
 */
import type { AtelyxCtx, ShellProcessHandle, ShellStreamHandlers } from "../ctx";
import type { LlamaSettings } from "../settings";
import { shellQuote, type Platform } from "./process";
import { PL_SCRIPT, PS_SCRIPT, PY_SCRIPT } from "./watchScripts";

export interface WatchHooks {
  /** 外部 API 调用触发；requestLine = 触发请求的首行，供日志指明是什么调用。 */
  onTrigger(requestLine: string): void;
  /** 待命异常结束（绑定失败、解释器缺失等），带可读原因。 */
  onBroken(reason: string): void;
}

/** arm 的结果：ok=false 时 error 说明原因（触发也返回 ok=true，启动由 onTrigger 接手）。 */
export interface ArmOutcome {
  ok: boolean;
  error?: string;
}

export class ApiCallWatcher {
  private readonly ctx: AtelyxCtx;
  private handle: ShellProcessHandle | null = null;
  private host = "";
  private port = 0;
  /** 每次 arm/disarm 自增；占位进程的退出事件按代过滤，上一代的迟到事件直接忽略。 */
  private gen = 0;
  /** arm/disarm 串行队列：并发调用按序执行，防「 disarm 先跑、arm 后落地」留下该退出的进程。 */
  private queue: Promise<unknown> = Promise.resolve();
  /** 占位进程的最后一段错误输出：绑定失败等原因都在末尾。 */
  private stderrTail = "";
  /** 触发请求的首行：占位进程经 stdout 带回，日志与通知用它指明来源。 */
  private triggerLine = "";
  private scriptDir: string | null = null;
  private readonly writtenScripts = new Set<string>();

  constructor(ctx: AtelyxCtx) {
    this.ctx = ctx;
  }

  isArmed(): boolean {
    return this.handle !== null;
  }

  arm(platform: Platform, settings: LlamaSettings, hooks: WatchHooks): Promise<ArmOutcome> {
    return this.enqueue(() => this.doArm(platform, settings, hooks));
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

  private async doArm(platform: Platform, settings: LlamaSettings, hooks: WatchHooks): Promise<ArmOutcome> {
    const host = settings.host.trim() || "127.0.0.1";
    const port = settings.port;
    if (this.handle && this.host === host && this.port === port) return { ok: true };
    await this.doDisarm();
    // 回环地址与 llama-server 同绑 127.0.0.1；局域网地址绑 0.0.0.0：绑特定 IP 收不到本机回环调用，DHCP 换址也会失效
    const bindHost = host === "127.0.0.1" || host.toLowerCase() === "localhost" ? "127.0.0.1" : "0.0.0.0";
    // llama-server 将绑定的地址：就绪探测与转发指向它
    const serverHost = host === "0.0.0.0" ? "127.0.0.1" : host;

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
    const trigger = (requestLine: string): void => {
      if (ended) return;
      ended = true;
      hooks.onTrigger(requestLine);
    };
    const raw: ShellStreamHandlers = {
      chunk: ({ stream, data }) => {
        if (stream === "stderr" && data.trim()) this.stderrTail = data.trim();
        // stdout 首行即触发；触发后占位进程转为转发器自灭，句柄摘除，disarm 不再取消它
        if (stream === "stdout" && !this.triggerLine) {
          const line = data.trim();
          if (line) {
            this.triggerLine = line.split("\n")[0].trim();
            trigger(this.triggerLine);
            this.handle = null;
          }
        }
      },
      end: ({ code }) => {
        if (gen !== this.gen || ended) return;
        // 兜底：没有 stdout 首行的正常退出仍按触发处理
        if (code === 0) trigger(this.triggerLine);
        else fail(`待命进程异常退出（退出码 ${code ?? "未知"}）${clip(this.stderrTail)}`);
      },
      error: (message) => {
        if (gen !== this.gen) return;
        fail(message);
      },
    };

    const spawnOpts =
      platform === "windows"
        ? {
            command: "cmd.exe",
            args: [
              "/C",
              "powershell.exe",
              "-NoProfile",
              "-NonInteractive",
              "-ExecutionPolicy",
              "Bypass",
              "-File",
              await this.writeScript("autostart-watch.ps1", PS_SCRIPT),
              "-BindHost",
              bindHost,
              "-Port",
              String(port),
              "-ServerHost",
              serverHost,
            ],
          }
        : await this.unixSpawnOpts(bindHost, port, serverHost);
    if (!spawnOpts) {
      return {
        ok: false,
        error: "Linux 上未找到 python3 或 perl，自动启动待命不可用（安装其一后重试）",
      };
    }

    const handle = await this.ctx.shell.spawn(spawnOpts, raw);
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

  /** Linux 占位命令：依次探测 python3 → perl，都没有返回 null（调用方给可操作原因）。 */
  private async unixSpawnOpts(
    bindHost: string,
    port: number,
    serverHost: string,
  ): Promise<{ command: string; args: string[] } | null> {
    let interpreter = "";
    for (const candidate of ["python3", "perl"]) {
      const found = await this.ctx.shell.exec({ command: "sh", args: ["-c", `command -v ${candidate}`] });
      if (found && found.code === 0 && found.stdout.trim()) {
        interpreter = candidate;
        break;
      }
    }
    if (!interpreter) return null;
    const script = await this.writeScript(
      interpreter === "perl" ? "autostart-watch.pl" : "autostart-watch.py",
      interpreter === "perl" ? PL_SCRIPT : PY_SCRIPT,
    );
    const line = `${interpreter} ${shellQuote(script)} ${shellQuote(bindHost)} ${String(port)} ${shellQuote(serverHost)}`;
    return { command: "sh", args: ["-c", line] };
  }

  private async writeScript(fileName: string, content: string): Promise<string> {
    const dir = this.scriptDir ?? (this.scriptDir = await this.ctx.fs.privateDir());
    const path = `${dir}/${fileName}`;
    if (this.writtenScripts.has(fileName)) return path;
    const result = await this.ctx.fs.writeFile(path, content);
    if (!result.ok) throw new Error(`写待命脚本失败：${result.summary}`);
    this.writtenScripts.add(fileName);
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
