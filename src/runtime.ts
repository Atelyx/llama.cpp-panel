/**
 * 连接状态中心：HTTP 探测本机 llama-server，判定 direct/offline。
 * 只关心「服务在不在」，故无长连接；轮询保持状态新鲜，探测失败一律落 offline 而不抛。
 * 宿主每窗口各装配一份插件实例，探测前须先握手：端口被自家占位监听（兄弟实例待命）持有时
 * 不打正式端点——占位监听对非触发请求只会关闭连接，打了也只是白费流量的噪音。
 */
import { baseUrl, type LlamaSettings } from "./settings";

export type Channel = "direct" | "offline";

export interface RuntimeSnapshot {
  channel: Channel;
}

/** 握手端点与标记：占位监听对该端点白名单放行并回标记（响应须带 CORS 头，webview fetch 才读得到）。 */
export const WATCH_PING_PATH = "/__llama-panel-watch-ping";
export const WATCH_PING_MARKER = "llama-panel-watch-ok";

/** 探测端点按版本兼容顺序：/api/models → /api/version → /（Web UI）。 */
const PROBE_PATHS = ["/api/models", "/api/version", "/"];

/** 单次探测超时：本机服务，慢于此即视作不可用。 */
const PROBE_TIMEOUT_MS = 3000;

/** 握手超时：占位监听应答是毫秒级纯内存操作，慢即视为不是自家监听。 */
const PING_TIMEOUT_MS = 1500;

/** 插件活跃期轮询间隔：状态新鲜度与本机服务之间取平衡。 */
const POLL_INTERVAL_MS = 5000;

/** 握手：端口持有者应答约定标记 = 自家占位监听在待命；其余（未监听、真服务的 404）一律 false。 */
async function watchPingOk(base: string): Promise<boolean> {
  try {
    const res = await fetchWithTimeout(`${base}${WATCH_PING_PATH}`, { method: "GET" }, PING_TIMEOUT_MS);
    return res.status === 200 && (await res.text()) === WATCH_PING_MARKER;
  } catch {
    // 连接拒绝/超时/CORS 拒读都算「不是自家监听」
    return false;
  }
}

export class LlamaRuntime {
  private settings: LlamaSettings;
  private channel: Channel = "offline";
  // useSyncExternalStore 靠引用相等判定快照是否变化：getSnapshot 必须返回缓存对象，
  // 每次调用都新建对象会被判成「状态变了」而无限重渲
  private snapshot: RuntimeSnapshot = { channel: "offline" };
  private readonly listeners = new Set<() => void>();
  private timer: number | null = null;
  /** 挂起时端口由占位监听持有，任何探测都会把自己触发成启动：轮询与探测全部短路，在途探测一并中止。 */
  private suspended = false;
  /** 在途探测的控制器：挂起时逐个 abort，否则启动期在途的探测会打进刚落位的占位监听、自己触发自己。 */
  private readonly probing = new Set<AbortController>();

  constructor(settings: LlamaSettings) {
    this.settings = settings;
  }

  getSnapshot = (): RuntimeSnapshot => this.snapshot;

  isOnline = (): boolean => this.channel === "direct";

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  applySettings(settings: LlamaSettings): void {
    this.settings = settings;
    if (!this.suspended) void this.probeOnce();
  }

  /** 端口持有者是否为自家占位监听（兄弟实例待命中）。待命裁决抢端口失败后据此分类，见 controller。 */
  isSiblingWatchHeld = async (): Promise<boolean> => watchPingOk(baseUrl(this.settings));

  /** 挂起/恢复：挂起即停轮询、探测短路；恢复时重启轮询并立即探测。 */
  setSuspended(suspended: boolean): void {
    if (suspended === this.suspended) return;
    this.suspended = suspended;
    if (suspended) {
      this.stopPolling();
      for (const controller of this.probing) controller.abort();
      this.probing.clear();
    } else {
      this.startPolling();
      void this.probe();
    }
  }

  /** 探测并更新状态。可重复调用（轮询/手动重检都走它）。 */
  async probe(): Promise<Channel> {
    const next = await this.probeOnce();
    this.emit(next);
    return next;
  }

  /** 启动轮询（插件装载后调用）；返回时已启动。 */
  startPolling(): void {
    this.stopPolling();
    this.timer = window.setInterval(() => {
      void this.probe();
    }, POLL_INTERVAL_MS);
  }

  /** 停止轮询（插件停用前调用）。 */
  stopPolling(): void {
    if (this.timer !== null) {
      window.clearInterval(this.timer);
      this.timer = null;
    }
  }

  private emit(next: Channel): void {
    if (next === this.channel) return;
    this.channel = next;
    this.snapshot = { channel: next };
    for (const listener of this.listeners) listener();
  }

  private async probeOnce(): Promise<Channel> {
    if (this.suspended) return this.channel;
    const base = baseUrl(this.settings);
    // 握手先行：自家占位监听持有端口时（兄弟实例待命），正式端点探测只会吃到关闭的连接，纯噪音
    if (await watchPingOk(base)) return this.channel;
    const controller = new AbortController();
    this.probing.add(controller);
    try {
      for (const path of PROBE_PATHS) {
        // abort 只能掐断在途请求，拦不住循环把下一个端点再发出去；
        // 已中止的探测不得发起任何新请求：挂起即视为待命期间，对占位端口发请求纯属噪音
        if (controller.signal.aborted) break;
        try {
          const res = await fetchWithTimeout(`${base}${path}`, { method: "GET" }, PROBE_TIMEOUT_MS, controller.signal);
          if (res.status >= 200 && res.status < 300) return "direct";
        } catch {
          // 单个端点探测失败（超时/跨域/拒绝）试下一个；全失败才判 offline
        }
      }
    } finally {
      this.probing.delete(controller);
    }
    // 被挂起中止的探测不给结论：状态以挂起前为准，也不驱动界面变化
    if (controller.signal.aborted) return this.channel;
    return "offline";
  }
}

/** 带超时的 fetch：超时走内部 controller；外部信号（挂起）中止时一并取消，不影响后续端点各自独立计时。
 *  外部信号已中止时直接拒绝：对已中止信号挂 abort 监听不会再触发，不挡住就等于请求照发。 */
function fetchWithTimeout(
  url: string,
  init: RequestInit,
  timeoutMs: number,
  external?: AbortSignal,
): Promise<Response> {
  if (external?.aborted) return Promise.reject(new DOMException("aborted", "AbortError"));
  return new Promise((resolve, reject) => {
    const controller = new AbortController();
    const timer = window.setTimeout(() => controller.abort(), timeoutMs);
    const onExternal = (): void => controller.abort();
    external?.addEventListener("abort", onExternal);
    const cleanup = (): void => {
      window.clearTimeout(timer);
      external?.removeEventListener("abort", onExternal);
    };
    fetch(url, { ...init, signal: controller.signal })
      .then((res) => {
        cleanup();
        resolve(res);
      })
      .catch((err: unknown) => {
        cleanup();
        reject(err);
      });
  });
}
