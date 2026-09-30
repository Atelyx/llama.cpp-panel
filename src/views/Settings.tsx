/**
 * 设置页：全局项——llama-server 文件夹、监听地址、模型目录、端口、进程模式。
 *
 * 逐项改动即生效并落盘（无保存按钮）：改动都是用户明确触发，改完不丢更符合直觉。
 * 选模型与启动参数在启动面板按模型文件夹操作，不在此重复。
 */
import React from "react";
import type { AtelyxCtx } from "../ctx";
import { type LlamaSettings, type ProcessMode } from "../settings";
import {
  Button,
  Card,
  Field,
  Notice,
  Select,
  TextInput,
  bgSecondary,
  textPrimary,
  FONT_SM,
} from "./ui";

interface SettingsProps {
  ctx: AtelyxCtx;
  settings: LlamaSettings;
  onSettingsChanged: (next: LlamaSettings) => void;
}

/** 端口合法性判定（与 settings.coerce 一致）。 */
function isPortValid(v: number): boolean {
  return Number.isInteger(v) && v > 0 && v < 65536;
}

export function SettingsView(props: SettingsProps): unknown {
  const { ctx, settings, onSettingsChanged } = props;

  const isExternal = settings.processMode === "external";

  // 改一项即生效；落盘由入口 onSettingsChanged 统一收口
  const patch = React.useCallback(
    (changes: Partial<LlamaSettings>): void => {
      onSettingsChanged({ ...settings, ...changes });
    },
    [settings, onSettingsChanged],
  );

  // 本地对话框错误展示（对话框失败不影响表单）
  const [dialogError, setDialogError] = React.useState("");
  function notifyErr(err: unknown): void {
    setDialogError(err instanceof Error ? err.message : String(err));
  }

  const pickFolder = React.useCallback(
    async (field: "modelsDir" | "serverDir"): Promise<void> => {
      setDialogError("");
      try {
        const dir = await ctx.dialog.pickDirectory();
        if (dir) patch(field === "modelsDir" ? { modelsDir: dir } : { serverDir: dir });
      } catch (err) {
        notifyErr(err);
      }
    },
    [ctx, patch],
  );

  const setPort = React.useCallback(
    (v: string): void => {
      const n = parseInt(v, 10);
      if (isPortValid(n)) patch({ port: n });
    },
    [patch],
  );

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        background: bgSecondary,
        color: textPrimary,
        fontSize: FONT_SM,
        boxSizing: "border-box",
      }}
    >
      {dialogError && (
        <Notice tone="error" onClose={() => setDialogError("")}>
          选择失败：{dialogError}
        </Notice>
      )}

      <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: "8px 10px 16px" }}>
        <Card title="连接">
          <Field label="监听地址" hint="默认 127.0.0.1 仅本机可访问；0.0.0.0 允许局域网设备调用。">
            <TextInput value={settings.host} onChange={(v) => patch({ host: v.trim() })} placeholder="127.0.0.1" />
          </Field>
          <Field label="端口" hint="服务端口。">
            <TextInput
              value={String(settings.port)}
              onChange={setPort}
              type="number"
              min={1}
              max={65535}
              placeholder={String(settings.port)}
            />
          </Field>
        </Card>

        <Card title={isExternal ? "外部进程（只检测，不托管）" : "进程托管"}>
          <Field label="llama-server 文件夹" hint="文件夹里放 llama-server（Windows 为 llama-server.exe），启动时按平台名在里面查找。">
            <div style={{ display: "flex", gap: 6 }}>
              <TextInput value={settings.serverDir} onChange={(v) => patch({ serverDir: v })} placeholder="选择或输入 llama-server 所在文件夹" style={{ flex: 1 }} />
              <Button onClick={() => pickFolder("serverDir")} title="选择 llama-server 所在文件夹">选择</Button>
            </div>
          </Field>
          <Field label="进程模式" hint="托管：插件负责启停；外部：只检测连接。">
            <Select
              value={settings.processMode}
              onChange={(v) => patch({ processMode: v as ProcessMode })}
              options={[
                { value: "managed", label: "托管（本插件启停）" },
                { value: "external", label: "外部（只检测连接）" },
              ]}
            />
          </Field>
        </Card>

        <Card title="模型">
          <Field label="模型目录" hint="递归扫描 *.gguf 的根目录。选模型与启动参数在面板的模型区。">
            <div style={{ display: "flex", gap: 6 }}>
              <TextInput value={settings.modelsDir} onChange={(v) => patch({ modelsDir: v })} placeholder="选择或输入模型文件夹" style={{ flex: 1 }} />
              <Button onClick={() => pickFolder("modelsDir")} title="选择模型目录">选择</Button>
            </div>
          </Field>
        </Card>
      </div>
    </div>
  );
}