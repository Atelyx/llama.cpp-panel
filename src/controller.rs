//! 进程托管状态中心：llama-server 启停、就绪判定、日志与待命裁决编排。
//! 进程（本插件启动与否）与连接（网络可达）是两条信息：用户自起的服务连接通但不归本插件管，
//! 界面要同时呈现。待命裁决唯一入口是 [`evaluate_watcher`]：装载后的首次裁决、进程启停、
//! 设置变更、探测变化都经它收敛。

use std::sync::Arc;
use std::time::Duration;

use atelyx_plugin_protocol::method::{NotificationLevel, ProcessEvent, ProcessEventParams};

use crate::app::{App, LogTone};
use crate::launcher::{
    describe_start_command, find_server_executable, missing_executable_message, start_command,
};
use crate::probe::probe_once;
use crate::settings::LlamaSettings;

/// 加载大模型会慢，给足时间但不无限等。
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// 就绪轮询间隔。
const READY_POLL: Duration = Duration::from_millis(1500);
/// 心跳间隔：加载耗时数十秒，定期留一行让用户知道还在等。
const HEARTBEAT: Duration = Duration::from_secs(5);
/// 错误输出可能极长，进通知与快照前截断。
const ERROR_DETAIL_MAX: usize = 200;
/// 单行输出上限：进程可能吐出不换行的一大段，截断后不把大块文本塞进界面树。
const OUTPUT_LINE_MAX: usize = 4096;

/// 启动并等待服务就绪。返回 false 有两种含义——已有进程/正在启动、启动失败：后者写 error，前者不写。
pub async fn start(app: &Arc<App>) -> bool {
    {
        let mut core = app.core();
        if core.host().starting || core.host().pid.is_some() {
            return false;
        }
        // starting 先行置位：手动启动与自动触发并发时，第二个 start 会被这个挡住
        let host = core.host_mut();
        host.starting = true;
        host.watch_paused = false;
        host.watch_pause_notified = false;
        host.died = String::new();
        host.last_stderr = String::new();
        host.notified = false;
        host.cancelled = false;
    }
    app.invalidate();

    // 占位监听持有端口时 llama-server 绑不上，启动前必须先让它退出
    let was_armed = app.watcher.disarm().await;
    app.core().probe_suspended = false;
    if was_armed && app.core().host().watch {
        let mut core = app.core();
        core.host_mut().watch = false;
        drop(core);
        app.log_status("已退出自动启动待命");
    }
    app.invalidate();

    let settings = app.core().settings.clone();
    let located = find_server_executable(&settings.server_dir)
        .and_then(|found| found.ok_or_else(|| missing_executable_message(&settings.server_dir)));
    let server_exe = match located {
        Ok(exe) => exe,
        Err(message) => {
            fail_start(app, message);
            evaluate_watcher(app).await;
            return false;
        }
    };
    // 定位可执行文件的窗口里用户叫停：不再拉起进程
    // （锁守卫不得跨 await：先在独立作用域内落状态，await 移出作用域）
    let cancelled_now = app.core().host().cancelled;
    if cancelled_now {
        {
            let mut core = app.core();
            core.host_mut().starting = false;
        }
        app.invalidate();
        evaluate_watcher(app).await;
        return false;
    }
    let line = describe_start_command(&settings, &server_exe);
    let (program, args, cwd) = match start_command(&settings, &server_exe) {
        Ok(parts) => parts,
        Err(message) => {
            fail_start(app, message);
            evaluate_watcher(app).await;
            return false;
        }
    };
    // 日志不清屏：跨多次启动保留现场，只有用户手动清屏才清
    {
        let mut core = app.core();
        let host = core.host_mut();
        host.error = String::new();
        host.start_line = line;
    }
    app.invalidate();

    let spawn = app.ctx.spawn(&program, &args, cwd.as_deref(), None).await;
    match spawn {
        Ok(pid) => {
            // 句柄先落地再裁决：消费者要凭它认领 spawn 应答之前暂存的那批事件，
            // 秒退进程（参数错、缺运行库）的错误输出与退出事件正在那一批里
            {
                let mut core = app.core();
                let host = core.host_mut();
                host.pid = Some(pid);
                host.running = true;
            }
            app.invalidate();
            let (died, cancelled_now) = {
                let core = app.core();
                let host = core.host();
                (host.died.clone(), host.cancelled)
            };
            if !died.is_empty() {
                fail_start(app, String::new());
                evaluate_watcher(app).await;
                return false;
            }
            // 定位/拉起期间用户叫停：刚落地的进程不能留着（这次退出不算失败）
            if cancelled_now {
                let _ = app.ctx.kill(pid).await;
                {
                    let mut core = app.core();
                    let host = core.host_mut();
                    host.starting = false;
                    host.pid = None;
                }
                app.log_status("已取消启动");
                app.invalidate();
                evaluate_watcher(app).await;
                return false;
            }
        }
        Err(err) => {
            fail_start(app, err.to_string());
            evaluate_watcher(app).await;
            return false;
        }
    }
    app.invalidate();

    let ready = wait_ready(app).await;
    if ready {
        app.core().host_mut().starting = false;
    } else {
        fail_start(app, String::new());
    }
    app.invalidate();
    evaluate_watcher(app).await;
    ready
}

