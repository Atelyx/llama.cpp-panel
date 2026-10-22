//! 模型商店面：检索工具行 + 「结果列表｜仓库详情」分栏 + 下载卡与按地址直下载。

use atelyx_plugin_protocol::view::{ButtonVariant, CardTone, ColorToken, EventKind, NodeBody, ViewNode};

use super::*;
use crate::app::{Core, SURFACE_LIBRARY};
use crate::download::DownloadStatus;
use crate::hf::SearchSort;
use crate::modelscan::short_date;

pub fn library(core: &Core) -> ViewNode {
    let lib = &core.lib;
    let mut children = vec![
        fill_row(
            "lib-search",
            vec![
                text_input("q", lib.query.clone(), "搜索模型，回车检索", ReportMode::Debounced(300), vec![
                    EventKind::Change,
                    EventKind::Submit,
                ]),
                button_md("search", "搜索", ButtonVariant::Primary, !lib.searching, vec![EventKind::Click]),
            ],
        ),
        fill_row(
            "lib-tools",
            vec![
                dropdown(
                    "sort",
                    vec!["按下载量".into(), "按点赞".into(), "最近更新".into()],
                    Some(match lib.sort {
                        SearchSort::Downloads => 0,
                        SearchSort::Likes => 1,
                        SearchSort::LastModified => 2,
                    }),
                    vec![EventKind::Change],
                ),
                dropdown(
                    "libmirror",
                    crate::mirrors::MIRRORS.iter().map(|m| m.label.to_string()).collect(),
                    Some(
                        crate::mirrors::MIRRORS
                            .iter()
                            .position(|m| m.id == core.settings.download_mirror)
                            .unwrap_or(0) as u32,
                    ),
                    vec![EventKind::Change],
                ),
            ],
        ),
    ];

    if !lib.error.is_empty() {
        children.push(notice("lib-error", CardTone::Default, ColorToken::Danger, lib.error.clone()));
    }

    let mut status = vec![
        section_head("lib-results-title", "检索结果"),
        chip(
            "lib-count",
            format!("{} 条", lib.results.len()),
            if lib.results.is_empty() { ColorToken::TextMuted } else { ColorToken::Accent },
        ),
    ];
    if lib.searching {
        status.push(caption("lib-busy", "检索中…"));
    }
    children.push(row("lib-status", status));

    children.push(pane_row(
        "lib-panes",
        results_pane(core),
        scroll("lib-detail", detail_pane(core)),
    ));

    if core.dl.status != DownloadStatus::Idle {
        children.push(download_card(core));
    }
    children.push(field_row(
        "lib-direct",
        "按地址直接下载",
        fill_row(
            "lib-direct-row",
            vec![
                text_input("dlinput", lib.dl_input.clone(), "整条下载地址，或 owner/repo/文件名", ReportMode::Commit, vec![
                    EventKind::Change,
                    EventKind::Submit,
                ]),
                button_md("dlgo", "下载", ButtonVariant::Ghost, true, vec![EventKind::Click]),
            ],
        ),
    ));
    panel_root(SURFACE_LIBRARY, children)
}

/// 左栏：检索结果列表（滚到底自动翻页）。
fn results_pane(core: &Core) -> ViewNode {
    let lib = &core.lib;
    if lib.results.is_empty() {
        if lib.searching {
            return text_secondary("lib-loading", "检索中…");
        }
        let hint = if lib.query.trim().is_empty() {
            "输入关键词回车检索；留空回车即取热门列表。"
        } else {
            "换个关键词，或用空关键词回车取热门列表。"
        };
        return empty_state("lib-empty", "search", "没有匹配的模型", hint);
    }
    let mut items: Vec<ViewNode> = lib
        .results
        .iter()
        .enumerate()
        .map(|(index, summary)| {
            let selected = lib
                .detail
                .as_ref()
                .is_some_and(|detail| detail.summary.repo == summary.repo);
            row_card(
                format!("rcard{index}"),
                selected,
                vec![
                    button(
                        format!("r{index}"),
                        summary.repo.clone(),
                        ButtonVariant::Ghost,
                        true,
                        vec![EventKind::Click],
                    ),
                    spread_row(
                        format!("rmeta{index}"),
                        vec![
                            caption(
                                format!("rmeta-left{index}"),
                                format!("下载 {} · 赞 {}", thousands(summary.downloads), thousands(summary.likes)),
                            ),
                            caption(format!("rmeta-right{index}"), short_date(&summary.updated_at)),
                        ],
                    ),
                ],
            )
        })
        .collect();
    if lib.exhausted {
        items.push(caption("lib-end", "已到列表末尾"));
    } else if !lib.searching {
        items.push(button("more", "加载更多", ButtonVariant::Ghost, true, vec![EventKind::Click]));
    }
    let mut list = list("lib-results", 6.0, items);
    // 滚动位置只用来判定「滚到底」：接近末端即取下一页
    list.events = vec![EventKind::Scroll];
    list
}

