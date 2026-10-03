/**
 * 插件入口：注册三个页签（模型库 / 已装模型 / 日志）、设置页与命令。
 *
 * runtime 与 host 在 apply 期间各建一次，页签只消费快照——页签拆到不同面板也共享同一份
 * 连接状态、进程状态与下载任务。进程收尾归 Atelyx，这里只解除待命、停轮询与下载，
 * 不 cancel 长期进程，以免误杀用户自起的外部服务。
 */
import React from "react";
import type { AtelyxCtx } from "./ctx";
import { loadSettings, saveSettings, type LlamaSettings } from "./settings";
import { LlamaRuntime } from "./runtime";
import { HostController } from "./host/controller";
import { Downloader } from "./host/downloader";
import { LibraryView } from "./views/Library";
import { InstalledView } from "./views/Installed";
import { LogsView } from "./views/Logs";
import { SettingsView } from "./views/Settings";
import { PANEL_CSS, bgPrimary, textMuted, fontUi } from "./views/ui";

/** 与清单里的 name 一致。 */
const PLUGIN_ID = "com.atelyx.llama.cpp-panel";

/** 页签标识：`kind` 是开放槽位，注册即出现在面板的「添加视图」里。 */
const VIEW_LIBRARY = `${PLUGIN_ID}.library`;
const VIEW_INSTALLED = `${PLUGIN_ID}.installed`;
const VIEW_LOGS = `${PLUGIN_ID}.logs`;

const SETTING_KEY = `${PLUGIN_ID}.settings`;

/** 就绪后视图依赖的一组服务与状态。 */
interface PanelDeps {
  ctx: AtelyxCtx;
  runtime: LlamaRuntime;
  host: HostController;
  downloader: Downloader;
  settings: LlamaSettings;
  onSettingsChanged: (next: LlamaSettings) => void;
}

