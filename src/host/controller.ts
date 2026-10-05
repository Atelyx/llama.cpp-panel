/**
 * 进程托管状态中心：启停、就绪判定与日志，独立于 runtime。
 * 进程（本机）与连接（网络）是两条信息：用户自起的服务连接通但不归本插件管，界面要同时呈现。
 */
import type { AtelyxCtx, ShellProcessHandle } from "../ctx";
import type { LlamaSettings } from "../settings";
import type { LlamaRuntime } from "../runtime";
import { ApiCallWatcher, type WatchHooks } from "./watcher";
import {
  describeStartCommand,
  findServerExecutable,
  missingExecutableMessage,
  resolvePlatform,
  startLlama,
  type Platform,
} from "./process";

export interface HostSnapshot {
  /** 是否由本插件启动。 */
  running: boolean;
  /** 正在启动（等接口就绪）。 */
  starting: boolean;
  /** 自动启动待命中：占位进程正监听服务端口，外部调用将触发启动。 */
  watch: boolean;
  /** 展示将要执行的命令，便于用户确认。 */
  startLine: string;
  /** 尾部若干行日志。 */
  logs: readonly string[];
  /** 日志累计行数（含被裁剪掉的）：裁剪后 logs.length 恒定，界面自动滚动要靠它感知新行。 */
  logSeq: number;
  /** 最近一次操作的错误。 */
  error: string;
}

/** 日志缓冲的裁剪上限（日志页签的页脚文案也用它，避免两份常量各说各话）。 */
export const MAX_LOGS = 300;
/** 加载大模型会慢，给足时间但不无限等。 */
const READY_TIMEOUT_MS = 180000;
/** 错误输出可能极长，进通知与快照前截断。 */
const ERROR_DETAIL_MAX = 200;
/** 跟随模式的握手间隔：兄弟实例的占位监听对该请求白名单放行，频一点也无妨。 */
const FOLLOWER_TICK_MS = 5000;

const EMPTY: HostSnapshot = {
  running: false,
  starting: false,
  watch: false,
  startLine: "",
  logs: [],
  logSeq: 0,
  error: "",
};

export class HostController {
  private readonly ctx: AtelyxCtx;
  private settings: LlamaSettings;
  private readonly runtime: LlamaRuntime;
  private readonly watcher: ApiCallWatcher;
  private readonly listeners = new Set<() => void>();
  private snap: HostSnapshot = EMPTY;
  /** 首次用到时向 Atelyx 问一次并缓存。 */
  private platform: Platform | null = null;
  /** 本插件启动的进程句柄；停止与「是否在运行」都依它判断。 */
  private handle: ShellProcessHandle | null = null;
  /** 等待就绪期间进程退出或出错的原因；非空即终止等待，不再空等满超时。 */
  private died = "";
  /** 最后一条非空错误输出：进程没打印可读原因时，它就是最接近现场的信息。 */
  private lastStderr = "";
  /** 日志累计行数（快照里叫 logSeq）。 */
  private logSeq = 0;
  /** 本次启动是否已弹过失败通知：spawn 失败会同时走 error 回调与 reject，只通知一次。 */
  private notified = false;
  /** 本次启动是否被用户主动停止：收场同样是进程退出，但不是失败。 */
  private cancelled = false;
  /** 自动启动暂缓：启动失败或待命异常后置位，手动启动或功能开启下的设置变更时清位。 */
  private watchPaused = false;
  /** 每轮暂缓最多提示一次；恢复待命能力后重新允许提示。 */
  private watchPauseNotified = false;
  /** 跟随模式定时器：端口被兄弟实例的占位监听持有时保持探测挂起，仅定时握手，非空即跟随中。 */
  private followerTimer: number | null = null;
  private disposed = false;

  constructor(ctx: AtelyxCtx, settings: LlamaSettings, runtime: LlamaRuntime) {
    this.ctx = ctx;
    this.settings = settings;
    this.runtime = runtime;
    this.watcher = new ApiCallWatcher(ctx);
    // 探测状态变化时重新裁决：外部起的服务下线 → 进入待命；在线 → 不占它的端口
    runtime.subscribe(() => this.evaluateWatcher());
  }

