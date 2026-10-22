//! ViewNode 树构建：Core 快照 → 各面的界面树（纯函数，无副作用）；一面一文件，构造器共用。
//! 交互节点的事件声明与 app 的事件路由按节点 id 一一对应；输入回显值取 Core 里的权威值。
//! 宿主无徽标/提示块/空态这类语义控件，也不给文字配装饰图标，这些由卡片 + 小号文字拼出，每个文字节点显式声明字重。

mod console;
mod library;
mod settings;

use atelyx_plugin_protocol::view::{
    Align, BoxSize, ButtonVariant, CardTone, ColorSpec, ColorToken, ControlSize, EventKind, Justify,
    NodeBody, PaddingSpec, ReportMode, SizeSpec, TextWeight, ViewNode,
};

use crate::app::{Core, SURFACE_CONSOLE, SURFACE_LIBRARY, SURFACE_SETTINGS};

/// KV 缓存 K 量化的可选值（下标即界面下拉的选中项；空串 = 不传该旗标）。
/// 取值集合与设置侧的 `KV_K_TYPES` 一致，否则已存的合法值在界面上显示成「默认」而命令仍带着旗标。
pub const CACHE_K_OPTIONS: [&str; 7] = ["", "bf16", "q8_0", "q5_1", "q5_0", "q4_1", "q4_0"];
/// KV 缓存 K 量化的展示标签（与 [`CACHE_K_OPTIONS`] 一一对应）。
pub const CACHE_K_LABELS: [&str; 7] =
    ["默认（f16）", "bf16", "q8_0（近无损）", "q5_1", "q5_0", "q4_1（精度略降）", "q4_0（精度略降）"];
/// 载入模式的可选值。
pub const LOAD_MODE_OPTIONS: [&str; 7] = ["", "auto", "mmap", "mmap+mlock", "mlock", "none", "dio"];
/// 载入模式的展示标签。
pub const LOAD_MODE_LABELS: [&str; 7] = [
    "默认（auto）",
    "auto（自动选择）",
    "mmap（内存映射）",
    "mmap+mlock（映射并锁定）",
    "mlock（锁定内存）",
    "none（直接读入）",
    "dio（直接 I/O）",
];

/// 控制台日志区高度（固定高，滚动看历史）。
const LOG_AREA_HEIGHT: f32 = 220.0;
/// 字号倍率（相对宿主正文）：标题 / 次级标题 / 正文 / 辅助行。
const SIZE_TITLE: f32 = 1.2;
const SIZE_HEAD: f32 = 1.05;
const SIZE_CAPTION: f32 = 0.85;

// ---------- 本地构造器（字段默认值收拢在此，事件声明保持显式） ----------

fn node(id: impl Into<String>, body: NodeBody) -> ViewNode {
    ViewNode {
        id: id.into(),
        body,
        interaction: None,
        grow: None,
        children: vec![],
        events: vec![],
    }
}

fn with_children(id: impl Into<String>, body: NodeBody, children: Vec<ViewNode>) -> ViewNode {
    let mut n = node(id, body);
    n.children = children;
    n
}

fn row_body(gap: f32, justify: Option<Justify>, cross: Option<Align>, size: Option<BoxSize>) -> NodeBody {
    NodeBody::Row { gap: Some(gap), padding: None, justify, cross_align: cross, size }
}

fn fill_width() -> Option<BoxSize> {
    Some(BoxSize { width: Some(SizeSpec::Fill), ..BoxSize::default() })
}

fn fill_both() -> Option<BoxSize> {
    Some(BoxSize { width: Some(SizeSpec::Fill), height: Some(SizeSpec::Fill), ..BoxSize::default() })
}

/// 居中行（图标与文字并排这类）。
fn row(id: impl Into<String>, children: Vec<ViewNode>) -> ViewNode {
    with_children(id, row_body(8.0, None, Some(Align::Center), None), children)
}

/// 占满宽度的工具行：Fill 子项均分剩余宽度（搜索框这类需要吃满空间的控件）。
fn fill_row(id: impl Into<String>, children: Vec<ViewNode>) -> ViewNode {
    with_children(id, row_body(8.0, None, Some(Align::Center), fill_width()), children)
}

/// 两端对齐行：左侧信息、右侧操作（行宽撑满才能把两端推开）。
fn spread_row(id: impl Into<String>, children: Vec<ViewNode>) -> ViewNode {
    with_children(
        id,
        row_body(8.0, Some(Justify::SpaceBetween), Some(Align::Center), fill_width()),
        children,
    )
}

