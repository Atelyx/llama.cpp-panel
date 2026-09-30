/**
 * 模型目录扫描：从 modelsDir 递归列出，收集 *.gguf（忽略大小写）。
 *
 * 模型目录为用户自选的本地路径；根目录读不了返回
 * ok:false 附可读原因，子目录读不了（权限等）跳过不断整体。多文件夹分类的模型库
 * （如 vision/、text/ 子目录）靠递归覆盖；条目名是相对 modelsDir 的路径（/ 分隔）。
 * 递归有三重上限（深度/目录数/条目数）：模型库再大也不失控，触顶标 capped 提示收窄。
 * 文件名以 mmproj 开头的是视觉投影（llama-server 的 --mmproj），与普通模型分开返回。
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

export async function listModels(ctx: AtelyxCtx, modelsDir: string): Promise<ListModelsResult> {
  const root = modelsDir.trim().replace(/\\+/g, "/").replace(/\/+$/, "");
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

/** 相对路径 → 所在子目录（无目录部分返回空串；反斜杠归一为 /）。 */
export function dirNameOf(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx === -1 ? "" : norm.slice(0, idx);
}

/** modelsDir + 相对路径 → 模型全路径（用 / 连接，llama-server 与 cmd.exe 均接受）。 */
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
