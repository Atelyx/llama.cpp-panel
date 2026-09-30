import type { Context } from "@atelyx/cordis";

/**
 * 插件上下文（`ctx`）的最小可用类型面。
 *
 * 只声明本插件实际用到的服务与方法；Atelyx 在加载时按自己的实现注入，
 * 这里的类型仅为本地 `tsc --noEmit` 提供约束。
 * 依赖声明见 `index.tsx`——这里用到的平台服务在插件运行时恒在。
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
  };
  shell: {
    /** 启动长驻进程并立即拿到句柄（不等进程结束）。 */
    spawn(
      opts: { command: string; args?: string[]; cwd?: string; env?: Record<string, string> },
      handlers?: ShellStreamHandlers,
    ): Promise<ShellProcessHandle>;
  };
  dialog: {
    pickDirectory(): Promise<string | null>;
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
