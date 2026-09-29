/**
 * 连接状态中心：探测本机 llama-server 的 HTTP API，判定 direct/offline。
 *
 * llama-server 无队列/生成结果要轮询，只关心「服务在不在」——因此只需 HTTP 探测，
 * 不做长连接。轮询保持状态新鲜（服务崩了能感知），探测失败不抛断。
 */
import { baseUrl, type LlamaSettings } from "./settings";

export type Channel = "direct" | "offline";

export interface RuntimeSnapshot {
  channel: Channel;
}

/** 探测端点按版本兼容顺序：/api/models → /api/version → /（Web UI）。 */
const PROBE_PATHS = ["/api/models", "/api/version", "/"];

/** 单次探测超时：本机服务，慢于此即视作不可用。 */
const PROBE_TIMEOUT_MS = 3000;

/** 插件活跃期轮询间隔：状态新鲜度与本机服务之间取平衡。 */
const POLL_INTERVAL_MS = 5000;

export class LlamaRuntime {
  private settings: LlamaSettings;
  private channel: Channel = "offline";
  // useSyncExternalStore 靠引用相等判定快照是否变化：getSnapshot 必须返回缓存对象，
  // 每次调用都新建对象会被判成「状态变了」而无限重渲
  private snapshot: RuntimeSnapshot = { channel: "offline" };
  private readonly listeners = new Set<() => void>();
  private timer: number | null = null;

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
    void this.probeOnce();
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
    const base = baseUrl(this.settings);
    for (const path of PROBE_PATHS) {
      try {
        const res = await fetchWithTimeout(`${base}${path}`, { method: "GET" }, PROBE_TIMEOUT_MS);
        if (res.status >= 200 && res.status < 300) return "direct";
      } catch {
        // 单个端点探测失败（超时/跨域/拒绝）试下一个；全失败才判 offline
      }
    }
    return "offline";
  }
}

/** 带超时的 fetch：AbortController 取消未完成的请求。 */
function fetchWithTimeout(url: string, init: RequestInit, timeoutMs: number): Promise<Response> {
  return new Promise((resolve, reject) => {
    const controller = new AbortController();
    const timer = window.setTimeout(() => controller.abort(), timeoutMs);
    fetch(url, { ...init, signal: controller.signal })
      .then((res) => {
        window.clearTimeout(timer);
        resolve(res);
      })
      .catch((err: unknown) => {
        window.clearTimeout(timer);
        reject(err);
      });
  });
}