  /** 触发/异常回调收口：先收敛快照状态，再交给各自流程。 */
  private readonly watchHooks: WatchHooks = {
    onTrigger: (requestLine, userAgent) => {
      if (this.disposed) return;
      if (this.snap.watch) this.emit({ watch: false });
      void this.autoStart(requestLine, userAgent);
    },
    onBroken: (reason) => {
      if (this.disposed) return;
      if (this.snap.watch) this.emit({ watch: false });
      void this.classifyArmFailure(reason);
    },
  };

  private async getPlatform(): Promise<Platform> {
    if (this.platform === null) this.platform = await resolvePlatform(this.ctx);
    return this.platform;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): HostSnapshot => this.snap;

  applySettings(settings: LlamaSettings): void {
    this.settings = settings;
    // 功能开启下的设置变更是用户重新表态：清掉暂缓，让待命按新设置重新裁决
    if (settings.autoStartOnApiCall) {
      this.watchPaused = false;
      this.watchPauseNotified = false;
    }
    this.evaluateWatcher();
  }

  /** 待命裁决唯一入口：装载完成后的首次裁决、进程启停、设置变更、探测变化都经此收敛。 */
  evaluateWatcher(): void {
    if (this.disposed) return;
    if (this.watchDesired()) {
      // 先挂起探测再起占位进程：否则探测会打进占位监听、把自己触发成启动
      this.runtime.setSuspended(true);
      void this.getPlatform()
        .then((platform) => this.watcher.arm(platform, this.settings, this.watchHooks))
        .then((result) => {
          if (!result.ok) {
            void this.classifyArmFailure(result.error ?? "待命进程未能启动");
            return;
          }
          if (this.disposed || !this.watcher.isArmed() || this.snap.watch) return;
          // 排队落地期间状态可能已变：按当前状态重新裁决
          if (!this.watchDesired()) {
            this.evaluateWatcher();
            return;
          }
          this.emit({ watch: true });
          this.logStatus(`自动启动待命中：监听 ${this.settings.host}:${this.settings.port}，外部调用将自动启动`);
        })
        .catch((err: unknown) => void this.classifyArmFailure(describe(err)));
    } else {
      // 先杀占位进程再恢复探测，顺序反了恢复的首个探测会把自己触发成启动
      void this.watcher.disarm().then((wasArmed) => {
        this.runtime.setSuspended(false);
        if (wasArmed) {
          this.emit({ watch: false });
          this.logStatus("已退出自动启动待命");
        }
      });
    }
  }

  /** 应待命 = 功能开 && 服务未运行且未在启动 && 未暂缓 && 探测未连接。 */
  private watchDesired(): boolean {
    return (
      this.settings.autoStartOnApiCall &&
      !this.handle &&
      !this.snap.starting &&
      !this.watchPaused &&
      !this.runtime.isOnline()
    );
  }

  /** 自动启动暂缓：失败原因持久（缺模型、端口被占等），重试无意义；手动启动或设置变更时恢复。 */
  private pauseAutoStart(reason: string): void {
    if (this.disposed) return;
    this.watchPaused = true;
    this.runtime.setSuspended(false);
    if (this.snap.watch) this.emit({ watch: false });
    this.logStatus(`自动启动已暂缓：${reason}`);
    if (this.watchPauseNotified) return;
    this.watchPauseNotified = true;
    try {
      this.ctx.notification.notify({
        level: "warning",
        title: "外部调用自动启动已暂缓",
        message: `${reason}。手动启动服务或修改设置后会恢复待命。`,
      });
    } catch {
      // 通知通道异常不改变暂缓结论
    }
  }

  /** 抢端口/待命失败的分类：对手是自家兄弟实例的占位监听 → 跟随；否则按失败暂缓。
   *  分类只能用握手 ping——正式端点探测若打中兄弟占位监听，会把它触发成启动。 */
  private async classifyArmFailure(reason: string): Promise<void> {
    if (this.disposed) return;
    if (await this.runtime.isSiblingWatchHeld()) {
      this.enterFollower();
      return;
    }
    this.pauseAutoStart(reason);
  }

  /** 跟随模式：保持探测挂起（绝不发正式请求，防止触发兄弟实例），定时握手等兄弟退场后接管。
   *  多窗口各自装配一份插件实例是宿主常态，跟随即静默共存，不弹通知。 */
  private enterFollower(): void {
    if (this.followerTimer !== null) return;
    this.logStatus("端口由其他窗口的待命持有，本窗口跟随");
    this.followerTimer = window.setInterval(() => void this.followerTick(), FOLLOWER_TICK_MS);
  }