/// 左右分栏行：占满父容器剩余宽高，两栏各自滚动。
fn pane_row(id: impl Into<String>, left: ViewNode, right: ViewNode) -> ViewNode {
    with_children(id, row_body(10.0, None, Some(Align::Start), fill_both()), vec![left, right])
}

fn column(id: impl Into<String>, children: Vec<ViewNode>) -> ViewNode {
    column_gap(id, 8.0, children)
}

fn column_gap(id: impl Into<String>, gap: f32, children: Vec<ViewNode>) -> ViewNode {
    with_children(
        id,
        NodeBody::Column {
            gap: Some(gap),
            padding: None,
            cross_align: None,
            align: None,
            size: None,
        },
        children,
    )
}

/// 卡片盒：`accent` = 选中态强调边框，`tone` = 底色档位，`padding` 四边等距。
fn card_box(
    id: impl Into<String>,
    accent: bool,
    tone: CardTone,
    padding: f32,
    children: Vec<ViewNode>,
) -> ViewNode {
    with_children(
        id,
        NodeBody::Card {
            gap: Some(6.0),
            padding: Some(PaddingSpec::All(padding)),
            accent: Some(accent),
            tone: Some(tone),
            radius: Some(8.0),
            bordered: Some(true),
            align: None,
            size: None,
        },
        children,
    )
}

/// 列表行卡片：未选中走次级底，选中走强条底 + 强调边框。
fn row_card(id: impl Into<String>, selected: bool, children: Vec<ViewNode>) -> ViewNode {
    let tone = if selected { CardTone::AccentSoft } else { CardTone::Muted };
    card_box(id, selected, tone, 10.0, children)
}

/// 徽标：收缩卡片里的小号加粗文字，用来标注计数与状态。
fn chip(id: impl Into<String>, content: impl Into<String>, color: ColorToken) -> ViewNode {
    let id: String = id.into();
    let label = format!("{id}-label");
    with_children(
        id,
        NodeBody::Card {
            gap: None,
            padding: Some(PaddingSpec::All(4.0)),
            accent: Some(false),
            tone: Some(CardTone::Muted),
            radius: Some(6.0),
            bordered: Some(true),
            align: None,
            size: Some(BoxSize { width: Some(SizeSpec::Shrink), ..BoxSize::default() }),
        },
        vec![text_sized(label, content, SIZE_CAPTION, TextWeight::Medium, color)],
    )
}

/// 提示块：整行底色 + 语义色文字（宿主无 info/warn/error 档位，用卡片底色近似）。
fn notice(id: impl Into<String>, tone: CardTone, color: ColorToken, content: impl Into<String>) -> ViewNode {
    let id: String = id.into();
    let label = format!("{id}-label");
    card_box(id, false, tone, 10.0, vec![text_sized(label, content, 1.0, TextWeight::Normal, color)])
}

/// 空态：装饰图标 + 一句标题 + 一句指引（图标钮置灰且不声明事件，只当图标用）。
/// 居中靠纵向容器的 `cross_align`：卡片要先收缩出宽度余量才谈得上 `align`，这里铺满整栏更省事。
fn empty_state(id: impl Into<String>, icon: &str, heading: &str, hint: &str) -> ViewNode {
    let id: String = id.into();
    let children = vec![
        node(
            format!("{id}-icon"),
            NodeBody::IconButton {
                icon: icon.to_string(),
                tooltip: None,
                enabled: Some(false),
                size: Some(ControlSize::Lg),
                variant: None,
                hover_only: None,
            },
        ),
        text_sized(format!("{id}-title"), heading, SIZE_HEAD, TextWeight::Medium, ColorToken::TextSecondary),
        text_sized(format!("{id}-hint"), hint, SIZE_CAPTION, TextWeight::Normal, ColorToken::TextMuted),
    ];
    with_children(
        id,
        NodeBody::Column {
            gap: Some(4.0),
            padding: Some(PaddingSpec::All(18.0)),
            cross_align: Some(Align::Center),
            align: None,
            size: None,
        },
        children,
    )
}

fn scroll(id: impl Into<String>, children: Vec<ViewNode>) -> ViewNode {
    with_children(id, NodeBody::Scroll { direction: None, gap: Some(8.0), height: None }, children)
}

