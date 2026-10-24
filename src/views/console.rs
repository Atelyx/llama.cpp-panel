//! 控制台面：服务状态条 + 启停 + 「已装模型｜启动参数」分栏 + 命令预览与进程日志。
//! 分栏用 `pane_row`（`Fill::Both`）吃掉面板剩余高度，两栏各自是滚动容器才有真视口；
//! 宿主把两个填充子项恒均分（`max_width` 对填充子项无效），故比例只能 50/50，日志区另给固定高度。

use atelyx_plugin_protocol::view::{
    ButtonVariant, CardTone, ColorToken, EventKind, NodeBody, TextFamily, TextWeight, ViewNode,
};

use super::*;
use crate::app::{selected_folder_index, Core, LogLine, LogTone, RunPhase, SURFACE_CONSOLE};
use crate::modelscan::{file_name_of, ModelFolder};
use crate::settings::{normalize_dir, params_for};

/// 日志行字号倍率：比正文小一档，一屏能多读几行输出。
const SIZE_LOG: f32 = 0.9;

pub fn console(core: &Core) -> ViewNode {
    let mut children = vec![top_bar(core)];
    children.extend(notices(core));
    children.push(models_tools(core));
    children.push(pane_row("main-panes", models_pane(core), params_pane(core)));
    children.push(node("logs-div", NodeBody::Divider {}));
    children.push(logs_head(core));
    if !core.host.start_line.is_empty() {
        // 命令预览标签行带复制钮：剪贴板没有界面反馈之外的一键入口
        children.push(column_gap(
            "logs-cmd-field",
            4.0,
            vec![
                spread_row(
                    "logs-cmd-label-row",
                    vec![
                        field_label("logs-cmd-field", "启动命令"),
                        icon_button("copycmd", "copy", "复制启动命令", true, vec![EventKind::Click]),
                    ],
                ),
                read_only_field("logs-cmd", core.host.start_line.clone()),
            ],
        ));
    }
    children.push(logs_body(core));
    panel_root(SURFACE_CONSOLE, children)
}

/// 状态条：名称 + 运行/连接徽标 + 服务地址（附复制钮），右端启停。
fn top_bar(core: &Core) -> ViewNode {
    let phase = core.run_phase();
    let (run_label, run_color) = run_status(phase);
    let (conn_label, conn_color) = if core.probe_online {
        ("已连接", ColorToken::Success)
    } else {
        ("未连接", ColorToken::TextMuted)
    };
    let busy = matches!(phase, RunPhase::Running | RunPhase::Starting);
    let left = row(
        "top-left",
        vec![
            title("app-name", "llama.cpp"),
            chip("run-state", run_label, run_color),
            chip("conn-state", conn_label, conn_color),
            text_muted("top-addr", service_addr(core)),
            icon_button("copyaddr", "copy", "复制服务地址", true, vec![EventKind::Click]),
        ],
    );
    let right = row(
        "top-right",
        vec![
            button_md("start", "启动", ButtonVariant::Primary, !busy, vec![EventKind::Click]),
            button_md("stop", "停止", ButtonVariant::Danger, busy, vec![EventKind::Click]),
        ],
    );
    spread_row("top-bar", vec![left, right])
}

fn run_status(phase: RunPhase) -> (&'static str, ColorToken) {
    let color = match phase {
        RunPhase::Running => ColorToken::Success,
        RunPhase::Starting => ColorToken::Warning,
        RunPhase::Watching => ColorToken::Accent,
        RunPhase::Failed => ColorToken::Danger,
        RunPhase::Stopped => ColorToken::TextSecondary,
    };
    (phase.label(), color)
}

/// 服务地址（监听地址留空按本机回环展示）。
fn service_addr(core: &Core) -> String {
    let host = core.settings.host.trim();
    let host = if host.is_empty() { "127.0.0.1" } else { host };
    format!("{host}:{}", core.settings.port)
}

/// 顶部提示区：最近一次操作错误 + 扫描状态。
fn notices(core: &Core) -> Vec<ViewNode> {
    let mut out = vec![];
    if !core.host.error.is_empty() {
        out.push(notice("host-error", CardTone::Default, ColorToken::Danger, core.host.error.clone()));
    }
    if core.scan.scanning {
        out.push(caption("scan-busy", "扫描模型目录中…"));
    }
    if !core.scan.error.is_empty() {
        out.push(notice("scan-error", CardTone::Default, ColorToken::Danger, core.scan.error.clone()));
    } else if core.scan.capped {
        out.push(notice(
            "scan-capped",
            CardTone::AccentSoft,
            ColorToken::Warning,
            "模型目录超出上限，列表已截断；请把常用模型放到更浅的子目录。",
        ));
    }
    out
}