  private async followerTick(): Promise<void> {
    if (this.disposed) {
      // 卸载可能发生在握手 await 期间、晚于 dispose 的清理：这里兜底摘掉定时器
      this.exitFollower();
      return;
    }
    if (this.followerTimer === null) return;
    if (!this.watchDesired()) {
      // 功能已关/服务在跑/手动启动等：跟随的前提消失，回归常规状态
      this.exitFollower();
      this.runtime.setSuspended(false);
      return;
    }
    if (await this.runtime.isSiblingWatchHeld()) return;
    // 兄弟占位监听已消失：端口空出或真服务上线，恢复探测后重新裁决
    this.exitFollower();
    this.runtime.setSuspended(false);
    this.evaluateWatcher();
  }

  private exitFollower(): void {
    if (this.followerTimer === null) return;
    window.clearInterval(this.followerTimer);
    this.followerTimer = null;
  }

  /** 外部调用触发：提示后复用普通启动流程。调用方 UA 只落日志（通知保持一行说清）。 */
  private async autoStart(requestLine: string, userAgent: string): Promise<void> {
    const detail = requestLine ? `（${requestLine}）` : "";
    this.logStatus(`检测到外部 API 调用${detail}，正在启动 llama-server…`);
    if (userAgent) this.logStatus(`调用方标识（User-Agent）：${userAgent}`);
    try {
      this.ctx.notification.notify({
        level: "info",
        title: "llama.cpp",
        message: `检测到外部 API 调用${detail}，正在启动 llama-server…`,
      });
    } catch {
      // 通知失败不影响启动
    }
    await this.start(this.runtime);
  }

  private emit(patch: Partial<HostSnapshot>): void {
    this.snap = { ...this.snap, ...patch };
    for (const listener of this.listeners) listener();
  }

  /** 追加并裁剪到上限；logSeq 累计计数，裁剪后仍能驱动界面的自动滚动。 */
  private log(line: string, stream: "stdout" | "stderr" = "stdout"): void {
    const prefix = stream === "stderr" ? "! " : "";
    this.logSeq += 1;
    const next = [...this.snap.logs, `${prefix}${line}`];
    this.emit({
      logs: next.length > MAX_LOGS ? next.slice(next.length - MAX_LOGS) : next,
      logSeq: this.logSeq,
    });
  }

  /** 插件自身状态行带时刻前缀：进程输出自带 llama-server 时间戳，插件行没有，排障对时需要。 */
  private logStatus(line: string, stream: "stdout" | "stderr" = "stdout"): void {
    const now = new Date();
    const pad = (n: number): string => String(n).padStart(2, "0");
    this.log(`[${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}] ${line}`, stream);
  }

  clearLogs(): void {
    this.emit({ logs: [] });
  }

