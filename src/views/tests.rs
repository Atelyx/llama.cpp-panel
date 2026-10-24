//! 界面树构建的单元测试：树结构合法性、状态组合下的投影，以及过滤与翻页判定。

use super::*;
use atelyx_plugin_protocol::view::TextFamily;
use crate::app::SURFACE_CONSOLE;
use crate::download::DownloadStatus;
use crate::modelscan::{ModelEntry, ModelFolder};

fn test_core() -> Core {
    Core {
        settings: crate::settings::LlamaSettings::default(),
        attached: Default::default(),
        probe_online: false,
        probe_suspended: false,
        host: Default::default(),
        lib: Default::default(),
        scan: Default::default(),
        dl: crate::download::DownloadTask::idle(),
        tray_supported: true,
        tray_signature: String::new(),
    }
}

fn summary(repo: &str) -> crate::hf::ModelSummary {
    crate::hf::ModelSummary {
        repo: repo.into(),
        downloads: 8_489_648,
        likes: 2_167,
        updated_at: "2026-03-02T00:00:00Z".into(),
    }
}

fn folder(rel: &str) -> ModelFolder {
    ModelFolder {
        rel: rel.into(),
        full: format!("/models/{rel}"),
        quants: vec![ModelEntry { name: format!("{rel}/q5.gguf"), size: Some(5_400_000_000) }],
        mms: vec![ModelEntry { name: format!("{rel}/mmproj.gguf"), size: None }],
    }
}

#[test]
fn thousands_groups_digits_in_threes() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(8_489_648), "8,489,648");
    assert_eq!(thousands(1_234_567_890), "1,234,567,890");
}

#[test]
fn trees_are_valid_per_surface() {
    let core = test_core();
    for surface in [SURFACE_CONSOLE, SURFACE_LIBRARY, SURFACE_SETTINGS] {
        let tree = build(&core, surface).unwrap_or_else(|| panic!("缺面：{surface}"));
        tree.validate().unwrap_or_else(|err| panic!("{surface}: {err}"));
    }
}

#[test]
fn library_tree_survives_state_combinations() {
    let mut core = test_core();
    // 空态（尚未检索）
    build(&core, SURFACE_LIBRARY).unwrap().validate().unwrap();
    // 有结果 + 有详情 + 下载中
    core.lib.results.push(summary("Qwen/Qwen3.5-9B"));
    core.lib.detail = Some(crate::hf::RepoDetail {
        summary: summary("Qwen/Qwen3.5-9B"),
        gated: true,
        files: vec![
            crate::hf::RepoFile { name: "a.gguf".into(), size: Some(123) },
            crate::hf::RepoFile { name: "very/long/path/b.gguf".into(), size: None },
        ],
        readme: "简介".into(),
    });
    core.dl.status = DownloadStatus::Downloading;
    core.dl.total_bytes = Some(1000);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    tree.validate().unwrap();
    assert!(tree.find("rcard0").is_some());
    assert!(tree.find("f0").is_some());
    assert!(tree.find("detail-title").is_some());
    assert!(tree.find("dl-bar").is_some(), "下载中给进度条");
    assert!(
        matches!(tree.find("detail-readme").map(|n| &n.body), Some(NodeBody::Markdown { .. })),
        "模型简介交宿主按 Markdown 渲染"
    );
}

#[test]
fn library_results_declare_scroll_for_auto_paging() {
    let mut core = test_core();
    core.lib.results.push(summary("Qwen/Qwen3.5-9B"));
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    let pane = tree.find("lib-results").expect("结果列表节点");
    assert!(pane.events.contains(&EventKind::Scroll), "滚到底自动翻页要靠滚动位置");
    assert!(tree.find("more").is_some(), "未到底时保留手动翻页入口");

    core.lib.exhausted = true;
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find("more").is_none(), "到底后不再挂「加载更多」");
}

#[test]
fn console_tree_survives_state_combinations() {
    let mut core = test_core();
    // 未扫描：模型栏与参数栏都退成空态，日志区给空态
    build(&core, SURFACE_CONSOLE).unwrap().validate().unwrap();

    core.scan.folders = vec![folder("briannlm/mlTernary-Bonsai-2-27B"), folder("qwen/Qwen3-14B")];
    core.settings.model_file = "/models/briannlm/mlTernary-Bonsai-2-27B/q5.gguf".into();
    core.host.start_line = "llama-server --model x".into();
    core.host.logs.push_back(crate::app::LogLine {
        seq: 1,
        time: "10:00:00".into(),
        text: "listening on 127.0.0.1:8080".into(),
        tone: crate::app::LogTone::Normal,
        ansi: None,
    });
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    tree.validate().unwrap();
    assert!(tree.find("fcard0").is_some());
    assert!(tree.find("quant").is_some(), "选中模型后出参数表单");
    assert!(tree.find("logs-cmd").is_some(), "命令预览始终可读");
    assert!(tree.find("log1").is_some());
    assert!(tree.find("logs-scroll").is_some());
}

#[test]
fn library_direct_input_declares_change() {
    let core = test_core();
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    let input = tree.find("dlinput").expect("直链下载输入框");
    // 未声明 Change 即不上行：输入框的值永远回不来，「下载」钮会成哑钮
    assert!(input.events.contains(&EventKind::Change), "直链输入改动要回写");
    assert!(input.events.contains(&EventKind::Submit), "回车即下载");
}