/// 固定高度的滚动区（日志：上面吃满剩余高度，这里收住定高）。
fn scroll_fixed(id: impl Into<String>, height: f32, gap: f32, children: Vec<ViewNode>) -> ViewNode {
    let mut n = node(id, NodeBody::Scroll { direction: None, gap: Some(gap), height: Some(height) });
    n.children = children;
    n
}

/// 按键复用控件态的滚动列表（持续追加条目的列表）。
fn list(id: impl Into<String>, gap: f32, children: Vec<ViewNode>) -> ViewNode {
    with_children(id, NodeBody::List { gap: Some(gap) }, children)
}

fn text_sized(
    id: impl Into<String>,
    content: impl Into<String>,
    size: f32,
    weight: TextWeight,
    color: ColorToken,
) -> ViewNode {
    node(
        id,
        NodeBody::Text {
            content: content.into(),
            size: Some(size),
            weight: Some(weight),
            color: Some(ColorSpec::Token(color)),
            align: None,
            family: None,
            wrap: None,
        },
    )
}

/// 面板主标题。
fn title(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    text_sized(id, content, SIZE_TITLE, TextWeight::SemiBold, ColorToken::Text)
}

/// 区块标题：一栏的头部（比正文略大、加粗）。
fn section_head(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    text_sized(id, content, SIZE_HEAD, TextWeight::Medium, ColorToken::Text)
}

fn text_secondary(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    text_sized(id, content, 1.0, TextWeight::Normal, ColorToken::TextSecondary)
}

fn text_muted(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    text_sized(id, content, 1.0, TextWeight::Normal, ColorToken::TextMuted)
}

/// 辅助行：比正文小一档的哑色说明。
fn caption(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    text_sized(id, content, SIZE_CAPTION, TextWeight::Normal, ColorToken::TextMuted)
}

fn text_tone(id: impl Into<String>, content: impl Into<String>, color: ColorToken) -> ViewNode {
    text_sized(id, content, 1.0, TextWeight::Normal, color)
}

/// 占满剩余宽度的文字行：宿主只在声明 `align` 时把文本铺到父宽，
/// 未声明则按内容收缩——长内容会把同行右端的控件顶出可视区。
fn text_fill(id: impl Into<String>, content: impl Into<String>, color: ColorToken) -> ViewNode {
    node(
        id,
        NodeBody::Text {
            content: content.into(),
            size: Some(1.0),
            weight: Some(TextWeight::Normal),
            color: Some(ColorSpec::Token(color)),
            align: Some(Align::Start),
            family: None,
            wrap: None,
        },
    )
}

/// 只读 Markdown 正文：由宿主用笔记同源解析器渲染，无编辑态也不回发事件。
fn markdown(id: impl Into<String>, content: impl Into<String>) -> ViewNode {
    node(
        id,
        NodeBody::Markdown {
            content: content.into(),
            size: None,
            weight: None,
            color: None,
            family: None,
        },
    )
}

fn button(
    id: impl Into<String>,
    label: impl Into<String>,
    variant: ButtonVariant,
    enabled: bool,
    events: Vec<EventKind>,
) -> ViewNode {
    button_sized(id, label, variant, enabled, None, events)
}

/// 主要操作钮：常规尺寸（与输入框同高，视觉权重高于紧凑钮）。
fn button_md(
    id: impl Into<String>,
    label: impl Into<String>,
    variant: ButtonVariant,
    enabled: bool,
    events: Vec<EventKind>,
) -> ViewNode {
    button_sized(id, label, variant, enabled, Some(ControlSize::Md), events)
}

fn button_sized(
    id: impl Into<String>,
    label: impl Into<String>,
    variant: ButtonVariant,
    enabled: bool,
    size: Option<ControlSize>,
    events: Vec<EventKind>,
) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::Button {
            label: label.into(),
            icon: None,
            variant: Some(variant),
            enabled: Some(enabled),
            size,
        },
    );
    n.events = events;
    n
}

/// 图标钮：`hover_only` 时未悬停也占位（列表行布局不跳）。
fn icon_button(
    id: impl Into<String>,
    icon: &str,
    tooltip: &str,
    enabled: bool,
    hover_only: bool,
    events: Vec<EventKind>,
) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::IconButton {
            icon: icon.into(),
            tooltip: (!tooltip.is_empty()).then(|| tooltip.to_string()),
            enabled: Some(enabled),
            size: None,
            variant: None,
            hover_only: hover_only.then_some(true),
        },
    );
    n.events = events;
    n
}

