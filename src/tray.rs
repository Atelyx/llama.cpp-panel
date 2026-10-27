//! 系统托盘：按运行状态提交「状态 + 启停」条目，点击回转面板同一套启停入口。
//! 宿主托盘只有 Windows 实现：`tray.setItems` 回可读错误即置降级位，之后不再提交
//! （按失败降级、不重试，也不把托盘当必需入口）。状态变迁点分散在启停 / 待命 / 探测
//! 多个模块，按拍巡检比逐点挂调用可靠；只在条目签名变化时整发整换。

use std::sync::Arc;
use std::time::Duration;

use atelyx_plugin_protocol::method::TrayItemSpec;

use crate::app::{App, RunPhase};
use crate::controller;
use crate::settings;

/// 巡检节拍：托盘是状态镜像，迟一拍可接受。
const TICK: Duration = Duration::from_secs(1);

/// 条目 id（点击路由与签名共用）。
const ITEM_STATUS: &str = "status";
const ITEM_START: &str = "start";
const ITEM_STOP: &str = "stop";
const ITEM_COPY: &str = "copy";

/// 常驻巡检任务：立即同步一次，之后按拍同步。
pub fn spawn(app: &Arc<App>) {
    let app = Arc::clone(app);
    tokio::spawn(async move {
        loop {
            sync(&app).await;
            tokio::time::sleep(TICK).await;
        }
    });
}

/// 按当前运行状态整发整换托盘条目；平台不支持时降级为不再提交。
pub async fn sync(app: &Arc<App>) {
    let items = {
        let mut core = app.core();
        if !core.tray_supported {
            return;
        }
        let phase = core.run_phase();
        let addr = settings::base_url(&core.settings);
        let signature = format!("{}|{addr}", phase.label());
        if core.tray_signature == signature {
            return;
        }
        core.tray_signature = signature;
        items(phase, &addr)
    };
    if let Err(err) = app.ctx.tray_set_items(items).await {
        app.core().tray_supported = false;
        eprintln!("托盘条目提交失败，降级为不使用托盘：{err}");
    }
}

/// 托盘点击：启停与面板按钮同一入口；复制地址直接写剪贴板。
pub async fn on_click(app: &Arc<App>, id: &str) {
    match id {
        ITEM_START => {
            let app = Arc::clone(app);
            tokio::spawn(async move { controller::start(&app).await });
        }
        ITEM_STOP => {
            let app = Arc::clone(app);
            tokio::spawn(async move { controller::stop(&app).await });
        }
        ITEM_COPY => {
            let url = settings::base_url(&app.core().settings);
            crate::app::copy_text(app, url, "服务地址").await;
        }
        _ => {}
    }
}

/// 条目：状态行（置灰只读）+ 与状态互斥的启停项 + 复制服务地址。
fn items(phase: RunPhase, addr: &str) -> Vec<TrayItemSpec> {
    let stopping = matches!(phase, RunPhase::Running | RunPhase::Starting);
    vec![
        TrayItemSpec {
            id: ITEM_STATUS.into(),
            label: format!("llama-server：{} · {addr}", phase.label()),
            enabled: Some(false),
        },
        TrayItemSpec {
            id: if stopping { ITEM_STOP } else { ITEM_START }.into(),
            label: if stopping { "停止 llama-server" } else { "启动 llama-server" }.into(),
            enabled: Some(true),
        },
        TrayItemSpec {
            id: ITEM_COPY.into(),
            label: "复制服务地址".into(),
            enabled: Some(true),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_items_mirror_phase() {
        let running = items(RunPhase::Running, "http://127.0.0.1:8080");
        assert_eq!(running.len(), 3);
        assert_eq!(running[0].enabled, Some(false), "状态行置灰只读");
        assert!(running[0].label.contains("运行中"));
        assert_eq!(running[1].id, ITEM_STOP);
        assert_eq!(running[2].id, ITEM_COPY, "复制服务地址常驻");
        assert_eq!(running[2].enabled, Some(true));

        let stopped = items(RunPhase::Stopped, "http://127.0.0.1:8080");
        assert_eq!(stopped[1].id, ITEM_START);
        assert!(stopped[0].label.contains("已停止"));

        // 启动中同样只给停止：重复发起启动会被控制器挡掉，给出去反而是哑钮
        assert_eq!(items(RunPhase::Starting, "")[1].id, ITEM_STOP);
    }
}