#[test]
fn model_and_result_cards_are_clickable_as_a_whole() {
    let mut core = test_core();
    core.scan.folders = vec![folder("qwen/Qwen3-14B")];
    core.lib.results.push(summary("Qwen/Qwen3.5-9B"));
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    let card = tree.find("fcard0").expect("模型卡片");
    assert!(card.events.contains(&EventKind::Click), "整卡承接点击");
    assert!(card.interaction.is_some(), "悬停与选中态由 interaction 档位承担");
    assert!(tree.find("fcard0-name").is_some(), "名称是普通文字");
    assert!(tree.find("pick0").is_none(), "卡片内不再有独立按钮");

    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    let card = tree.find("rcard0").expect("结果卡片");
    assert!(card.events.contains(&EventKind::Click), "整卡承接点击");
    assert!(tree.find("rcard0-name").is_some());
    assert!(tree.find("r0").is_none());
}

#[test]
fn file_rows_are_operable_without_hover() {
    let mut core = test_core();
    core.lib.detail = Some(crate::hf::RepoDetail {
        summary: summary("Qwen/Qwen3.5-9B"),
        gated: false,
        files: vec![crate::hf::RepoFile { name: "a.gguf".into(), size: Some(1) }],
        readme: String::new(),
    });
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    let row = tree.find("frow0").expect("文件行节点");
    assert!(row.events.contains(&EventKind::SecondaryClick), "右键即下载");
    assert!(row.events.contains(&EventKind::Key), "焦点回车/空格即下载");
    assert!(row.interaction.is_some(), "焦点框让键盘用户看见当前行");
    let icon = tree.find("f0").expect("下载钮");
    assert!(
        !matches!(&icon.body, NodeBody::IconButton { hover_only: Some(true), .. }),
        "下载钮常显，不依赖悬停"
    );
    assert!(
        matches!(&tree.find("fname0").unwrap().body, NodeBody::Text { max_lines: Some(2), .. }),
        "长文件名两行收口"
    );
}

#[test]
fn number_params_report_on_commit() {
    let mut core = test_core();
    core.scan.folders = vec![folder("qwen/Qwen3-14B")];
    core.settings.model_file = "/models/qwen/Qwen3-14B/q5.gguf".into();
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    for id in ["ngl", "ctx", "threads", "parallel"] {
        let node = tree.find(id).unwrap_or_else(|| panic!("缺参数框 {id}"));
        assert!(
            matches!(&node.body, NodeBody::NumberInput { report: Some(ReportMode::Commit), .. }),
            "{id} 回车/失焦才上报"
        );
    }
}

#[test]
fn console_offers_clipboard_copy_entries() {
    let mut core = test_core();
    core.host.start_line = "llama-server --model x".into();
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    for id in ["copycmd", "copyaddr"] {
        let node = tree.find(id).unwrap_or_else(|| panic!("缺复制钮 {id}"));
        assert!(node.events.contains(&EventKind::Click));
    }
}

#[test]
fn log_rows_use_mono_family() {
    let mut core = test_core();
    core.host.logs.push_back(crate::app::LogLine {
        seq: 1,
        time: "10:00:00".into(),
        text: "hello".into(),
        tone: crate::app::LogTone::Normal,
        ansi: None,
    });
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    let row = tree.find("log1").expect("日志行");
    assert!(
        matches!(&row.body, NodeBody::Text { family: Some(TextFamily::Mono), .. }),
        "日志行等宽（终端样式）"
    );
}

#[test]
fn download_card_gates_actions_by_state() {
    let mut core = test_core();
    core.lib.last_download = Some(crate::download::DownloadRequest {
        input: "owner/repo/a.gguf".into(),
        dir: "/models".into(),
        mirror_id: "hf-mirror".into(),
    });

    // 下载中：给暂停、不给「关闭」（任务未落定前收掉卡片只会藏掉进度）
    core.dl.status = DownloadStatus::Downloading;
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find("dlpause").is_some());
    assert!(tree.find("dldismiss").is_none());

    // 失败：只有目标重名才挂「改名保留」，其余失败换名也下不来
    core.dl.status = DownloadStatus::Error;
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find("dlrename").is_none(), "非重名失败不给改名入口");
    core.lib.dl_conflict = true;
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find("dlrename").is_some());
    assert!(tree.find("dldismiss").is_some(), "落定后可关卡片");
}

#[test]
fn console_filter_narrows_cards_without_renumbering_targets() {
    let mut core = test_core();
    core.scan.folders = vec![folder("briannlm/mlTernary-Bonsai-2-27B"), folder("qwen/Qwen3-14B")];
    core.scan.filter = "qwen3".into();
    let visible = visible_folders(&core);
    assert_eq!(visible, vec![1], "过滤只影响显示，卡片仍指向真实下标");
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    tree.validate().unwrap();
    assert!(tree.find("fcard1").is_some());
    assert!(tree.find("fcard0").is_none());
}

#[test]
fn visible_folders_matches_substring_case_insensitively() {
    let mut core = test_core();
    core.scan.folders = vec![folder("A/B"), folder("a/c"), folder("D/E")];
    core.scan.filter = "a/".into();
    assert_eq!(visible_folders(&core), vec![0, 1]);
    core.scan.filter = "  ".into();
    assert_eq!(visible_folders(&core), vec![0, 1, 2], "空白过滤词不参与筛选");
}