export default function apply(pluginCtx: AtelyxCtx): void {
  // 设置要先读出来才能建 runtime（地址端口决定传输层）。读取是异步的，故设一个就绪后通知
  // 订阅者的小状态——未就绪时显示「初始化中」而不是闪空白。
  let deps: PanelDeps | null = null;
  const listeners = new Set<() => void>();
  const subscribe = (listener: () => void): (() => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };
  const getDeps = (): PanelDeps | null => deps;
  const notify = (): void => {
    for (const listener of listeners) listener();
  };

  // 设置变更收口：host 先裁决（待命路径会挂起探测），runtime 再更新——反了会自己触发自己；最后重渲并落盘。
  const onSettingsChanged = (next: LlamaSettings): void => {
    if (deps) {
      deps.host.applySettings(next);
      deps.runtime.applySettings(next);
      // 换一个新对象引用，getDeps 返回的快照才变化，useSyncExternalStore 才会重渲
      deps = { ...deps, settings: next };
    }
    void saveSettings(pluginCtx, next);
    notify();
  };

  void loadSettings(pluginCtx)
    .then((loaded) => {
      const runtime = new LlamaRuntime(loaded);
      const host = new HostController(pluginCtx, loaded, runtime);
      const downloader = new Downloader(pluginCtx);
      deps = { ctx: pluginCtx, runtime, host, downloader, settings: loaded, onSettingsChanged };

      // 常驻轮询保持连接状态新鲜
      runtime.startPolling();

      // 首次探测出结果后做一次待命裁决：加载期若服务本就未运行，探测结果无变化、
      // 不会产生状态事件，必须显式裁决一次才会进入待命
      void runtime.probe().then(() => host.evaluateWatcher());
      notify();
    })
    .catch((err: unknown) => {
      pluginCtx.notification.notify({
        level: "warning",
        message: `设置读取失败，已用默认值启动：${err instanceof Error ? err.message : String(err)}`,
      });
    });

  /**
   * 面板外壳：铺满可用空间（显式 width 防宿主包装层缩成内容宽）。共用的样式表也挂在这里——
   * 它只在该页签挂载时生效，而悬停底色与聚焦环是每个页签都要的，故放在共同外壳上。
   * 面板边框与底色由宿主容器给，插件不自绘（见宿主插件样式契约）。
   */
  function PanelShell(props: { children: unknown }): unknown {
    return (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          minHeight: 0,
          boxSizing: "border-box",
          background: bgPrimary,
        }}
      >
        <style>{PANEL_CSS}</style>
        {props.children}
      </div>
    );
  }

  function InitPlaceholder(): unknown {
    return (
      <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: textMuted, fontSize: fontUi }}>
        正在初始化…
      </div>
    );
  }

  /** 就绪依赖 → 视图的装配收在一处；页签自带就绪判定，先打开哪个都能等到依赖就绪。 */
  function panel(render: (ready: PanelDeps) => unknown): () => unknown {
    return function Panel(): unknown {
      const ready = React.useSyncExternalStore(subscribe, getDeps);
      if (!ready) return <InitPlaceholder />;
      return <PanelShell>{render(ready)}</PanelShell>;
    };
  }

  const LibraryPanel = panel((ready) => (
    <LibraryView ctx={ready.ctx} downloader={ready.downloader} settings={ready.settings} onSettingsChanged={ready.onSettingsChanged} />
  ));

  const InstalledPanel = panel((ready) => (
    <InstalledView ctx={ready.ctx} runtime={ready.runtime} host={ready.host} settings={ready.settings} onSettingsChanged={ready.onSettingsChanged} />
  ));

  const LogsPanel = panel((ready) => <LogsView host={ready.host} />);

  function SettingsPanelView(): unknown {
    const ready = React.useSyncExternalStore(subscribe, getDeps);
    if (!ready) return <InitPlaceholder />;
    return (
      <PanelShell>
        <SettingsView ctx={ready.ctx} settings={ready.settings} onSettingsChanged={ready.onSettingsChanged} />
      </PanelShell>
    );
  }

  pluginCtx.effect(() => {
    const views: Array<{ kind: string; label: string; component: () => unknown }> = [
      { kind: VIEW_LIBRARY, label: "llama.cpp：模型库", component: LibraryPanel },
      { kind: VIEW_INSTALLED, label: "llama.cpp：已装模型", component: InstalledPanel },
      { kind: VIEW_LOGS, label: "llama.cpp：日志", component: LogsPanel },
    ];
    const offViews = views.map((view) => pluginCtx.slots.registerView(view));
    const offSetting = pluginCtx.slots.registerSetting({
      key: SETTING_KEY,
      label: "llama.cpp",
      component: SettingsPanelView,
    });
    return () => {
      for (const off of offViews) off();
      offSetting();
      // 轮询、下载与自动启动待命随插件停用收尾；常驻服务进程由 Atelyx 统一结束，这里不 cancel
      deps?.host.dispose();
      deps?.runtime.stopPolling();
      deps?.downloader.dispose();
    };
  });

  // 默认布局：左列上下叠放「已装模型 / 日志」，右列「模型库」。宿主一次性追加为布局列表新条目
  // （不激活、不改用户既有布局），用户已摆放任一本插件视图时跳过——判重由宿主承担，这里无需兜底。
  pluginCtx.layout.declareDefaultLayout({
    name: "llama.cpp",
    tree: {
      kind: "split",
      direction: "horizontal",
      sizes: [65, 35],
      children: [
        {
          kind: "split",
          direction: "vertical",
          sizes: [65, 35],
          children: [
            { kind: "panel", views: [VIEW_INSTALLED] },
            { kind: "panel", views: [VIEW_LOGS] },
          ],
        },
        { kind: "panel", views: [VIEW_LIBRARY] },
      ],
    },
  });

  pluginCtx.effect(() => {
    const offStart = pluginCtx.slots.registerCommand({
      id: `${PLUGIN_ID}.start`,
      label: "llama.cpp：启动服务",
      run: (): void => {
        if (deps) void deps.host.start(deps.runtime);
      },
    });
    const offStop = pluginCtx.slots.registerCommand({
      id: `${PLUGIN_ID}.stop`,
      label: "llama.cpp：停止服务",
      run: (): void => {
        if (deps) void deps.host.stop();
      },
    });
    const offRefresh = pluginCtx.slots.registerCommand({
      id: `${PLUGIN_ID}.refresh`,
      label: "llama.cpp：刷新连接探测",
      run: (): void => {
        if (deps) void deps.runtime.probe();
      },
    });
    return () => {
      offStart();
      offStop();
      offRefresh();
    };
  });
}