  /** 就绪判定用「接口通」而不是「日志出现某句话」：接口可用才是真的可用。 */
  private async waitReady(runtime: LlamaRuntime): Promise<boolean> {
    const started = Date.now();
    let lastHeartbeat = 0;
    for (;;) {
      // 进程已退出就等不到服务，立刻定论——由调用方按 died 给出失败原因
      if (this.died) return false;
      const channel = await runtime.probe();
      if (channel === "direct") return true;
      if (this.died) return false;
      const elapsed = Date.now() - started;
      if (elapsed >= READY_TIMEOUT_MS) return false;
      // 加载模型会耗时数十秒，定期留个心跳，让用户看到还在等
      if (elapsed - lastHeartbeat >= 5000) {
        lastHeartbeat = elapsed;
        this.logStatus(`等待服务就绪…（${Math.round(elapsed / 1000)}s）`);
      }
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
  }

  /** 启动失败弹一条宿主通知：面板不可见时也能知道结果与原因。 */
  private notifyFailure(message: string): void {
    if (this.notified) return;
    try {
      this.ctx.notification.notify({
        level: "error",
        title: "llama-server 启动失败",
        message,
      });
    } catch {
      // 通知通道异常不改变「启动失败」这个结论
    }
  }

  /** 启动失败收口：写原因并弹通知。主动停止同样以进程退出收场，但那不算失败。 */
  private failStart(message?: string): void {
    if (this.cancelled) {
      this.emit({ starting: false });
      return;
    }
    const msg = message || this.died || `启动后 ${Math.round(READY_TIMEOUT_MS / 1000)}s 内服务未就绪`;
    this.emit({ starting: false, error: msg });
    this.notifyFailure(msg);
    // 启动失败后暂缓自动启动：原因持久，重试无意义
    this.watchPaused = true;
  }

  /** 启动并等待服务就绪。返回 false 有两种含义——已有进程/正在启动、启动失败：后者写 error，前者不写。 */
  async start(runtime: LlamaRuntime): Promise<boolean> {
    if (this.snap.starting || this.handle) return false;
    // starting 先行同步置位：手动启动与自动触发并发时，第二个 start 会被这个挡住
    this.emit({ starting: true });
    this.watchPaused = false;
    this.watchPauseNotified = false;
    // 占位监听持有端口时 llama-server 绑不上，启动前必须先让它退出
    if (await this.watcher.disarm()) this.emit({ watch: false });
    this.runtime.setSuspended(false);
    this.died = "";
    this.lastStderr = "";
    this.notified = false;
    this.cancelled = false;
    let line: string;
    let exe: string;
    let platform: Platform;
    try {
      platform = await this.getPlatform();
      const found = await findServerExecutable(this.ctx, platform, this.settings.serverDir);
      // 用户选的是文件夹，可执行文件在这一步才定位：找不到就以可操作的原因收场
      if (!found) throw new Error(missingExecutableMessage(this.settings.serverDir, platform));
      exe = found;
      line = describeStartCommand(this.settings, exe);
    } catch (err) {
      this.failStart(describe(err));
      this.evaluateWatcher();
      return false;
    }
    // 定位可执行文件的窗口里用户叫停（stop 拿不到句柄）：不再拉起进程
    if (this.cancelled) {
      this.emit({ starting: false });
      this.evaluateWatcher();
      return false;
    }

    this.emit({ error: "", startLine: line, logs: [] });

    try {
      const handle = await startLlama(
        this.ctx,
        platform,
        this.settings,
        exe,
        {
          onLog: (text, stream) => {
            if (stream === "stderr" && text.trim()) this.lastStderr = text.trim();
            this.log(text, stream);
          },
          onExit: (code) => {
            // 无论就绪前后退出，对界面而言都是「进程不在了」；starting 归 start() 收口
            this.handle = null;
            const detail = this.lastStderr
              ? `：${truncate(this.lastStderr, ERROR_DETAIL_MAX)}`
              : "";
            this.died = `进程已退出（退出码 ${code ?? "未知"}）${detail}`;
            this.emit({ running: false });
            this.logStatus(`进程已退出（退出码 ${code ?? "未知"}）`, "stderr");
            // 进程退出后重新裁决，待命自动恢复
            this.evaluateWatcher();
          },
          onError: (message) => {
            this.handle = null;
            this.died = message;
            // 启动阶段由 start() 统一收口；就绪后出错没有这一步，直接落进快照
            if (this.snap.starting) this.emit({ running: false });
            else this.emit({ running: false, error: message });
            this.log(message, "stderr");
            this.evaluateWatcher();
          },
        },
      );
      // 句柄落地前进程就已退出：onExit 已记下原因，这里不能再把句柄写回去
      if (this.died) {
        this.failStart();
        return false;
      }
      this.handle = handle;
      this.emit({ running: true });
    } catch (err) {
      this.failStart(describe(err));
      return false;
    }

    const ready = await this.waitReady(runtime);
    if (ready) this.emit({ starting: false });
    else this.failStart();
    this.evaluateWatcher();
    return ready;
  }

  /** 停止本插件启动的进程（结束整棵进程树，不给服务留孤儿）。待命不随停止解除：进程退出后自动重新待命。 */
  async stop(): Promise<void> {
    // 等就绪期间用户叫停：别让 start() 把这次退出当成启动失败
    if (this.snap.starting) this.cancelled = true;
    const handle = this.handle;
    if (!handle) {
      this.emit({ running: false, starting: false });
      return;
    }
    try {
      await handle.cancel();
      this.logStatus("已结束 llama-server 进程");
    } catch (err) {
      this.emit({ error: describe(err) });
    } finally {
      this.handle = null;
      this.emit({ running: false, starting: false });
    }
  }

  /** 插件停用/卸载：解除待命与跟随并忽略后续触发；进程收尾归宿主。 */
  dispose(): void {
    this.disposed = true;
    this.exitFollower();
    void this.watcher.disarm();
  }
}

function describe(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text;
}
