//! 界面树构建的单元测试：树结构合法性、状态组合下的投影，以及过滤与翻页判定。

use super::*;
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
    assert!(tree.find("r0").is_some());
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
    });
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    tree.validate().unwrap();
    assert!(tree.find("pick0").is_some());
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
    assert!(input.events.contains(&EventKind::Change), "直链输入要逐键回写");
    assert!(input.events.contains(&EventKind::Submit), "回车即下载");
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
    assert!(tree.find("pick1").is_some());
    assert!(tree.find("pick0").is_none());
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