fn text_input(
    id: impl Into<String>,
    value: impl Into<String>,
    placeholder: &str,
    report: ReportMode,
    events: Vec<EventKind>,
) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::TextInput {
            value: value.into(),
            placeholder: (!placeholder.is_empty()).then(|| placeholder.to_string()),
            multiline: None,
            password: None,
            read_only: None,
            report: Some(report),
        },
    );
    n.events = events;
    n
}

/// 只读字段外观的文本（命令预览这类要整段选中复制的内容）。
fn read_only_field(id: impl Into<String>, value: impl Into<String>) -> ViewNode {
    node(
        id,
        NodeBody::TextInput {
            value: value.into(),
            placeholder: None,
            multiline: None,
            password: None,
            read_only: Some(true),
            report: None,
        },
    )
}

fn number_input(
    id: impl Into<String>,
    value: Option<f64>,
    min: Option<f64>,
    placeholder: &str,
    events: Vec<EventKind>,
) -> ViewNode {
    number_input_max(id, value, min, None, placeholder, events)
}

fn number_input_max(
    id: impl Into<String>,
    value: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
    placeholder: &str,
    events: Vec<EventKind>,
) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::NumberInput {
            value,
            min,
            max,
            step: Some(1.0),
            placeholder: (!placeholder.is_empty()).then(|| placeholder.to_string()),
            read_only: None,
            report: None,
        },
    );
    n.events = events;
    n
}

fn dropdown(
    id: impl Into<String>,
    options: Vec<String>,
    selected: Option<u32>,
    events: Vec<EventKind>,
) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::Dropdown { options, selected, placeholder: None, read_only: None },
    );
    n.events = events;
    n
}

/// 值列表 + 展示标签列表的下拉（值与标签一一对应，change 事件值 = 下标）。
fn dropdown_values(
    id: &str,
    values: &[&str],
    labels: &[&str],
    selected_value: &str,
) -> ViewNode {
    let selected = values
        .iter()
        .position(|v| *v == selected_value)
        .map(|i| i as u32)
        .unwrap_or(0);
    dropdown(id, labels.iter().map(|l| l.to_string()).collect(), Some(selected), vec![EventKind::Change])
}

fn checkbox(id: impl Into<String>, checked: bool, label: &str, events: Vec<EventKind>) -> ViewNode {
    let mut n = node(
        id,
        NodeBody::Checkbox { checked, label: (!label.is_empty()).then(|| label.to_string()), read_only: None },
    );
    n.events = events;
    n
}

/// 面板根：纵向排布；边界留白由宿主的面板外壳统一供给，插件侧不再叠加。
fn panel_root(id: &str, children: Vec<ViewNode>) -> ViewNode {
    column(id, children)
}

/// 「标签 + 控件」的字段列（标签为小号加粗次要色，控件在其下）。
fn field_row(id: &str, label: &str, control: ViewNode) -> ViewNode {
    column_gap(id, 4.0, vec![field_label(id, label), control])
}

/// 字段标签（与 [`field_row`] 同名规则：`{字段 id}-label`）。
fn field_label(id: impl Into<String>, label: &str) -> ViewNode {
    let id = id.into();
    text_sized(format!("{id}-label"), label, SIZE_CAPTION, TextWeight::Medium, ColorToken::TextSecondary)
}

/// 千分位：8489648 → 8,489,648。
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, byte) in digits.as_bytes().iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

/// 体积徽标文案（探不到体积时给可读占位，不留空徽标）。
fn size_label(bytes: Option<u64>) -> String {
    if bytes.is_some() {
        crate::modelscan::format_size(bytes)
    } else {
        "体积未知".to_string()
    }
}

// ---------- 按面构建 ----------

/// 按面构建界面树；未知面返回 None。
pub fn build(core: &Core, surface: &str) -> Option<ViewNode> {
    match surface {
        SURFACE_CONSOLE => Some(console::console(core)),
        SURFACE_LIBRARY => Some(library::library(core)),
        SURFACE_SETTINGS => Some(settings::settings_panel(core)),
        _ => None,
    }
}

/// 已装模型列表的可见项下标（按名称过滤后的 `folders` 下标，界面与事件路由同源）。
pub fn visible_folders(core: &Core) -> Vec<usize> {
    let needle = core.scan.filter.trim().to_lowercase();
    core
        .scan
        .folders
        .iter()
        .enumerate()
        .filter(|(_, folder)| {
            needle.is_empty() || folder.rel.to_lowercase().contains(&needle)
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests;