/// 停止本插件启动的进程（结束整棵进程树，不给服务留孤儿）。待命不随停止解除：进程退出后自动重新待命。
pub async fn stop(app: &Arc<App>) {
    let pid = {
        let mut core = app.core();
        // 等就绪期间用户叫停：别让 start() 把这次退出当成启动失败
        if core.host().starting {
            core.host_mut().cancelled = true;
        }
        core.host().pid
    };
    let Some(pid) = pid else {
        let mut core = app.core();
        let host = core.host_mut();
        host.running = false;
        host.starting = false;
        drop(core);
        app.invalidate();
        return;
    };
    match app.ctx.kill(pid).await {
        Ok(()) => app.log_status("已结束 llama-server 进程"),
        Err(err) => {
            app.core().host_mut().error = err.to_string();
        }
    }
    // 退出事件稍后到达：pid 已清空即被忽略，状态在此落定
    let mut core = app.core();
    let host = core.host_mut();
    host.pid = None;
    host.running = false;
    host.starting = false;
    drop(core);
    app.invalidate();
}

/// 就绪判定用「接口通」而不是「日志出现某句话」：接口可用才是真的可用。
async fn wait_ready(app: &Arc<App>) -> bool {
    let started = std::time::Instant::now();
    let mut last_heartbeat = started;
    loop {
        if !app.core().host().died.is_empty() {
            return false; // 进程已退出就等不到服务，立刻定论
        }
        if app.core().host().cancelled {
            return false; // 用户叫停：不算启动失败
        }
        let settings = app.core().settings.clone();
        if probe_once(&app.client, &settings).await {
            return true;
        }
        if !app.core().host().died.is_empty() {
            return false;
        }
        let elapsed = started.elapsed();
        if elapsed >= READY_TIMEOUT {
            return false;
        }
        if last_heartbeat.elapsed() >= HEARTBEAT {
            last_heartbeat = std::time::Instant::now();
            app.log_status(format!("等待服务就绪…（{}s）", elapsed.as_secs()));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// 启动失败收口：写原因并弹通知。主动停止同样以进程退出收场，但那不算失败。
fn fail_start(app: &Arc<App>, message: String) {
    let detail = if message.is_empty() {
        let core = app.core();
        let host = core.host();
        if !host.died.is_empty() {
            host.died.clone()
        } else {
            format!("启动后 {}s 内服务未就绪", READY_TIMEOUT.as_secs())
        }
    } else {
        message
    };
    {
        let mut core = app.core();
        if core.host().cancelled {
            core.host_mut().starting = false;
            return;
        }
        let host = core.host_mut();
        host.starting = false;
        host.error = detail.clone();
        // 启动失败后暂缓自动启动：原因持久，重试无意义
        host.watch_paused = true;
    }
    app.invalidate();
    notify_failure(app, &detail);
}

/// 启动失败弹一条宿主通知：面板不可见时也能知道结果与原因。每次启动至多一条。
fn notify_failure(app: &Arc<App>, message: &str) {
    let already = app.core().host().notified;
    if already {
        return;
    }
    let mut core = app.core();
    core.host_mut().notified = true;
    drop(core);
    let ctx = app.ctx.clone();
    let message = message.to_string();
    tokio::spawn(async move {
        let _ = ctx
            .notify("llama-server 启动失败", Some(&message), NotificationLevel::Error)
            .await;
    });
}

/// 宿主把 spawn 应答之前到达的事件按序暂存、定下 pid 后统一补投，插件侧这批事件因此早于句柄落地。
/// 秒退进程的 stderr 与退出事件全在这一批里，直接丢弃会让启动失败无声无息（就绪等待白等满
/// [`READY_TIMEOUT`]），故先等 `start()` 把句柄写下；句柄落地前叫停或启动失败即放弃等待。
const HANDLE_WAIT_ROUNDS: usize = 80;
const HANDLE_WAIT: Duration = Duration::from_millis(25);

async fn await_spawn_handle(app: &Arc<App>) -> Option<u32> {
    for _ in 0..HANDLE_WAIT_ROUNDS {
        let settled = {
            let core = app.core();
            let host = core.host();
            host.pid.is_some() || !host.starting
        };
        if settled {
            break;
        }
        tokio::time::sleep(HANDLE_WAIT).await;
    }
    app.core().host().pid
}

/// 宿主托管进程事件：日志、退出定论与待命重裁决。队列消费者按到达顺序调用，行序与退出时刻才可靠。
pub async fn on_process_event(app: &Arc<App>, params: ProcessEventParams) {
    let current_pid = await_spawn_handle(app).await;
    if Some(params.pid) != current_pid {
        return; // 上一代进程的迟到事件（已被停止/取代）一律忽略
    }
    match params.event {
        ProcessEvent::Stdout { data } => {
            if let Some(line) = output_line(&data) {
                app.log_line(line, LogTone::Normal);
            }
        }
        ProcessEvent::Stderr { data } => {
            // llama-server 把日志写 stderr：逐行定色，最后一条非空行就是最接近现场的失败信息
            if let Some(line) = output_line(&data) {
                let tone = stderr_tone(&line);
                app.core().host_mut().last_stderr = line.trim().to_string();
                app.log_line(format!("! {line}"), tone);
            }
        }
        ProcessEvent::Terminated { code } => {
            let line = {
                let mut core = app.core();
                let host = core.host_mut();
                host.pid = None;
                host.running = false;
                let detail = if host.last_stderr.is_empty() {
                    String::new()
                } else {
                    format!("：{}", truncate(&host.last_stderr, ERROR_DETAIL_MAX))
                };
                let exit = match code {
                    Some(code) => format!("退出码 {code}"),
                    None => "退出码未知".into(),
                };
                let line = format!("进程已退出（{exit}）{detail}");
                host.died = line.clone();
                line
            };
            app.log_line(line, LogTone::Warn);
            app.invalidate();
            // 进程退出后重新裁决，待命自动恢复（含手动停止）
            evaluate_watcher(app).await;
        }
        ProcessEvent::Error { message } => {
            {
                let mut core = app.core();
                let host = core.host_mut();
                host.pid = None;
                host.running = false;
                host.died = message.clone();
                // 启动阶段由 start() 统一收口；就绪后出错直接落进快照
                if !host.starting {
                    host.error = message.clone();
                }
            }
            app.log_line(message, LogTone::Error);
            app.invalidate();
            evaluate_watcher(app).await;
        }
    }
}

/// 一条输出事件即为宿主按行切好的一行（含行尾符，`\n` 或 `\r`）：去掉行尾即整行。
/// CRLF 由宿主拆成「…\r」与「\n」两条事件，后者的行尾去掉即为空——空白行不占日志额度。
/// 超长（进程未换行的一整段）按上限截断，不把大块文本塞进界面树。
fn output_line(data: &str) -> Option<String> {
    let line = data.trim_end_matches(['\n', '\r']);
    if line.trim().is_empty() {
        return None;
    }
    Some(truncate(line, OUTPUT_LINE_MAX))
}

/// stderr 行着色：llama-server 把日志写 stderr，行头形如「时间戳 I|W|E 组件」；
/// 解析不出级别的 stderr 行按警告处理——它至少值得注意。
fn stderr_tone(line: &str) -> LogTone {
    let body = line.trim_start();
    let mut parts = body.split_whitespace();
    let _timestamp = parts.next();
    match parts.next() {
        Some("E") => LogTone::Error,
        Some("W") => LogTone::Warn,
        Some("I" | "D") => LogTone::Normal,
        _ => LogTone::Warn,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// 待命裁决唯一入口。应待命 = 功能开 && 服务未运行且未在启动 && 未暂缓 && 探测未连接。
pub async fn evaluate_watcher(app: &Arc<App>) {
    let _guard = app.evaluate_lock.lock().await;
    let settings = app.core().settings.clone();
    let (desired, watch_now) = {
        let core = app.core();
        let host = core.host();
        (
            settings.auto_start_on_api_call && host.pid.is_none() && !host.starting && !host.watch_paused && !core.probe_online,
            host.watch,
        )
    };
    if desired {
        // 先挂起探测再进待命：待命期间端口不对外服务，探测只会白跑
        app.core().probe_suspended = true;
        let hooks = watch_hooks(app);
        match app.watcher.arm(&settings, hooks).await {
            Err(reason) => {
                pause_auto_start(app, reason).await;
            }
            Ok(()) => {
                // 排队落地期间状态可能已变：按当前状态重新裁决
                let still_desired = {
                    let core = app.core();
                    let host = core.host();
                    settings.auto_start_on_api_call
                        && host.pid.is_none()
                        && !host.starting
                        && !host.watch_paused
                        && !core.probe_online
                };
                if !still_desired {
                    app.watcher.disarm().await;
                    app.core().probe_suspended = false;
                } else if !watch_now {
                    let mut core = app.core();
                    core.host_mut().watch = true;
                    drop(core);
                    app.log_status(format!(
                        "自动启动待命中：监听 {}:{}，外部调用将自动启动",
                        settings.host, settings.port
                    ));
                    app.invalidate();
                }
            }
        }
    } else {
        // 先解除监听再恢复探测：端口一空出来，恢复的首个探测结论才真实
        let was_armed = app.watcher.disarm().await;
        app.core().probe_suspended = false;
        if was_armed && watch_now {
            app.core().host_mut().watch = false;
            app.log_status("已退出自动启动待命");
            app.invalidate();
        }
    }
}

/// 自动启动暂缓：失败原因持久（缺模型、端口被占等），重试无意义；手动启动或设置变更时恢复。
async fn pause_auto_start(app: &Arc<App>, reason: String) {
    {
        let mut core = app.core();
        core.probe_suspended = false;
        if core.host().watch {
            core.host_mut().watch = false;
        }
        core.host_mut().watch_paused = true;
    }
    app.log_status(format!("自动启动已暂缓：{reason}"));
    app.invalidate();
    let should_notify = {
        let mut core = app.core();
        if core.host().watch_pause_notified {
            false
        } else {
            core.host_mut().watch_pause_notified = true;
            true
        }
    };
    if should_notify {
        let ctx = app.ctx.clone();
        tokio::spawn(async move {
            let _ = ctx
                .notify(
                    "外部调用自动启动已暂缓",
                    Some(&format!("{reason}。手动启动服务或修改设置后会恢复待命。")),
                    NotificationLevel::Warning,
                )
                .await;
        });
    }
}

/// 外部调用触发：提示后复用普通启动流程。调用方 UA 只落日志（通知保持一行说清）。
pub async fn handle_trigger(app: &Arc<App>, request_line: String, user_agent: String) {
    // 调用方来自本机任意进程，串长只受请求头解析上限约束：进日志与通知前先钳到一行
    let request_line = truncate(&request_line, ERROR_DETAIL_MAX);
    let user_agent = truncate(&user_agent, ERROR_DETAIL_MAX);
    if app.core().host().watch {
        let mut core = app.core();
        core.host_mut().watch = false;
        drop(core);
    }
    let detail = if request_line.is_empty() { String::new() } else { format!("（{request_line}）") };
    app.log_status(format!("检测到外部 API 调用{detail}，正在启动 llama-server…"));
    if !user_agent.is_empty() {
        app.log_line(format!("调用方标识（User-Agent）：{user_agent}"), LogTone::Normal);
    }
    app.invalidate();
    let ctx = app.ctx.clone();
    let detail = detail.clone();
    tokio::spawn(async move {
        let _ = ctx
            .notify(
                "llama.cpp",
                Some(&format!("检测到外部 API 调用{detail}，正在启动 llama-server…")),
                NotificationLevel::Info,
            )
            .await;
    });
    start(app).await;
}

/// 设置变更收口：先裁决（待命路径会挂起探测），最后重渲。落盘由调用方安排。
pub async fn apply_settings(app: &Arc<App>, next: LlamaSettings) {
    {
        let mut core = app.core();
        // 功能开启下的设置变更是用户重新表态：清掉暂缓，让待命按新设置重新裁决
        if next.auto_start_on_api_call {
            core.host_mut().watch_paused = false;
            core.host_mut().watch_pause_notified = false;
        }
        core.settings = next;
    }
    app.invalidate();
    evaluate_watcher(app).await;
}

/// 连接探测结果落状态：外部起的服务下线 → 进入待命；在线 → 不占它的端口。
pub async fn on_probe_result(app: &Arc<App>, online: bool) {
    let changed = {
        let mut core = app.core();
        let changed = core.probe_online != online;
        core.probe_online = online;
        changed
    };
    if changed {
        app.invalidate();
        evaluate_watcher(app).await;
    }
}

/// 供 App 组装待命回调（回调里的长流程另起任务，不阻塞监听循环）。
pub fn watch_hooks(app: &Arc<App>) -> crate::watcher::WatchHooks {
    let trigger_app = Arc::clone(app);
    let noise_app = Arc::clone(app);
    crate::watcher::WatchHooks {
        on_trigger: Arc::new(move |line, ua| {
            let app = Arc::clone(&trigger_app);
            tokio::spawn(async move { handle_trigger(&app, line, ua).await });
        }),
        on_noise: Arc::new(move |line| {
            let app = Arc::clone(&noise_app);
            // 请求头来自外部连接、单条上限数十 KB，进日志前钳到一行
            let line = truncate(&line, ERROR_DETAIL_MAX);
            tokio::spawn(async move {
                // 裁决排队/收尾窗口里可能已退出待命：此刻的噪音行不再属于「待命收到」
                if app.core().host().watch {
                    app.log_status(format!("待命收到请求：{line}"));
                }
            });
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_line_strips_host_line_terminator() {
        assert_eq!(output_line("load\n").as_deref(), Some("load"));
        assert_eq!(output_line("load\r").as_deref(), Some("load"));
        assert_eq!(output_line("无行尾的尾段").as_deref(), Some("无行尾的尾段"));
        assert_eq!(output_line("\n"), None, "CRLF 拆开的后半段不占一行");
        assert_eq!(output_line("   \n"), None, "空白行不进日志");
    }

    #[test]
    fn output_line_caps_runaway_line() {
        let long = "x".repeat(OUTPUT_LINE_MAX + 10);
        let line = output_line(&long).unwrap();
        assert_eq!(line.chars().count(), OUTPUT_LINE_MAX + 1, "超限即截断并标记");
        assert!(line.ends_with('…'));
    }
}
