/**
 * 模型下载：单任务状态机（一次一个文件），只负责发命令、看进度、收尾改名。
 *
 * 落盘到 `模型目录/发布者/模型名/`，同仓库的量化与投影同处一个文件夹，便于按文件夹配上 mmproj；
 * 目录不存在时由 curl 的 `create-dirs` 补建。进度取 `.part` 的实测字节数——不解析 curl 进度条，
 * 其格式随平台变、经宿主按行切分会碎。下完才改名 `.gguf`，模型扫描因此看不到半截文件；
 * 取消只结束 curl 进程并保留 `.part`，再开始时用 `-C -` 续传。
 */
import type { AtelyxCtx, ListDirEntry, ShellProcessHandle } from "../ctx";
import { describeCurlExit, spawnCurl } from "./curl";
import { fetchRemoteInfo } from "./hf";
import { downloadUrl, mirrorById, parseTarget, uniqueName } from "./mirrors";
import { describeFsError, invalidateModelCache } from "./models";
import { fileNameOf, formatSize, type DownloadTask } from "./types";

/** 进度采样间隔：够跟手，又不至于把宿主 fs 调用打满。 */
const TICK_MS = 600;

/** 下载请求（界面收集的用户输入）。 */
export interface DownloadRequest {
  /** 用户填写的下载地址或 owner/repo/文件名。 */
  input: string;
  /** 模型目录根；落盘为 `根/发布者/模型名/文件名`。 */
  dir: string;
  /** 镜像站 id。 */
  mirrorId: string;
}

export type PrepareResult =
  | { ok: true; task: DownloadTask }
  | { ok: false; error: string; existingPath?: string };

function idleTask(): DownloadTask {
  return {
    status: "idle",
    repo: "",
    file: "",
    target: "",
    url: "",
    totalBytes: null,
    bytes: 0,
    statusText: "",
  };
}

export class Downloader {
  private readonly ctx: AtelyxCtx;
  private task: DownloadTask = idleTask();
  private handle: ShellProcessHandle | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private partPath = "";
  private finalPath = "";
  /**
   * 当前下载进程的世代号。进程退出回调与 `cancel()` 是两条独立的异步路径，
   * 取消时先「废掉」世代号，旧进程的结束回调就不会再把状态改回失败——取消后的
   * 状态只由 `cancel()` 自己落定。
   */
  private runToken = 0;
  private readonly listeners = new Set<() => void>();

