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
        dls: Vec::new(),
        tray_supported: true,
        tray_signature: String::new(),
    }
}

/// 构造一条指定状态的下载任务（id 自增保证节点 id 唯一）。
fn dl_task(core: &mut Core, status: DownloadStatus, conflict: bool) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    core.dls.push(crate::download::DownloadTask {
        id,
        status,
        repo: "owner/repo".into(),
        file: "a.gguf".into(),
        target: "/models/owner/repo/a.gguf".into(),
        url: "https://example/a.gguf".into(),
        total_bytes: Some(1000),
        bytes: 500,
        conflict,
        status_text: "测试任务".into(),
    });
    id
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
    let running_id = dl_task(&mut core, DownloadStatus::Downloading, false);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    tree.validate().unwrap();
    assert!(tree.find("rcard0").is_some());
    assert!(tree.find("f0").is_some());
    assert!(tree.find("detail-title").is_some());
    assert!(tree.find(&format!("dl-bar{running_id}")).is_some(), "下载中给进度条");
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

    // 下载中：给暂停、不给「移除」（任务未落定前收掉卡片只会藏掉进度）
    let running = dl_task(&mut core, DownloadStatus::Downloading, false);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find(&format!("dlpause{running}")).is_some());
    assert!(tree.find(&format!("dlremove{running}")).is_none());
    assert!(tree.find("dlclear").is_none(), "没有已落定任务时不给清空");

    // 失败：只有目标重名才挂「改名保留」，其余失败换名也下不来
    let failed = dl_task(&mut core, DownloadStatus::Error, false);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find(&format!("dlrename{failed}")).is_none(), "非重名失败不给改名入口");
    assert!(tree.find("dlclear").is_some(), "有已落定任务时给清空");

    let mut core = test_core();
    let conflicted = dl_task(&mut core, DownloadStatus::Error, true);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find(&format!("dlrename{conflicted}")).is_some());
    assert!(tree.find(&format!("dlremove{conflicted}")).is_some(), "落定后可移除卡片");

    // 暂停给「继续」；排队中只等位，不给启停钮
    let mut core = test_core();
    let paused = dl_task(&mut core, DownloadStatus::Paused, false);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find(&format!("dlresume{paused}")).is_some());
    let mut core = test_core();
    let queued = dl_task(&mut core, DownloadStatus::Queued, false);
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find(&format!("dlpause{queued}")).is_none());
    assert!(tree.find(&format!("dlremove{queued}")).is_some(), "排队中可撤下");
}

#[test]
fn detail_marks_local_files_and_shard_groups() {
    let mut core = test_core();
    core.settings.models_dir = "/models".into();
    core.scan.folders.push(crate::modelscan::ModelFolder {
        rel: "Qwen/Qwen3.5-9B".into(),
        full: "/models/Qwen/Qwen3.5-9B".into(),
        quants: vec![crate::modelscan::ModelEntry { name: "Qwen/Qwen3.5-9B/a.gguf".into(), size: Some(1) }],
        mms: vec![],
    });
    core.lib.detail = Some(crate::hf::RepoDetail {
        summary: summary("Qwen/Qwen3.5-9B"),
        gated: false,
        files: vec![
            crate::hf::RepoFile { name: "a.gguf".into(), size: Some(1) },
            crate::hf::RepoFile { name: "b-00001-of-00002.gguf".into(), size: Some(1) },
            crate::hf::RepoFile { name: "b-00002-of-00002.gguf".into(), size: Some(1) },
        ],
        readme: String::new(),
    });
    let tree = build(&core, SURFACE_LIBRARY).unwrap();
    assert!(tree.find("fown0").is_some(), "本地已有的文件标「已在库」");
    assert!(tree.find("fown1").is_none(), "本地没有的文件不标");
    assert!(tree.find("shards0").is_some(), "识别分片组并给整组下载入口");
}

#[test]
fn console_offers_restart_and_secret_api_key() {
    let mut core = test_core();
    core.scan.folders = vec![folder("qwen/Qwen3-14B")];
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    let restart = tree.find("restart").expect("重启按钮常驻");
    assert!(
        matches!(&restart.body, NodeBody::Button { enabled: Some(false), .. }),
        "未运行时重启不可点"
    );

    core.settings.model_file = "/models/qwen/Qwen3-14B/q5.gguf".into();
    core.host.pid = Some(7);
    let tree = build(&core, SURFACE_CONSOLE).unwrap();
    let restart = tree.find("restart").unwrap();
    assert!(
        matches!(&restart.body, NodeBody::Button { enabled: Some(true), .. }),
        "运行中给一键重启"
    );
    let input = tree.find("apikey").expect("API 密钥输入框");
    assert!(
        matches!(&input.body, NodeBody::TextInput { password: Some(true), report: Some(ReportMode::Commit), .. }),
        "密钥框密文渲染、回车/失焦才上报"
    );
    assert!(input.events.contains(&EventKind::Change));
}

#[test]
fn run_phase_distinguishes_crash_from_manual_stop() {
    let mut core = test_core();
    assert_eq!(core.run_phase(), crate::app::RunPhase::Stopped);
    core.host.exited = true;
    assert_eq!(core.run_phase(), crate::app::RunPhase::Exited, "无人叫停的进程退出与手动停止分开显示");
    core.host.error = "boom".into();
    assert_eq!(core.run_phase(), crate::app::RunPhase::Failed, "宿主级错误优先于已退出");
    core.host.watch = true;
    assert_eq!(core.run_phase(), crate::app::RunPhase::Watching, "待命优先于失败与已退出");
    core.host.pid = Some(7);
    assert_eq!(core.run_phase(), crate::app::RunPhase::Running);
    core.host.starting = true;
    assert_eq!(core.run_phase(), crate::app::RunPhase::Starting);
}

#[test]
fn option_tables_match_settings_whitelists() {
    // 界面选项与设置侧合法值表不一致时，已存的合法值会显示成「默认」而命令仍带旗标
    for value in CACHE_K_OPTIONS.iter().filter(|v| !v.is_empty()) {
        assert!(crate::settings::KV_K_TYPES.contains(value), "KV 缓存选项 {value} 不在合法值表");
    }
    for value in LOAD_MODE_OPTIONS.iter().filter(|v| !v.is_empty()) {
        assert!(crate::settings::LOAD_MODES.contains(value), "载入模式选项 {value} 不在合法值表");
    }
    for value in crate::settings::LOAD_MODES {
        assert!(LOAD_MODE_OPTIONS.contains(&value), "合法载入模式 {value} 缺界面选项");
    }
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
