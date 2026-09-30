/** 「已装模型」页签的启动参数表单与选中语义。值按模型文件夹持久化，选中即启动目标。 */
import React from "react";
import { normalizeDir, paramsFor, type LlamaSettings, type ModelLaunchParams } from "../settings";
import { joinModelPath, type ModelEntry, type ModelFolder } from "../host/models";
import { fileNameOf, formatSize } from "../host/types";
import { Checkbox, Field, Select, TextInput } from "./ui";

/** 扫描条目名自带 modelsDir 前缀，剥掉它才是相对所在文件夹的路径；不剥会拼出中段重复的死路径。 */
export function folderRelName(folder: ModelFolder, entry: ModelEntry): string {
  return folder.rel ? entry.name.slice(folder.rel.length + 1) : entry.name;
}

/** 数字输入 → 可选整数；空/非法 = null（不传该参数）。 */
function optionalInt(v: string, min: number): number | null {
  const n = parseInt(v, 10);
  return Number.isInteger(n) && n >= min ? n : null;
}

/**
 * 从设置里找当前选中模型所属的文件夹。
 *
 * 选中态以启动字段（modelFile）为准：页签分处不同面板时，它们读同一份设置，选中态自然一致。
 */
export function selectedFolderOf(folders: ModelFolder[], settings: LlamaSettings): ModelFolder | null {
  const dir = settings.modelFile.replace(/\\/g, "/").split("/").slice(0, -1).join("/");
  return folders.find((f) => normalizeDir(f.full) === normalizeDir(dir)) ?? null;
}

/** 选中一个模型文件夹后的完整设置（量化与投影的旧值失效时落到第一个）。 */
export function selectFolderSettings(settings: LlamaSettings, folder: ModelFolder): LlamaSettings {
  const prev = paramsFor(settings, folder.full);
  const quant = folder.quants.some((q) => folderRelName(folder, q) === prev.quant)
    ? prev.quant
    : folder.quants[0]
      ? folderRelName(folder, folder.quants[0])
      : "";
  const mmproj = folder.mms.some((m) => folderRelName(folder, m) === prev.mmproj)
    ? prev.mmproj
    : folder.mms[0]
      ? folderRelName(folder, folder.mms[0])
      : "";
  return {
    ...settings,
    modelFile: quant ? joinModelPath(folder.full, quant) : "",
    mmprojFile: mmproj ? joinModelPath(folder.full, mmproj) : "",
    modelParams: { ...settings.modelParams, [normalizeDir(folder.full)]: { ...prev, quant, mmproj } },
  };
}

/** 改写选中文件夹的启动参数后的完整设置（量化决定启动用的模型文件）。 */
export function updateParamsSettings(
  settings: LlamaSettings,
  folder: ModelFolder,
  changes: Partial<ModelLaunchParams>,
): LlamaSettings {
  const next: ModelLaunchParams = { ...paramsFor(settings, folder.full), ...changes };
  return {
    ...settings,
    modelFile: next.quant ? joinModelPath(folder.full, next.quant) : settings.modelFile,
    mmprojFile: next.mmproj ? joinModelPath(folder.full, next.mmproj) : "",
    modelParams: { ...settings.modelParams, [normalizeDir(folder.full)]: next },
  };
}

interface ParamsFormProps {
  settings: LlamaSettings;
  folder: ModelFolder;
  onChange: (changes: Partial<ModelLaunchParams>) => void;
}

/** 模型文件夹的启动参数表单（值随模型文件夹持久化）。 */
export function ParamsForm(props: ParamsFormProps): unknown {
  const { settings, folder, onChange } = props;
  const params = paramsFor(settings, folder.full);
  return (
    <>
      <Field label="量化文件">
        <Select
          value={params.quant || (folder.quants[0] ? folderRelName(folder, folder.quants[0]) : "")}
          onChange={(v) => onChange({ quant: v })}
          options={folder.quants.map((q) => ({
            value: folderRelName(folder, q),
            label: `${fileNameOf(q.name)}${q.size != null ? `（${formatSize(q.size)}）` : ""}`,
          }))}
        />
      </Field>
      <Field label="视觉投影（多模态模型）">
        <Select
          value={params.mmproj}
          onChange={(v) => onChange({ mmproj: v })}
          options={[
            { value: "", label: "不挂投影" },
            ...folder.mms.map((m) => ({ value: folderRelName(folder, m), label: fileNameOf(m.name) })),
          ]}
        />
      </Field>
      <div style={{ display: "flex", gap: 8 }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="GPU 层数（-ngl）">
            <TextInput
              value={params.ngl === null ? "" : String(params.ngl)}
              onChange={(v) => onChange({ ngl: optionalInt(v, 0) })}
              type="number"
              min={0}
              placeholder="不传"
            />
          </Field>
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="上下文（--ctx-size）">
            <TextInput
              value={params.ctxSize === null ? "" : String(params.ctxSize)}
              onChange={(v) => onChange({ ctxSize: optionalInt(v, 1) })}
              type="number"
              min={1}
              placeholder="默认"
            />
          </Field>
        </div>
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="线程数（--threads）">
            <TextInput
              value={params.threads === null ? "" : String(params.threads)}
              onChange={(v) => onChange({ threads: optionalInt(v, 1) })}
              type="number"
              min={1}
              placeholder="自动"
            />
          </Field>
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="并行槽位（--parallel）">
            <TextInput
              value={params.parallel === null ? "" : String(params.parallel)}
              onChange={(v) => onChange({ parallel: optionalInt(v, 1) })}
              type="number"
              min={1}
              placeholder="1"
            />
          </Field>
        </div>
      </div>
      <Field label="KV 缓存 K 量化" hint="量化 KV 缓存省显存。">
        <Select
          value={params.cacheTypeK}
          onChange={(v) => onChange({ cacheTypeK: v })}
          options={[
            { value: "", label: "默认（f16）" },
            { value: "q8_0", label: "q8_0（显存减半，近无损）" },
            { value: "q4_0", label: "q4_0（更省，质量略降）" },
            { value: "q5_1", label: "q5_1" },
            { value: "bf16", label: "bf16" },
          ]}
        />
      </Field>
      <Field label="载入模式">
        <Select
          value={params.loadMode}
          onChange={(v) => onChange({ loadMode: v })}
          options={[
            { value: "", label: "默认（auto）" },
            { value: "auto", label: "auto（自动选择）" },
            { value: "mmap", label: "mmap（内存映射）" },
            { value: "mmap+mlock", label: "mmap+mlock（映射并锁定）" },
            { value: "mlock", label: "mlock（锁定内存）" },
            { value: "none", label: "none（直接读入）" },
            { value: "dio", label: "dio（直接 I/O）" },
          ]}
        />
      </Field>
      <Field label="常用开关">
        <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 16px" }}>
          <Checkbox
            checked={params.flashAttn}
            onChange={(c) => onChange({ flashAttn: c })}
            label="Flash Attention"
            title="--flash-attn on：注意力提速并省显存"
          />
          <Checkbox
            checked={params.jinja}
            onChange={(c) => onChange({ jinja: c })}
            label="Jinja 模板"
            title="--jinja：使用模型自带的聊天模板"
          />
          <Checkbox
            checked={params.noWebui}
            onChange={(c) => onChange({ noWebui: c })}
            label="隐藏 WebUI"
            title="--no-webui：不开放内置网页界面"
          />
        </div>
      </Field>
      <Field label="附加参数">
        <TextInput
          value={params.extraArgs}
          onChange={(v) => onChange({ extraArgs: v })}
          placeholder="空白分隔，含空格的项用引号包裹"
        />
      </Field>
    </>
  );
}