  constructor(ctx: AtelyxCtx) {
    this.ctx = ctx;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  getSnapshot = (): DownloadTask => this.task;

  /** 结束进程与轮询；供插件停用收尾。不删 `.part`。 */
  dispose(): void {
    // 置停用标志：start() 还在 await 时不能再落地新进程与轮询
    this.runToken += 1;
    if (this.handle) void this.handle.cancel().catch(() => undefined);
    this.clearTimer();
    this.handle = null;
  }

  private notify(next: DownloadTask): void {
    this.task = next;
    for (const listener of this.listeners) listener();
  }

  private clearTimer(): void {
    if (this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  /**
   * 预检：解析地址、确认落盘名与远端体积。该模型文件夹里已有同名文件即判定为已下载并拒绝
   * （重下几个 GB 不可接受）；界面要保留下载时传 `rename`，得到带序号的新名字。
   */
  async prepare(req: DownloadRequest, mode: "skip" | "rename" = "skip"): Promise<PrepareResult> {
    const parsed = parseTarget(req.input);
    if (!parsed.ok) return { ok: false, error: parsed.error };
    const folder = req.dir.trim().replace(/\\+/g, "/").replace(/\/+$/, "");
    if (!folder) return { ok: false, error: "未设置模型目录：先到设置页选择模型目录" };
    try {
      // 模型根目录必须先存在：设置写错时给明确原因，而下面的仓库子目录允许还没建
      await this.ctx.fs.listDir(folder);
    } catch (err) {
      return { ok: false, error: describeFsError(err, "模型目录") };
    }

    const repoDir = `${folder}/${parsed.target.repo}`;
    const files = await this.filesIn(repoDir);
    // 仓库里的文件路径可能带子目录，落盘压平成单段名（与仓库文件夹同层摆放）
    const wanted = fileNameOf(parsed.target.file);
    const existing = files.find((entry) => entry.name.toLowerCase() === wanted.toLowerCase());
    if (existing && mode === "skip") {
      return {
        ok: false,
        error: `模型目录里已有 ${wanted}（${repoDir}/${existing.name}），无需重复下载`,
        existingPath: `${repoDir}/${existing.name}`,
      };
    }

    // 体积只用于显示进度百分比：探测不到（镜像不支持 HEAD、网络抖动）也照下
    const url = downloadUrl(mirrorById(req.mirrorId), parsed.target.repo, parsed.target.file);
    const totalBytes = await fetchRemoteInfo(this.ctx, url);

    const finalName = existing ? uniqueName(new Set(files.map((entry) => entry.name.toLowerCase())), wanted) : wanted;
    // 上次中断留下的临时文件：预检就把已下字节数带出来，续传时进度从实际位置起算
    const bytes = files.find((entry) => entry.name === `${finalName}.part`)?.size ?? 0;
    return {
      ok: true,
      task: {
        status: "idle",
        repo: parsed.target.repo,
        file: finalName,
        target: `${repoDir}/${finalName}`,
        url,
        totalBytes,
        bytes,
        statusText: "",
      },
    };
  }

  /** 开始下载预检过的任务；进行中重复调用 = 无操作。 */
  async start(prepared: DownloadTask): Promise<void> {
    // 状态守卫拦住「已在下载」的重复调用；世代号在第一个 await 之前自增，拦住并发进来的
    // 两次调用——否则两个 curl 会往同一个 `.part` 上追加，文件损坏且前一个进程失去取消入口。
    if (this.task.status === "downloading") return;
    this.clearTimer();
    const token = ++this.runToken;
    const slash = prepared.target.lastIndexOf("/");
    const dir = prepared.target.slice(0, slash);
    const part = `${prepared.target}.part`;
    const partName = `${prepared.file}.part`;
    this.partPath = part;
    this.finalPath = prepared.target;

    const partSize = await this.sizeOf(dir, partName);
    // 读取期间被取消、或已被后一次 start 取代：本次不再落地状态与进程
    if (this.runToken !== token) return;
    // 临时文件已达完整体积（上次下载完了但改名失败）：直接改名，重发 curl 只会得到 Range 错误
    if (partSize != null && prepared.totalBytes != null && partSize >= prepared.totalBytes) {
      this.notify({ ...prepared, status: "downloading", bytes: partSize, statusText: "文件已下载完整，正在落盘改名…" });
      await this.finish(token, 0, "");
      return;
    }

    this.notify({
      ...prepared,
      status: "downloading",
      bytes: partSize ?? 0,
      statusText: partSize && partSize > 0 ? "从断点继续…" : "启动下载…",
    });

    let stderrTail = "";
    let handle;
    try {
      // 断点续传交给 curl 的 continue-at：目标不存在时它从 0 开始，存在时接着下
      handle = await spawnCurl(
        this.ctx,
        { url: prepared.url, output: part, resume: true, retries: 3, createDirs: true, stall: { limit: 1024, seconds: 60 } },
        {
          onStderr: (line) => {
            stderrTail = (stderrTail + line).slice(-2000);
          },
          onExit: (code) => void this.finish(token, code, stderrTail),
          onError: (message) => void this.fail(token, message),
        },
      );
    } catch (err) {
      await this.fail(token, err instanceof Error ? err.message : String(err));
      return;
    }
    if (this.runToken !== token) {
      // 等待进程启动期间被取消/取代：刚起来的进程不该继续跑
      void handle.cancel().catch(() => undefined);
      return;
    }
    this.handle = handle;
    // 立即采样一次，避免第一帧进度为空
    void this.sample();
    this.timer = setInterval(() => void this.sample(), TICK_MS);
  }

  /** 读目录里某个文件的字节数（不在、读不到、或目录条目被宿主截断都返回 null）。 */
  private async sizeOf(dir: string, name: string): Promise<number | null> {
    try {
      const listing = await this.ctx.fs.listDir(dir);
      return listing.entries.find((entry) => entry.name === name)?.size ?? null;
    } catch {
      return null;
    }
  }

  /** 取消下载：结束 curl 进程，保留 `.part` 供续传。 */
  async cancel(): Promise<void> {
    if (this.task.status !== "downloading") return;
    this.clearTimer();
    // 先废世代号：下面 await 期间旧进程的结束回调可能先到，不该由它落状态
    this.runToken += 1;
    const handle = this.handle;
    this.handle = null;
    if (handle) {
      try {
        await handle.cancel();
      } catch {
        // 进程已退出即视为已取消
      }
    }
    this.notify({ ...this.task, status: "paused", statusText: "已暂停；再点下载会从断点继续" });
  }

  /** 清理已完成/失败/暂停的任务卡片。 */
  dismiss(): void {
    if (this.task.status === "downloading") return;
    this.clearTimer();
    this.notify(idleTask());
  }

  /** 列出目录里的普通文件；目录不存在或读不了都算空表（首次下载该仓库时目录还没建）。 */
  private async filesIn(dir: string): Promise<ListDirEntry[]> {
    try {
      const listing = await this.ctx.fs.listDir(dir);
      return listing.entries.filter((entry) => entry.kind === "file");
    } catch {
      return [];
    }
  }

  /** 轮询临时文件实测字节数（唯一进度来源）。 */
  private async sample(): Promise<void> {
    if (this.task.status !== "downloading") return;
    const slash = this.partPath.lastIndexOf("/");
    const size = await this.sizeOf(this.partPath.slice(0, slash), this.partPath.slice(slash + 1));
    if (size == null || size === this.task.bytes) return;
    this.notify({ ...this.task, bytes: size });
  }

  private async finish(token: number, code: number | null, stderr: string): Promise<void> {
    // 世代号不符 = 本次已被取消或被新的下载取代：不落任何状态
    if (token !== this.runToken) return;
    this.clearTimer();
    this.handle = null;
    if (code !== 0) {
      await this.fail(token, describeCurlExit(code, stderr));
      return;
    }
    const slash = this.finalPath.lastIndexOf("/");
    const finalName = this.finalPath.slice(slash + 1);
    try {
      await this.ctx.fs.renameFile(this.partPath, finalName);
    } catch (err) {
      await this.fail(
        token,
        `下载已完成，但改名失败：${describeFsError(err, "模型文件")}。` +
          `文件已完整保存在 ${this.partPath}，把它改名为 ${finalName} 即可使用。`,
      );
      return;
    }
    const size = this.task.totalBytes ?? this.task.bytes;
    this.notify({
      ...this.task,
      status: "done",
      bytes: size,
      statusText: `已保存到 ${this.finalPath}（${formatSize(size)}）`,
    });
    // 落了新模型：让扫描缓存失效，各页签重扫即可看到它
    invalidateModelCache();
  }

  private async fail(token: number, message: string): Promise<void> {
    if (token !== this.runToken) return; // 旧进程的错误不该盖到新任务上
    this.clearTimer();
    this.handle = null;
    this.notify({ ...this.task, status: "error", statusText: message });
  }
}
