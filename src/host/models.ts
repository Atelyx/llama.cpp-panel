/**
 * 模型目录扫描：从 modelsDir 递归收集 *.gguf（忽略大小写），条目名为相对 modelsDir 的路径。
 * 根目录读不了整体失败并附原因，子目录读不了跳过；深度/目录数/条目数三重上限，触顶标 capped。
 * 文件名含 mmproj 的识别为视觉投影（llama-server 的 --mmproj），与普通模型分开返回。
 */
import type { AtelyxCtx } from "../ctx";

export interface ModelEntry {
  /** 相对 modelsDir 的路径（/ 分隔，含子目录）。 */
  name: string;
  size?: number;
}

export type ListModelsResult =
  | { ok: true; models: ModelEntry[]; projectors: ModelEntry[]; capped: boolean }
  | { ok: false; error: string };

/** 递归深度上限：模型库按类别分层足够，再深基本是扫错目录。 */
const MAX_DEPTH = 6;
/** 目录访问数上限：挡符号链接环与异常深的树。 */
const MAX_DIRS = 300;
/** 收集的 gguf 条目上限：界面列表与启动选择的合理规模。 */
const MAX_ENTRIES = 600;

export async function listModels(ctx: AtelyxCtx, modelsDir: string): Promise<ListModelsResult> {  const root = modelsDir.trim().replace(/\\+/g, "/").replace(/\/+$/, "");
  // 未选择目录：不是错误，只是没东西可列
  if (!root) return { ok: true, models: [], projectors: [], capped: false };

  const models: ModelEntry[] = [];
  const projectors: ModelEntry[] = [];
  let dirs = 0;
  let entries = 0;
  let capped = false;

  const walk = async (abs: string, rel: string, depth: number): Promise<void> => {
    if (depth > MAX_DEPTH) {
      capped = true;
      return;
    }
    if (dirs >= MAX_DIRS || entries >= MAX_ENTRIES) {
      capped = true;
      return;
    }
    dirs += 1;
    let result;
    try {
      result = await ctx.fs.listDir(abs);
    } catch (err) {
      // 根目录失败 = 整体失败；子目录失败（权限/占用）跳过，不拖垮整次扫描
      if (rel === "") throw err;
      return;
    }
    if (result.capped) capped = true;
    for (const entry of result.entries) {
      if (entries >= MAX_ENTRIES) {
        capped = true;
        break;
      }
      const childRel = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.kind === "dir") {
        await walk(`${abs}/${entry.name}`, childRel, depth + 1);
        continue;
      }
      if (!/\.gguf$/i.test(entry.name)) continue;
      entries += 1;
      const item: ModelEntry = { name: childRel, size: entry.size };
      // mmproj 视觉投影两种社区命名都认：前缀（mmproj-F16.gguf）与中段
      // （Model-mmproj-Q8_0.gguf）；带分隔符的词边界防误伤含相似字样的普通模型
      if (/(^|[-_.\s])mmproj/i.test(entry.name)) projectors.push(item);
      else models.push(item);
    }
  };

  try {
    await walk(root, "", 0);
  } catch (err: unknown) {
    return { ok: false, error: describeFsError(err, "模型目录") };
  }
  const byPath = (a: ModelEntry, b: ModelEntry): number => a.name.localeCompare(b.name);
  models.sort(byPath);
  projectors.sort(byPath);
  return { ok: true, models, projectors, capped };
}

/** 带缓存的扫描：同目录只扫一次。宿主切换标签会卸载旧视图，没有缓存则每次切回都要重扫一遍。 */
const scanCache = new Map<string, ListModelsResult>();

export async function listModelsCached(
  ctx: AtelyxCtx,
  modelsDir: string,
  force = false,
): Promise<ListModelsResult> {
  const key = modelsDir.trim().replace(/\\+/g, "/").replace(/\/+$/, "");
  if (!key) return listModels(ctx, modelsDir);
  if (!force) {
    const hit = scanCache.get(key);
    if (hit) return hit;
  }
  const result = await listModels(ctx, modelsDir);
  // 只缓存成功结果：失败（目录不存在/无权限）留给下次重试，否则用户修好目录也刷不出来
  if (result.ok) scanCache.set(key, result);
  else scanCache.delete(key);
  return result;
}

/** 下载落盘后清空扫描缓存，让各页签重扫就能看到新模型（缓存的目录不只下载落点一处）。 */
export function invalidateModelCache(): void {
  scanCache.clear();
}

/** 相对路径 → 所在子目录（无目录部分返回空串；反斜杠归一为 /）。 */
export function dirNameOf(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx === -1 ? "" : norm.slice(0, idx);
}

/** 一个模型文件夹：同目录的量化文件与视觉投影聚在一起。 */
export interface ModelFolder {
  /** 相对 modelsDir 的目录路径（空串 = 根目录）。 */
  rel: string;
  /** 全路径（/ 分隔）。 */
  full: string;
  quants: ModelEntry[];
  mms: ModelEntry[];
}

/**
 * 扫描结果按文件夹聚合：文件夹 = 含至少一个非 mmproj 的 gguf；投影挂到同目录文件夹
 * （没有量化文件的目录不成为模型文件夹——孤零零的投影无处可挂）。
 */
export function groupByFolder(scan: { models: ModelEntry[]; projectors: ModelEntry[] }, modelsDir: string): ModelFolder[] {
  const map = new Map<string, { quants: ModelEntry[]; mms: ModelEntry[] }>();
  const bucket = (dir: string): { quants: ModelEntry[]; mms: ModelEntry[] } => {
    let b = map.get(dir);
    if (!b) {
      b = { quants: [], mms: [] };
      map.set(dir, b);
    }
    return b;
  };
  for (const m of scan.models) bucket(dirNameOf(m.name)).quants.push(m);
  for (const p of scan.projectors) {
    const dir = dirNameOf(p.name);
    if (map.has(dir)) bucket(dir).mms.push(p);
  }
  return [...map.entries()]
    .map(([rel, b]) => ({ rel, full: joinModelPath(modelsDir, rel), quants: b.quants, mms: b.mms }))
    .sort((a, b) => a.rel.localeCompare(b.rel));
}

/** modelsDir + 相对路径 → 模型全路径（用 / 连接，llama-server 接受）。 */
export function joinModelPath(modelsDir: string, name: string): string {
  const dir = modelsDir.trim().replace(/\/+$/, "");
  return dir ? `${dir}/${name}` : name;
}

/**
 * 宿主 fs 经 Tauri invoke 抛的是字符串而非 Error，两种形态都要透出，
 * 否则真实原因（不存在/无权限等）会被兜底文案吞掉。what = 被读的对象（模型目录等）。
 */
export function describeFsError(err: unknown, what: string): string {
  const reason = (typeof err === "string" ? err : err instanceof Error ? err.message : "").trim();
  return reason ? `读取${what}失败：${reason}` : `读取${what}失败（目录不存在或不是目录）`;
}
