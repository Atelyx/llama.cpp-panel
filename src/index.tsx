/**
 * 插件入口：装配启动面板、设置页与命令。
 *
 * 连接状态（runtime）与进程托管（host）在 `apply` 期间建立一次，界面消费其快照，
 * 因此切面板不会断连接；停用时注册项与轮询随上下文一起撤销。
 *
 * 进程收尾由 Atelyx 统一负责（插件停用/卸载/应用退出都会结束本插件启动的进程树），
 * 本插件只停轮询，不自己 cancel 进程——避免误杀用户自起的外部服务。
 */
import React from "react";
import type { AtelyxCtx } from "./ctx";
import { loadSettings, saveSettings, type LlamaSettings } from "./settings";
import { LlamaRuntime } from "./runtime";
import { HostController } from "./host/controller";
import { LaunchPanel } from "./views/LaunchPanel";
import { SettingsView } from "./views/Settings";
import { bgSecondary, border, FONT_SM, textMuted } from "./views/ui";

/** 与清单里的 name 一致。 */
const PLUGIN_ID = "com.atelyx.llama.cpp-panel";

const VIEW_KIND = PLUGIN_ID;
const SETTING_KEY = `${PLUGIN_ID}.settings`;

/** 三者就绪后才可渲染。 */
interface PanelDeps {
  ctx: AtelyxCtx;
  runtime: LlamaRuntime;
  host: HostController;
  settings: LlamaSettings;
  onSettingsChanged: (next: LlamaSettings) => void;
}

export default function apply(pluginCtx: AtelyxCtx): void {
  // 设置要先读出来才能建运行时（地址与端口决定传输层）。读取是异步的，
  // 故设一个就绪后通知订阅者的小状态：未就绪时显示初始化中，而不是闪空白。
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

  // 设置变更：更新运行时/宿主的内存设置、重建 deps（新引用触发视图重渲），落盘统一在此收口——
  // 面板的每模型参数与设置页的全局项走同一条路，任何改动都持久化，视图侧不再各自保存。
  const onSettingsChanged = (next: LlamaSettings): void => {
    if (deps) {
      deps.runtime.applySettings(next);
      deps.host.applySettings(next);
      // 换一个新对象引用，getDeps 返回的快照才变化，useSyncExternalStore 才会重渲
      deps = { ...deps, settings: next };
    }
    void saveSettings(pluginCtx, next);
    notify();
  };

  void loadSettings(pluginCtx)
    .then((loaded) => {
      const runtime = new LlamaRuntime(loaded);
      const host = new HostController(pluginCtx, loaded);
      deps = { ctx: pluginCtx, runtime, host, settings: loaded, onSettingsChanged };

      // 常驻轮询保持连接状态新鲜
      runtime.startPolling();

      // 首次立即探测一次：给界面一个即时连接状态
      void runtime.probe();
      notify();
    })
    .catch((err: unknown) => {
      pluginCtx.notification.notify({
        level: "warning",
        message: `设置读取失败，已用默认值启动：${err instanceof Error ? err.message : String(err)}`,
      });
    });

  /** 面板外壳：铺满可用空间（显式 width 防宿主包装层出现收缩上下文时缩成内容宽），自带边框跟随宿主主题。 */
  function PanelShell(props: { children: unknown }): unknown {
    return (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          minHeight: 0,
          border: `1px solid ${border}`,
          boxSizing: "border-box",
          background: bgSecondary,
        }}
      >
        {props.children}
      </div>
    );
  }

  function InitPlaceholder(): unknown {
    return (
      <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: textMuted, fontSize: FONT_SM }}>
        正在初始化…
      </div>
    );
  }

  function LaunchView(): unknown {
    const ready = React.useSyncExternalStore(subscribe, getDeps);
    if (!ready) return <InitPlaceholder />;
    return (
      <PanelShell>
        <LaunchPanel ctx={ready.ctx} runtime={ready.runtime} host={ready.host} settings={ready.settings} onSettingsChanged={ready.onSettingsChanged} />
      </PanelShell>
    );
  }

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
    // 启动面板与设置页
    const offView = pluginCtx.slots.registerView({
      kind: VIEW_KIND,
      label: "llama.cpp",
      component: LaunchView,
    });
    const offSetting = pluginCtx.slots.registerSetting({
      key: SETTING_KEY,
      label: "llama.cpp",
      component: SettingsPanelView,
    });
    return () => {
      offView();
      offSetting();
      // 只停轮询；进程树由 Atelyx 在停用/卸载时结束，这里不 cancel
      deps?.runtime.stopPolling();
    };
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