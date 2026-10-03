import type { Context } from "@atelyx/cordis";

/**
 * 宿主上下文（`ctx`）的类型面：只声明本插件用到的服务与方法，供本地 `tsc` 校验；
 * 运行时由 Atelyx 注入，所需服务在插件运行时恒在。
 */

/** 进程输出回调（`ctx.shell.spawn` 传 handlers 时启用）。 */
export interface ShellStreamHandlers {
  chunk(data: { stream: "stdout" | "stderr"; data: string }): void;
  end(data: { code: number | null }): void;
  error(message: string): void;
}

/** `ctx.shell.spawn` 的进程句柄：`cancel` 结束该进程及其全部子孙（含包装层）。 */
export interface ShellProcessHandle {
  pid: number;
  cancel(): Promise<void>;
}

/** `ctx.shell.exec` 的聚合结果（非流式：等进程结束一次性给出）。 */
export interface ShellExecResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

/** 单层目录条目（对应宿主 `external_list_dir`，目录在前、按名升序、含隐藏项）。 */
export interface ListDirEntry {
  name: string;
  kind: "dir" | "file";
  /** 文件字节大小（仅文件有）。 */
  size?: number;
  /** 目录直接子项数（仅目录有）。 */
  children?: number;
}

/** 单层目录列表结果（内联最多上限条，超限提示收窄）。 */
export interface ListDirResult {
  entries: ListDirEntry[];
  total: number;
  capped: boolean;
}

/**
 * 默认布局规格节点：只描述结构与视图 kind，面板/标签 id 由宿主实例化时生成。
 * `split` = 分割方向 + 子树 + 各子树占比（和 > 0）；`panel` = 一个停靠位的视图 kind 列表（首个为激活标签）。
 */
export type PluginLayoutSpecNode =
  | { kind: "split"; direction: "horizontal" | "vertical"; children: PluginLayoutSpecNode[]; sizes: number[] }
  | { kind: "panel"; views: string[] };

/** 默认布局声明载荷（`ctx.layout.declareDefaultLayout`）；宿主追加为布局列表新条目。 */
export interface PluginDefaultLayoutSpec {
  /** 布局名（非空，≤ 64 字节；与既有布局重名时宿主追加序号）。 */
  name: string;
  tree: PluginLayoutSpecNode;
}

export interface AtelyxCtx extends Context {
  /** 宿主平台信息（平台决定启动命令写法）。 */
  app: {
    platform(): Promise<string>;
  };
  /** 插件设置存储（整表落插件目录；值须 JSON 可序列化）。 */
  state: {
    read(): Promise<unknown>;
    write(data: unknown): Promise<void>;
  };
  /** 仓库外文件面：任意绝对路径（无目录授权门槛，调用由宿主按插件审计）。 */
  fs: {
    listDir(path: string): Promise<ListDirResult>;
    /** 写文本文件（父目录不存在时宿主会补建）。 */
    writeFile(path: string, content: string): Promise<{ ok: boolean; summary: string }>;
    /** 同目录重命名（newName 须为纯文件名；目标已存在会被宿主拒绝）。 */
    renameFile(path: string, newName: string): Promise<{ ok: boolean; summary: string; actualPath: string }>;
    /** 删除单个文件。 */
    deleteFile(path: string): Promise<{ ok: boolean; summary: string }>;
    /** 本插件私有目录绝对路径（不存在则创建；卸载保留配置时随插件数据保留、彻底卸载清除；更新保留）。 */
    privateDir(): Promise<string>;
  };
  shell: {
    /** 非流式执行：等进程结束，聚合 stdout/stderr 与退出码（短任务取数用）。 */
    exec(opts: { command: string; args?: string[]; cwd?: string; env?: Record<string, string> }): Promise<ShellExecResult | undefined>;
    /** 启动长驻进程并立即拿到句柄（不等进程结束）。 */
    spawn(
      opts: { command: string; args?: string[]; cwd?: string; env?: Record<string, string> },
      handlers?: ShellStreamHandlers,
    ): Promise<ShellProcessHandle>;
  };
  dialog: {
    pickDirectory(): Promise<string | null>;
  };
  /** 工作区布局：声明本插件默认布局（每插件一次性；归属 id 由宿主按调用方插件绑定）。 */
  layout: {
    declareDefaultLayout(spec: PluginDefaultLayoutSpec): () => void;
  };
  /** Markdown 渲染（宿主编辑器同一内核，恒可用）：输出已清洗 HTML，可直接挂入插件 UI。 */
  markdown: {
    renderHtml(markdown: string, options?: { katex?: boolean }): string;
  };
  notification: {
    notify(input: {
      message: string;
      level?: "info" | "success" | "warning" | "error";
      title?: string;
    }): string;
  };
  slots: {
    registerView(opts: {
      kind: string;
      label: string;
      component: (() => unknown) | unknown;
    }): () => void;
    registerSetting(opts: {
      key: string;
      label: string;
      component: (() => unknown) | unknown;
    }): () => void;
    registerCommand(opts: {
      id: string;
      label: string;
      run: () => unknown;
    }): () => void;
  };
  effect(fn: () => void | (() => void)): void;
}
