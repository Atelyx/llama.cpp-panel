/**
 * 插件设置：`ctx.state` 的单对象状态。
 *
 * 缺字段一律用默认值补齐（新增设置项时老状态仍可用）；读取只做内存合并，
 * 写盘仅发生在用户改设置时——启用插件不该产生写盘。
 */
import type { AtelyxCtx } from "./ctx";

export interface LlamaSettings {
  /** llama-server 监听地址（默认本机）。 */
  host: string;
  /** 监听端口：既是连接探测目标，也是托管启动时传给 llama-server 的端口。 */
  port: number;
  /** llama-server 所在文件夹：可执行文件在里面，启动时按平台名查找。 */
  serverDir: string;
  /** 模型目录：扫描 *.gguf 的根目录（用户自选的本地路径）。 */
  modelsDir: string;
  /** 启动用的模型文件（全路径），由面板选中模型的量化选择写入。 */
  modelFile: string;
  /** 视觉投影文件（全路径，可选）；设置后启动时追加 --mmproj。 */
  mmprojFile: string;
  /** 外部调用自动启动：服务未运行时插件占位监听服务端口，检测到外部请求即自动启动。 */
  autoStartOnApiCall: boolean;
  /** 模型下载用的镜像站 id（见 host/mirrors 的 MIRRORS）。 */
  downloadMirror: string;
  /** 各模型文件夹的启动参数，key = 模型文件夹全路径（/ 分隔）。 */
  modelParams: Record<string, ModelLaunchParams>;
}

/** 单个模型文件夹的启动参数（表单值，随模型持久化）。 */
export interface ModelLaunchParams {
  /** 选中的量化文件名（相对模型文件夹）。 */
  quant: string;
  /** 选中的视觉投影文件名（相对；空 = 不挂投影）。 */
  mmproj: string;
  /** GPU 卸载层数（-ngl）；null = 不传该参数。 */
  ngl: number | null;
  /** 上下文长度（--ctx-size）；null = 不传。 */
  ctxSize: number | null;
  /** 生成线程数（--threads）；null = 不传。 */
  threads: number | null;
  /** 并行槽位数（--parallel）；null = 不传。 */
  parallel: number | null;
  /** KV 缓存 K 量化类型（--cache-type-k）；空 = 不传。 */
  cacheTypeK: string;
  /** Flash Attention（--flash-attn on）。 */
  flashAttn: boolean;
  /** Jinja 聊天模板（--jinja）。 */
  jinja: boolean;
  /** 隐藏内置 WebUI（--no-webui）。 */
  noWebui: boolean;
  /** 模型载入模式（--load-mode：auto/none/mmap/mlock/mmap+mlock/dio）；空 = 不传。 */
  loadMode: string;
  /** 其余附加参数，原样追加在末尾。 */
  extraArgs: string;
}

/** 状态文件形状（顶层带版本号）。 */
interface PluginState {
  version: number;
  settings: LlamaSettings;
}

const STATE_VERSION = 1;

/**
 * 默认镜像站 id。写死字符串而不从 host/mirrors 导入：本模块是 settings 层，
 * 不该依赖 host 层；取值须与 MIRRORS 的首项一致（镜像表里改 id 要同步这里）。
 */
const DEFAULT_MIRROR = "hf-mirror";

export const DEFAULT_SETTINGS: LlamaSettings = {
  host: "127.0.0.1",
  port: 8080,
  serverDir: "",
  modelsDir: "",
  modelFile: "",
  mmprojFile: "",
  autoStartOnApiCall: false,
  downloadMirror: DEFAULT_MIRROR,
  modelParams: {},
};

/** 收敛模型参数表：缺字段补默认，key 统一为 / 分隔。 */
const KV_K_TYPES = new Set(["f16", "bf16", "q8_0", "q5_1", "q5_0", "q4_1", "q4_0"]);
const LOAD_MODES = new Set(["auto", "none", "mmap", "mlock", "mmap+mlock", "dio"]);

