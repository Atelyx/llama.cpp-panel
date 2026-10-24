//! 插件入口：握手、注册贡献点与常驻任务装配。
//! 插件进程生命周期归宿主：连接结束（宿主正常关闭）即退出；协议违规以非零码退出（失败可见）。

mod app;
mod controller;
mod download;
mod hf;
mod launcher;
mod logfmt;
mod mirrors;
mod modelscan;
mod probe;
mod settings;
mod tray;
mod views;
mod watcher;

use std::sync::{Arc, OnceLock};

use atelyx_plugin_protocol::handshake::PluginInfo;
use atelyx_plugin_protocol::method::{
    ProcessEventParams, SurfaceAttachedParams, SurfaceDetachedParams, TrayClickParams,
    ViewEventParams,
};
use atelyx_plugin_sdk::handler::PluginHandler;
use atelyx_plugin_sdk::Context;

use crate::app::App;

/// 插件通知处理：App 在 on_ready 拿到 Context 后才装配（设置读取需要能力面）。
/// SDK 顺序约定允许宿主通知先于 on_ready 执行，未装配时丢弃——挂载通知在贡献注册后才
/// 会出现，实际不存在丢业务事件的路径。
struct Handler {
    app: OnceLock<Arc<App>>,
}

impl Handler {
    fn with_app(&self) -> Option<Arc<App>> {
        self.app.get().cloned()
    }
}

#[async_trait::async_trait]
impl PluginHandler for Handler {
    async fn on_ready(&self, ctx: &Context) {
        let settings = settings::load(ctx).await;
        let app = app::build_app(ctx.clone(), settings);
        let _ = self.app.set(Arc::clone(&app));
        app::initialize(&app).await;
    }

    async fn on_view_event(&self, _ctx: &Context, params: ViewEventParams) {
        if let Some(app) = self.with_app() {
            tokio::spawn(async move { app::handle_view_event(&app, params).await });
        }
    }

    async fn on_surface_attached(&self, _ctx: &Context, params: SurfaceAttachedParams) {
        if let Some(app) = self.with_app() {
            app::on_surface(&app, &params.surface_id, true);
        }
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if let Some(app) = self.with_app() {
            app::on_surface(&app, &params.surface_id, false);
        }
    }

    async fn on_tray_click(&self, _ctx: &Context, params: TrayClickParams) {
        // 启停含就绪等待与进程收尾：起任务执行，不堵托盘巡检
        if let Some(app) = self.with_app() {
            let id = params.id;
            tokio::spawn(async move { crate::tray::on_click(&app, &id).await });
        }
    }

    async fn on_process_event(&self, _ctx: &Context, params: ProcessEventParams) {
        // 只入队：宿主通知各自起任务，交给单消费者按到达顺序逐行落地
        if let Some(app) = self.with_app() {
            if let Err(err) = app.process_tx.send(params) {
                eprintln!("进程事件入队失败（消费者已退出），该行丢失：{err}");
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let plugin = PluginInfo {
        id: crate::app::PLUGIN_ID.into(),
        name: crate::app::PLUGIN_NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    // 连接建立 → on_ready 装配 → 连接结束（宿主停用/退出）→ 进程退出。
    if let Err(err) = atelyx_plugin_sdk::run_stdio(plugin, Handler { app: OnceLock::new() }).await {
        eprintln!("连接异常结束：{err}");
        std::process::exit(1);
    }
}