/// 模型栏工具行：标题 + 计数徽标 + 过滤框 + 重扫 + 清除选择。
fn models_tools(core: &Core) -> ViewNode {
    let visible = visible_folders(core);
    let counting = if core.scan.filter.trim().is_empty() {
        format!("{} 个", visible.len())
    } else {
        format!("{} / {} 个", visible.len(), core.scan.folders.len())
    };
    fill_row(
        "models-tools",
        vec![
            section_head("models-title", "已装模型"),
            chip(
                "models-count",
                counting,
                if visible.is_empty() { ColorToken::TextMuted } else { ColorToken::Accent },
            ),
            text_input("filter", core.scan.filter.clone(), "按名称过滤", ReportMode::Debounced(300), vec![EventKind::Change]),
            icon_button("rescan", "refresh-cw", "重新扫描", !core.scan.scanning, vec![EventKind::Click]),
            button(
                "clearsel",
                "清除选择",
                ButtonVariant::Ghost,
                selected_folder_index(core).is_some(),
                vec![EventKind::Click],
            ),
        ],
    )
}

/// 左栏：模型卡片列表（过滤后为空给可读空态）。
fn models_pane(core: &Core) -> ViewNode {
    let visible = visible_folders(core);
    if visible.is_empty() {
        let (heading, hint) = if core.scan.scanning {
            ("正在扫描模型目录", "扫描完成后这里列出模型文件夹。")
        } else if core.scan.folders.is_empty() {
            ("未扫描到模型", "在「设置」选定模型目录，放入 .gguf 文件后点重新扫描。")
        } else {
            ("没有匹配的模型", "清空过滤词即可回到全部模型。")
        };
        return empty_state("models-empty", "hard-drive", heading, hint);
    }
    let selected_dir = selected_folder_index(core).map(|index| normalize_dir(&core.scan.folders[index].full));
    let cards = visible
        .iter()
        .map(|index| folder_card(*index, &core.scan.folders[*index], selected_dir.as_deref()))
        .collect();
    list("models-list", 6.0, cards)
}

/// 模型卡片：整卡可点选中，摘要行给量化/投影数量与总体积。
/// 下标是 `folders` 里的真实下标（事件路由按它定位，过滤只影响显示）。
fn folder_card(index: usize, folder: &ModelFolder, selected_dir: Option<&str>) -> ViewNode {
    let selected = selected_dir.is_some_and(|dir| dir == normalize_dir(&folder.full));
    let quant_size: u64 = folder.quants.iter().filter_map(|entry| entry.size).sum();
    row_card(
        format!("fcard{index}"),
        selected,
        vec![
            text_sized(
                format!("fcard{index}-name"),
                folder_name(&folder.rel),
                1.0,
                TextWeight::Medium,
                ColorToken::Text,
            ),
            spread_row(
                format!("fmeta{index}"),
                vec![
                    caption(
                        format!("fmeta-count{index}"),
                        format!("{} 个量化 · {} 个投影", folder.quants.len(), folder.mms.len()),
                    ),
                    chip(
                        format!("fmeta-size{index}"),
                        size_label((quant_size > 0).then_some(quant_size)),
                        ColorToken::TextSecondary,
                    ),
                ],
            ),
        ],
    )
}

/// 文件夹展示名：相对路径的末段（根目录文件夹给固定名）。
fn folder_name(rel: &str) -> String {
    if rel.is_empty() {
        return "（模型目录根）".into();
    }
    rel.rsplit('/').next().unwrap_or(rel).to_string()
}

/// 右栏：启动参数表单（未选中模型给空态）。
fn params_pane(core: &Core) -> ViewNode {
    let Some(index) = selected_folder_index(core) else {
        return empty_state("params-empty", "wrench", "尚未选择模型", "点左侧模型卡片选中一个文件夹，再调启动参数。");
    };
    let folder = &core.scan.folders[index];
    scroll("params-scroll", vec![column("params-col", params_body(core, folder))])
}

