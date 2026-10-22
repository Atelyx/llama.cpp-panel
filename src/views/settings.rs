//! 设置面：目录、监听、下载镜像与自动启动的表单（分区标题 + 分割线）。

use atelyx_plugin_protocol::view::{ButtonVariant, EventKind, NodeBody, ViewNode};

use super::*;
use crate::app::{Core, SURFACE_SETTINGS};

pub fn settings_panel(core: &Core) -> ViewNode {
    let settings = &core.settings;
    let children = vec![
        section_head("set-svc", "服务"),
        dir_field("server", "llama-server 所在文件夹", &settings.server_dir),
        dir_field("models", "模型目录", &settings.models_dir),
        node("set-svc-div", NodeBody::Divider {}),
        section_head("set-listen", "监听"),
        field_row(
            "host-field",
            "监听地址（--host；0.0.0.0 允许局域网访问）",
            text_input("host", settings.host.clone(), "127.0.0.1", ReportMode::Commit, vec![EventKind::Change]),
        ),
        field_row(
            "port-field",
            "端口（--port）",
            number_input_max("port", Some(settings.port as f64), Some(1.0), Some(65535.0), "", vec![EventKind::Change]),
        ),
        node("set-listen-div", NodeBody::Divider {}),
        section_head("set-dl", "下载"),
        field_row(
            "mirror-field",
            "下载镜像站",
            dropdown(
                "mirror",
                crate::mirrors::MIRRORS.iter().map(|m| m.label.to_string()).collect(),
                Some(
                    crate::mirrors::MIRRORS
                        .iter()
                        .position(|m| m.id == settings.download_mirror)
                        .unwrap_or(0) as u32,
                ),
                vec![EventKind::Change],
            ),
        ),
        node("set-dl-div", NodeBody::Divider {}),
        section_head("set-auto", "自动启动"),
        checkbox(
            "autostart",
            settings.auto_start_on_api_call,
            "外部调用自动启动（未运行时持有端口，收到 API 调用即启动）",
            vec![EventKind::Change],
        ),
        caption(
            "settings-note",
            "待命期间端口由插件持有，请勿在插件外启动 llama-server；服务退出后自动恢复待命，启动失败时暂缓。",
        ),
    ];
    panel_root(SURFACE_SETTINGS, children)
}

/// 「标签 + 当前路径 + 选择钮」的目录字段。
fn dir_field(id: &str, label: &str, value: &str) -> ViewNode {
    let control = spread_row(
        format!("{id}-row"),
        vec![
            text_secondary(format!("{id}-dir"), if value.is_empty() { "未设置" } else { value }),
            button(format!("pick{id}"), "选择…", ButtonVariant::Ghost, true, vec![EventKind::Click]),
        ],
    );
    column_gap(format!("{id}-field"), 4.0, vec![field_label(format!("{id}-field"), label), control])
}