function coerceParams(raw: unknown): Record<string, ModelLaunchParams> {
  const out: Record<string, ModelLaunchParams> = {};
  if (!raw || typeof raw !== "object") return out;
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (!key || typeof value !== "object" || value === null) continue;
    const p = value as Record<string, unknown>;
    const int = (v: unknown, min: number): number | null =>
      typeof v === "number" && Number.isInteger(v) && v >= min ? v : null;
    out[key.replace(/\\/g, "/")] = {
      quant: typeof p.quant === "string" ? p.quant : "",
      mmproj: typeof p.mmproj === "string" ? p.mmproj : "",
      ngl: int(p.ngl, 0),
      ctxSize: int(p.ctxSize, 1),
      threads: int(p.threads, 1),
      parallel: int(p.parallel, 1),
      cacheTypeK: typeof p.cacheTypeK === "string" && KV_K_TYPES.has(p.cacheTypeK) ? p.cacheTypeK : "",
      flashAttn: p.flashAttn === true,
      jinja: p.jinja === true,
      noWebui: p.noWebui === true,
      loadMode: typeof p.loadMode === "string" && LOAD_MODES.has(p.loadMode) ? p.loadMode : "",
      extraArgs: typeof p.extraArgs === "string" ? p.extraArgs : "",
    };
  }
  return out;
}

/** 收敛磁盘值：手改或半写的脏数据不该把插件带偏。 */
function coerce(raw: unknown): LlamaSettings {
  const source = (raw ?? {}) as Record<string, unknown>;
  const str = (key: keyof LlamaSettings, fallback: string): string => {
    const value = source[key];
    return typeof value === "string" ? value : fallback;
  };
  const port = source.port;
  return {
    host: str("host", DEFAULT_SETTINGS.host).trim() || DEFAULT_SETTINGS.host,
    // 非法端口在此挡掉，后面拼地址的地方无需再判
    port:
      typeof port === "number" && Number.isInteger(port) && port > 0 && port < 65536
        ? port
        : DEFAULT_SETTINGS.port,
    serverDir: str("serverDir", DEFAULT_SETTINGS.serverDir).trim(),
    modelsDir: str("modelsDir", DEFAULT_SETTINGS.modelsDir).trim(),
    modelFile: str("modelFile", DEFAULT_SETTINGS.modelFile).trim(),
    mmprojFile: str("mmprojFile", DEFAULT_SETTINGS.mmprojFile).trim(),
    autoStartOnApiCall: source.autoStartOnApiCall === true,
    downloadMirror: str("downloadMirror", DEFAULT_SETTINGS.downloadMirror).trim() || DEFAULT_MIRROR,
    modelParams: coerceParams(source.modelParams),
  };
}

/** 状态缺失或损坏一律回落默认，不抛错。 */
export async function loadSettings(ctx: AtelyxCtx): Promise<LlamaSettings> {
  try {
    const raw = (await ctx.state.read()) as PluginState | null;
    return coerce(raw?.settings);
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

/** 整体覆盖写入。 */
export async function saveSettings(ctx: AtelyxCtx, settings: LlamaSettings): Promise<void> {
  const payload: PluginState = { version: STATE_VERSION, settings };
  await ctx.state.write(payload);
}

/** 拼 HTTP 基址（末尾不带斜杠）。监听 0.0.0.0（对所有网卡开放，局域网可调 API）时探测走本机回环。 */
export function baseUrl(settings: LlamaSettings): string {
  const probeHost = settings.host === "0.0.0.0" ? "127.0.0.1" : settings.host;
  return `http://${probeHost}:${settings.port}`;
}

/** 模型文件夹路径归一（反斜杠→/、去尾分隔符），作 modelParams 的 key。 */
export function normalizeDir(dir: string): string {
  return dir.trim().replace(/\\/g, "/").replace(/\/+$/, "");
}

/** 取模型文件夹的持久化启动参数；没有记录给空白默认。 */
export function paramsFor(settings: LlamaSettings, modelDir: string): ModelLaunchParams {
  return (
    settings.modelParams[normalizeDir(modelDir)] ?? {
      quant: "",
      mmproj: "",
      ngl: null,
      ctxSize: null,
      threads: null,
      parallel: null,
      cacheTypeK: "",
      flashAttn: false,
      jinja: false,
      noWebui: false,
      loadMode: "",
      extraArgs: "",
    }
  );
}