fn params_body(core: &Core, folder: &ModelFolder) -> Vec<ViewNode> {
    let params = params_for(&core.settings, &folder.full);
    let rel_of = |entry: &crate::modelscan::ModelEntry| file_name_of(&rel_name(folder, &entry.name));

    let quant_selected =
        folder.quants.iter().position(|entry| rel_name(folder, &entry.name) == params.quant).map(|i| i as u32);
    let quant_options = folder
        .quants
        .iter()
        .map(|entry| format!("{}（{}）", rel_of(entry), size_label(entry.size)))
        .collect();
    let mm_selected =
        folder.mms.iter().position(|entry| rel_name(folder, &entry.name) == params.mmproj).map(|i| (i + 1) as u32);
    let mut mm_options = vec!["不加载投影".to_string()];
    mm_options.extend(folder.mms.iter().map(rel_of));

    let model_line = if params.quant.is_empty() {
        "未选择量化文件".to_string()
    } else {
        file_name_of(&params.quant)
    };

    vec![
        row(
            "params-head",
            vec![
                section_head("params-title", "启动参数"),
                text_tone("params-model", model_line, ColorToken::Accent),
            ],
        ),
        caption("params-folder", folder_path(&folder.rel)),
        field_row(
            "quant-field",
            "量化文件",
            dropdown("quant", quant_options, quant_selected.or(Some(0)), vec![EventKind::Change]),
        ),
        field_row(
            "mmproj-field",
            "视觉投影",
            dropdown("mmproj", mm_options, mm_selected.or(Some(0)), vec![EventKind::Change]),
        ),
        // 数值参数两列并排：宿主 Grid 的单元格默认正方形（格高 = 列宽），表单行会被顶成高格
        fill_row(
            "ngl-ctx-row",
            vec![
                field_row(
                    "ngl-field",
                    "GPU 层数（-ngl）",
                    number_input("ngl", params.ngl.map(|v| v as f64), Some(0.0), "不传", vec![EventKind::Change]),
                ),
                field_row(
                    "ctx-field",
                    "上下文（--ctx-size）",
                    number_input("ctx", params.ctx_size.map(|v| v as f64), Some(1.0), "不传", vec![EventKind::Change]),
                ),
            ],
        ),
        fill_row(
            "threads-parallel-row",
            vec![
                field_row(
                    "threads-field",
                    "线程数（--threads）",
                    number_input("threads", params.threads.map(|v| v as f64), Some(1.0), "不传", vec![EventKind::Change]),
                ),
                field_row(
                    "parallel-field",
                    "并行槽位（--parallel）",
                    number_input("parallel", params.parallel.map(|v| v as f64), Some(1.0), "不传", vec![EventKind::Change]),
                ),
            ],
        ),
        fill_row(
            "cache-load-row",
            vec![
                field_row(
                    "cachek-field",
                    "KV 缓存 K 量化",
                    dropdown_values("cachek", &CACHE_K_OPTIONS, &CACHE_K_LABELS, &params.cache_type_k),
                ),
                field_row(
                    "loadmode-field",
                    "载入模式",
                    dropdown_values("loadmode", &LOAD_MODE_OPTIONS, &LOAD_MODE_LABELS, &params.load_mode),
                ),
            ],
        ),
        row(
            "params-switches",
            vec![
                checkbox("fa", params.flash_attn, "Flash Attention", vec![EventKind::Change]),
                checkbox("jinja", params.jinja, "Jinja 模板", vec![EventKind::Change]),
                checkbox("nowebui", params.no_webui, "隐藏 WebUI", vec![EventKind::Change]),
            ],
        ),
        field_row(
            "extra-field",
            "附加参数（空格分隔，含空格的参数用引号包裹）",
            text_input("extra", params.extra_args.clone(), "追加在命令末尾", ReportMode::Commit, vec![EventKind::Change]),
        ),
    ]
}

/// 参数区副标题：文件夹的展示路径（根目录给固定说法）。
fn folder_path(rel: &str) -> String {
    if rel.is_empty() {
        "模型目录根".into()
    } else {
        rel.to_string()
    }
}

/// 选中文件夹内条目的相对名。
fn rel_name(folder: &ModelFolder, name: &str) -> String {
    crate::modelscan::rel_name(&folder.rel, name)
}

fn logs_head(core: &Core) -> ViewNode {
    let left = row(
        "logs-head-left",
        vec![
            section_head("logs-title", "日志"),
            chip(
                "logs-count",
                format!("{} 行", core.host.logs.len()),
                if core.host.logs.is_empty() { ColorToken::TextMuted } else { ColorToken::TextSecondary },
            ),
        ],
    );
    let mut right = vec![checkbox("follow", core.host.follow, "跟随最新", vec![EventKind::Change])];
    if !core.host.logs.is_empty() {
        right.push(icon_button("clear", "eraser", "清空日志", true, vec![EventKind::Click]));
    }
    spread_row("logs-head", vec![left, row("logs-head-right", right)])
}

/// 日志区：固定高度滚动；无日志给空态（命令预览在其上方，始终可读）。
fn logs_body(core: &Core) -> ViewNode {
    if core.host.logs.is_empty() {
        return empty_state("logs-empty", "file-text", "暂无日志", "启动 llama-server 后，这里显示进程输出。");
    }
    let lines = core.host.logs.iter().map(log_row).collect();
    scroll_fixed("logs-scroll", LOG_AREA_HEIGHT, 2.0, lines)
}

/// 一行日志：等宽族按位列对齐（终端样式），时刻前缀同色系。
/// 取色优先级：级别 > ANSI 主色 > 「listening on」就绪行绿 > 正文；
/// 行键取累计序号（覆盖刷新与清屏都不复用序号，控件态不错位）。
fn log_row(line: &LogLine) -> ViewNode {
    let color = match line.tone {
        LogTone::Error => ColorToken::Danger,
        LogTone::Warn => ColorToken::Warning,
        LogTone::Normal => line.ansi.unwrap_or_else(|| {
            if line.text.contains("listening on") {
                ColorToken::Success
            } else {
                ColorToken::Text
            }
        }),
    };
    node(
        format!("log{}", line.seq),
        NodeBody::Text {
            content: format!("[{}] {}", line.time, line.text),
            size: Some(SIZE_LOG),
            weight: Some(TextWeight::Normal),
            color: Some(ColorSpec::Token(color)),
            align: None,
            family: Some(TextFamily::Mono),
            wrap: None,
            max_lines: None,
        },
    )
}