/// 右栏仓库详情：统计、文件清单与简介（README 交宿主按 Markdown 渲染）。
fn detail_pane(core: &Core) -> Vec<ViewNode> {
    let lib = &core.lib;
    let Some(detail) = &lib.detail else {
        if lib.detail_loading {
            return vec![text_secondary("detail-hint", "仓库详情加载中…")];
        }
        return vec![empty_state(
            "detail-empty",
            "box",
            "尚未选择模型",
            "点左侧结果，这里列出该仓库的 .gguf 文件与模型简介。",
        )];
    };
    let summary = &detail.summary;
    let short_name = summary.repo.rsplit('/').next().unwrap_or(&summary.repo);

    let mut children = vec![
        title("detail-title", short_name.to_string()),
        caption("detail-full", summary.repo.clone()),
        caption(
            "detail-stats",
            format!(
                "下载 {} · 赞 {} · 更新于 {}",
                thousands(summary.downloads),
                thousands(summary.likes),
                short_date(&summary.updated_at)
            ),
        ),
    ];
    if detail.gated {
        children.push(notice(
            "detail-gated",
            CardTone::AccentSoft,
            ColorToken::Warning,
            "门控模型需先在官网同意条款，插件无法代下；请用浏览器下载后放入模型目录。",
        ));
    }
    if lib.detail_loading {
        children.push(text_secondary("detail-loading", "仓库详情加载中…"));
    }
    children.push(node("detail-div", NodeBody::Divider {}));
    children.push(spread_row(
        "detail-files-head",
        vec![
            section_head("detail-files-label", "文件"),
            chip("detail-files-num", format!("{} 个 gguf", detail.files.len()), ColorToken::Accent),
        ],
    ));
    if detail.files.is_empty() {
        children.push(caption("detail-files-empty", "该仓库没有 .gguf 文件，可能只提供原始权重。"));
    }
    for (index, file) in detail.files.iter().enumerate() {
        // 文件相对路径占满左半（长路径会把同行控件顶出可视区，故给占宽行），体积与下载钮贴右
        children.push(fill_row(
            format!("frow{index}"),
            vec![
                text_fill(format!("fname{index}"), file.name.clone(), ColorToken::TextSecondary),
                row(
                    format!("fright{index}"),
                    vec![
                        chip(format!("fsize{index}"), size_label(file.size), ColorToken::TextSecondary),
                        icon_button(format!("f{index}"), "download", "下载", true, true, vec![EventKind::Click]),
                    ],
                ),
            ],
        ));
    }

    if !detail.readme.is_empty() {
        children.push(node("detail-readme-div", NodeBody::Divider {}));
        children.push(section_head("detail-readme-h", "模型简介"));
        children.push(markdown("detail-readme", detail.readme.clone()));
    }
    children
}

/// 下载任务卡：状态徽标 + 进度 + 读数 + 暂停/继续/改名/关闭。
fn download_card(core: &Core) -> ViewNode {
    let task = &core.dl;
    let name = if task.repo.is_empty() && task.file.is_empty() {
        "下载任务".to_string()
    } else {
        format!("{}/{}", task.repo, task.file)
    };
    let (state, color) = match task.status {
        DownloadStatus::Downloading => ("下载中", ColorToken::Accent),
        DownloadStatus::Paused => ("已暂停", ColorToken::TextSecondary),
        DownloadStatus::Done => ("已完成", ColorToken::Success),
        DownloadStatus::Error => ("失败", ColorToken::Danger),
        DownloadStatus::Idle => ("空闲", ColorToken::TextMuted),
    };
    let mut children = vec![
        spread_row(
            "dl-head",
            vec![
                section_head("dl-title", name),
                chip("dl-state", state, color),
            ],
        ),
    ];
    if task.status == DownloadStatus::Downloading {
        if let Some(total) = task.total_bytes.filter(|total| *total > 0) {
            children.push(node(
                "dl-bar",
                NodeBody::Progress { value: task.bytes as f64 / total as f64, girth: None, tone: None },
            ));
        }
    }
    if !task.status_text.is_empty() {
        children.push(if task.status == DownloadStatus::Error {
            text_tone("dl-status", task.status_text.clone(), ColorToken::Danger)
        } else {
            text_secondary("dl-status", task.status_text.clone())
        });
    }
    if let Some(target) = (!task.target.is_empty() && task.status != DownloadStatus::Done)
        .then_some(task.target.as_str())
    {
        children.push(caption("dl-target", format!("落盘：{target}")));
    }
    let mut actions = vec![];
    if task.status == DownloadStatus::Downloading {
        actions.push(button("dlpause", "暂停", ButtonVariant::Ghost, true, vec![EventKind::Click]));
    }
    if core.lib.last_download.is_some() {
        // 暂停态给「继续」（.part 保留，断点续传）
        if task.status == DownloadStatus::Paused {
            actions.push(button("dlresume", "继续", ButtonVariant::Primary, true, vec![EventKind::Click]));
        }
        // 只有目标重名被拒时才给「改名保留」：其余失败换名也下不来
        if task.status == DownloadStatus::Error && core.lib.dl_conflict {
            actions.push(button("dlrename", "改名保留", ButtonVariant::Ghost, true, vec![EventKind::Click]));
        }
    }
    if task.status != DownloadStatus::Downloading {
        // 下载中没有「关闭」：任务未落定前收掉卡片只会把进度与结果一起藏掉
        actions.push(button("dldismiss", "关闭", ButtonVariant::Ghost, true, vec![EventKind::Click]));
    }
    children.push(row("dl-actions", actions));
    card_box("dl-card", false, CardTone::Default, 10.0, children)
}
