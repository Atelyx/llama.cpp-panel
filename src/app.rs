//! 共享状态与宿主事件收口：全部运行时状态在 [`App`]（单一状态树，视图为纯投影），
//! 视图树是它的纯投影；状态变化经版本号唤醒渲染循环整树重发，宿主按键 diff 挂载。

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use atelyx_plugin_protocol::method::{ContributionSlot, ProcessEventParams, ScrollEdge, ViewEventParams};
use atelyx_plugin_protocol::layout::LayoutSpecNode;
use atelyx_plugin_protocol::view::{Axis, ColorToken, EventValue, Key, KeyAction, NamedKey, ViewEvent};
use atelyx_plugin_sdk::Context;
use tokio::sync::{mpsc, watch};

use crate::controller;
use crate::download::{DownloadRequest, DownloadStatus, DownloadTask, Downloader};
use crate::hf::{fetch_repo_detail, search_models, ModelSummary, RepoDetail, SearchSort};
use crate::logfmt;
use crate::modelscan::{group_by_folder, list_models, ModelFolder};
use crate::probe::probe_once;
use crate::settings::{self, LlamaSettings, ModelLaunchParams};
use crate::tray;
use crate::views;
use crate::watcher::Watcher;

/// 与 plugin.json 的 id 一致（默认布局的视图 kind 也按它拼）。
pub const PLUGIN_ID: &str = "com.atelyx.llama-cpp-panel";
/// 与 plugin.json 的 name 一致。
pub const PLUGIN_NAME: &str = "llama.cpp 启动面板";

/// 贡献点 id（宿主以它作 surfaceId；不得含 `:`）。
pub const SURFACE_CONSOLE: &str = "console";
pub const SURFACE_LIBRARY: &str = "library";
pub const SURFACE_SETTINGS: &str = "settings";

/// 检索分页大小。
const PAGE_LIMIT: u32 = 20;
/// 检索累计取数上限（页数）：接口无游标、每次从头取满 limit，取数体积随页数线性增长。
const MAX_SEARCH_PAGES: u32 = 10;
/// 设置落盘防抖：逐键 change 不逐键写盘。
const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// 连接探测轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// 仓库详情缓存条数上限：只留最近访问的，超出丢弃最旧。
const DETAIL_CACHE_MAX: usize = 12;
/// 仓库详情缓存有效期（秒）：超龄的详情视为未命中，重新取数。
const DETAIL_CACHE_TTL: i64 = 600;

/// 日志行色调（级别字母定色 + 插件状态行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTone {
    Normal,
    Warn,
    Error,
}

pub struct LogLine {
    /// 累计序号：覆盖刷新与清屏都不复用序号，视图按它作控件态键。
    pub seq: u64,
    /// 收到时刻（本地时区 HH:MM:SS；进程输出自带时间戳，插件行靠它对时）。
    pub time: String,
    pub text: String,
    pub tone: LogTone,
    /// 行级 ANSI 主色（输出带转义且某前景色覆盖过半可见字符时生效）；None = 按 tone 规则着色。
    pub ansi: Option<ColorToken>,
}

/// 运行状态（运行中/启动中/待命中/已停止/启动失败），由进程与待命字段推导。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Running,
    Starting,
    Watching,
    Stopped,
    Failed,
}

impl RunPhase {
    /// 状态徽标与托盘条目共用的中文状态名。
    pub fn label(&self) -> &'static str {
        match self {
            RunPhase::Running => "运行中",
            RunPhase::Starting => "启动中",
            RunPhase::Watching => "待命中",
            RunPhase::Failed => "启动失败",
            RunPhase::Stopped => "已停止",
        }
    }
}

/// 进程托管状态（本插件启动与否）与日志。
#[derive(Default)]
pub struct HostState {
    pub pid: Option<u32>,
    pub running: bool,
    pub starting: bool,
    pub watch: bool,
    /// 展示将要执行的命令，便于用户确认。
    pub start_line: String,
    pub logs: VecDeque<LogLine>,
    /// 日志累计行数（含被覆盖刷新掉的拍）：界面感知新行靠它。
    pub log_seq: u64,
    /// 回刷中的逻辑行序号：`\r` 结尾的后续拍覆盖这一行，而不是新起一行。
    pub pending: Option<u64>,
    /// 跟随最新（装配时开）：新日志落地后把列表滚到底；关闭后不打扰正在回看的用户。
    pub follow: bool,
    /// 已跟随滚动到的日志序号：与 `log_seq` 不等即有新行待滚。
    pub scrolled_seq: u64,
    /// 最近一次操作的错误。
    pub error: String,
    /// 等待就绪期间进程退出或出错的原因；非空即终止等待。
    pub died: String,
    /// 最后一条非空错误输出：进程没打印可读原因时，它就是最接近现场的信息。
    pub last_stderr: String,
    /// 本次启动是否已弹过失败通知。
    pub notified: bool,
    /// 本次启动是否被用户主动停止：收场同样是进程退出，但不是失败。
    pub cancelled: bool,
    /// 自动启动暂缓：启动失败或待命异常后置位，手动启动或功能开启下的设置变更时清位。
    pub watch_paused: bool,
    /// 每轮暂缓最多提示一次。
    pub watch_pause_notified: bool,
}

impl HostState {
    /// 追加一行日志（领累计序号）。
    fn push_line(&mut self, text: String, tone: LogTone, ansi: Option<ColorToken>) {
        self.log_seq += 1;
        self.logs.push_back(LogLine { seq: self.log_seq, time: chrono_secs(), text, tone, ansi });
    }

    /// 覆盖末行（序号对得上才是待命行：插件状态行插在中间时覆盖失效，调用方改走新行）。
    fn overwrite_back(&mut self, seq: u64, text: String, tone: LogTone, ansi: Option<ColorToken>) -> bool {
        match self.logs.back_mut().filter(|line| line.seq == seq) {
            Some(line) => {
                line.text = text;
                line.time = chrono_secs();
                line.tone = tone;
                line.ansi = ansi;
                true
            }
            None => false,
        }
    }

    /// 落地一条进程输出（宿主按行切好，行尾 `\n` 或 `\r`）。
    ///
    /// `\r` 是回刷新屏：首拍建行、后续拍覆盖同一逻辑行（终端行为），模型加载进度
    /// 不把日志刷爆；`\n` 完结一行。插件自身状态行插在中间时下一拍找不到原行，
    /// 自然落成新行（进度重新起行）。返回 false = 该段不产生可见行。
    fn push_output(&mut self, stderr: bool, data: &str) -> bool {
        let event = logfmt::classify(data);
        let text = match &event {
            logfmt::OutputEvent::Line(text) | logfmt::OutputEvent::Refresh(text) => *text,
        };
        if text.trim().is_empty() {
            // 空白段不占额度；CRLF 拆出的后半个空行收掉待命行，进度行到此完结
            if matches!(event, logfmt::OutputEvent::Line(_)) {
                self.pending = None;
            }
            return false;
        }
        let (clean, ansi) = logfmt::strip_ansi(text);
        let clean = logfmt::truncate(&clean, logfmt::OUTPUT_LINE_MAX);
        let tone = if stderr { logfmt::stderr_tone(&clean) } else { LogTone::Normal };
        if stderr {
            self.last_stderr = clean.trim().to_string();
        }
        let text = if stderr { format!("! {clean}") } else { clean };
        match event {
            logfmt::OutputEvent::Refresh(_) => {
                // 待命行还在末位即覆盖；被插件状态行挤走则新起一行（进度重新起行）
                let at_back = self
                    .pending
                    .is_some_and(|seq| self.logs.back().is_some_and(|line| line.seq == seq));
                if at_back {
                    self.overwrite_back(self.pending.unwrap(), text, tone, ansi);
                } else {
                    self.push_line(text, tone, ansi);
                    self.pending = Some(self.log_seq);
                }
            }
            logfmt::OutputEvent::Line(_) => {
                // `\r` 后的文本覆盖该行（终端语义，鲜见）；没有待命行即独立新行
                let seq = self.pending.take();
                let at_back = seq.is_some_and(|seq| self.logs.back().is_some_and(|line| line.seq == seq));
                if at_back {
                    self.overwrite_back(seq.unwrap(), text, tone, ansi);
                } else {
                    self.push_line(text, tone, ansi);
                }
            }
        }
        true
    }
}

/// 模型库视图状态（检索 / 详情 / 下载入口）。
pub struct LibraryState {
    /// 已提交的检索词（输入框回显值）。
    pub query: String,
    pub sort: SearchSort,
    /// 已取回的页数（每次取 page+1 页，翻页只增）。
    pub page: u32,
    pub results: Vec<ModelSummary>,
    pub searching: bool,
    pub error: String,
    pub detail: Option<RepoDetail>,
    pub detail_loading: bool,
    /// 已取回的仓库详情（镜像 + 仓库为键，最近访问在尾部）。
    pub detail_cache: VecDeque<CachedDetail>,
    /// 最近一次预检的请求：重名拒绝后「改名保留」按它重试。
    pub last_download: Option<DownloadRequest>,
    /// 按地址直接下载的输入框回显值。
    pub dl_input: String,
    /// 列表已取到末尾：接口返回不足一页即判定没有下一页，不再挂「加载更多」。
    pub exhausted: bool,
    /// 检索轮次：每次发起检索自增，落地时轮次不符即丢弃（旧关键词的在途结果不得覆盖当前列表）。
    pub search_gen: u64,
    /// 最近一次下载失败是否因目标重名：只有重名才值得挂「改名保留」。
    pub dl_conflict: bool,
}

/// 详情缓存条目：同一镜像下的同一仓库在有效期内不再打接口。
pub struct CachedDetail {
    pub mirror_id: &'static str,
    pub repo: String,
    /// 落缓存时刻（epoch 秒）；与当前时刻比判定超龄。
    pub at: i64,
    pub detail: RepoDetail,
}

impl LibraryState {
    /// 取有效期内的详情缓存；超龄与换镜像的条目不参与命中。
    pub fn cached_detail(&self, mirror_id: &'static str, repo: &str) -> Option<RepoDetail> {
        let now = epoch_secs();
        self.detail_cache
            .iter()
            .rev()
            .find(|entry| {
                entry.mirror_id == mirror_id
                    && entry.repo == repo
                    && now - entry.at < DETAIL_CACHE_TTL
            })
            .map(|entry| entry.detail.clone())
    }

    /// 写入详情缓存：同键覆盖并刷新为最近访问，超出上限丢弃最旧。
    pub fn cache_detail(&mut self, mirror_id: &'static str, detail: &RepoDetail) {
        let entry = CachedDetail {
            mirror_id,
            repo: detail.summary.repo.clone(),
            at: epoch_secs(),
            detail: detail.clone(),
        };
        self.detail_cache
            .retain(|existing| !(existing.mirror_id == mirror_id && existing.repo == entry.repo));
        self.detail_cache.push_back(entry);
        while self.detail_cache.len() > DETAIL_CACHE_MAX {
            self.detail_cache.pop_front();
        }
    }
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            query: String::new(),
            sort: SearchSort::Downloads,
            page: 0,
            results: Vec::new(),
            searching: false,
            error: String::new(),
            detail: None,
            detail_loading: false,
            detail_cache: Default::default(),
            last_download: None,
            dl_input: String::new(),
            exhausted: false,
            search_gen: 0,
            dl_conflict: false,
        }
    }
}

/// 模型目录扫描状态（控制台左栏消费）。
#[derive(Default)]
pub struct ScanState {
    pub scanning: bool,
    pub error: String,
    pub capped: bool,
    pub folders: Vec<ModelFolder>,
    /// 列表过滤词（只在本次会话生效，不落盘）。
    pub filter: String,
}

/// 全部可变业务状态（短临界区；不跨 await 持锁）。
pub struct Core {
    pub settings: LlamaSettings,
    /// 已挂载的插件面（宿主 attached 的 surfaceId）。
    pub attached: BTreeSet<String>,
    pub probe_online: bool,
    /// 待命期间端口由插件持有：轮询与手动探测短路。
    pub probe_suspended: bool,
    pub host: HostState,
    pub lib: LibraryState,
    pub scan: ScanState,
    /// 下载任务快照（模型库视图的下载卡）。
    pub dl: DownloadTask,
    /// 托盘条目是否可用：平台不支持（非 Windows）时置假，之后不再提交。
    pub tray_supported: bool,
    /// 已提交的托盘条目签名：只在状态变化时整发整换，不按拍重提。
    pub tray_signature: String,
}

impl Core {
    pub fn host(&self) -> &HostState {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut HostState {
        &mut self.host
    }

    /// 运行状态推导：优先级 = 启动中 > 运行中 > 待命中 > 启动失败 > 已停止。
    pub fn run_phase(&self) -> RunPhase {
        let host = &self.host;
        if host.starting {
            RunPhase::Starting
        } else if host.pid.is_some() {
            RunPhase::Running
        } else if host.watch {
            RunPhase::Watching
        } else if !host.error.is_empty() {
            RunPhase::Failed
        } else {
            RunPhase::Stopped
        }
    }
}

/// 共享应用状态。长流程（启动 / 检索 / 下载 / 待命）各自起任务，经短临界区更新 Core。
/// 宿主通知由 SDK 逐条起独立任务、任务之间无先后保证：进程输出事件经队列按到达顺序处理。
pub struct App {
    pub ctx: Context,
    pub core: Mutex<Core>,
    version: watch::Sender<u64>,
    pub downloader: Arc<Downloader>,
    pub watcher: Watcher,
    /// 待命裁决串行锁：并发裁决按序收敛，防「先 disarm 后 arm 落地」留下该退出的监听。
    pub evaluate_lock: tokio::sync::Mutex<()>,
    /// 进程事件入队口：入口只入队，处理顺序由单消费者任务保证。
    pub process_tx: mpsc::UnboundedSender<ProcessEventParams>,
    /// 队列取端：装配后由消费者一次性取走。
    process_rx: Mutex<Option<mpsc::UnboundedReceiver<ProcessEventParams>>>,
    /// 取数与探测共用一条 HTTP 通道。
    pub client: reqwest::Client,
    save_gen: AtomicU64,
}

impl App {
    pub fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap()
    }

    /// 状态变化 → 唤醒渲染循环（watch 保留最新版本号，不丢唤醒）。
    pub fn invalidate(&self) {
        let next = self.version.borrow().wrapping_add(1);
        let _ = self.version.send(next);
    }

    /// 追加一行插件自身状态行（进程输出走 [`App::log_output`]）。
    pub fn log_line(&self, text: String, tone: LogTone) {
        {
            let mut core = self.core();
            core.host_mut().push_line(text, tone, None);
        }
        self.invalidate();
    }

    /// 落地一条进程输出（stderr 行按 llama-server 行头定级别并记最后错误输出）。
    pub fn log_output(&self, stderr: bool, data: &str) {
        {
            let mut core = self.core();
            core.host_mut().push_output(stderr, data);
        }
        self.invalidate();
    }

    /// 插件自身状态行（进程输出与插件行在视图里统一带时刻前缀）。
    pub fn log_status(&self, text: impl Into<String>) {
        self.log_line(text.into(), LogTone::Normal);
    }

    /// 设置落盘（防抖）：窗口内最后一次生效。
    pub fn schedule_save(self: &Arc<Self>) {
        let gen = self.save_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let app = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(SAVE_DEBOUNCE).await;
            if app.save_gen.load(Ordering::SeqCst) == gen {
                let settings = app.core().settings.clone();
                settings::save(&app.ctx, &settings).await;
            }
        });
    }

    /// 下载任务快照落状态；完成即触发重扫（模型列表要看到新落盘的文件）。
    fn on_download_task(self: &Arc<Self>, task: DownloadTask) {
        let done = task.status == DownloadStatus::Done;
        self.core().dl = task;
        self.invalidate();
        if done {
            let app = Arc::clone(self);
            tokio::spawn(async move { scan_models(&app).await });
        }
    }

    /// 清空日志显示（累计序号不动，避免已滚位置与新行错位）。
    pub fn clear_logs(&self) {
        {
            let mut core = self.core();
            let host = core.host_mut();
            host.logs.clear();
            host.pending = None;
            host.scrolled_seq = host.log_seq;
        }
        self.invalidate();
    }

    /// 切换跟随最新；重新打开时把已滚位置归零，下一轮渲染立即滚到底。
    pub fn set_follow(&self, follow: bool) {
        {
            let mut core = self.core();
            let host = core.host_mut();
            host.follow = follow;
            if follow {
                host.scrolled_seq = 0;
            }
        }
        self.invalidate();
    }
}

fn chrono_secs() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// 缓存判龄用的时刻（epoch 秒）。
fn epoch_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 装配共享状态（下载回调经 Weak 回接，避免 App ↔ Downloader 的强引用环）。
pub fn build_app(ctx: Context, settings: LlamaSettings) -> Arc<App> {
    let client = reqwest::Client::builder().build().expect("HTTP 客户端构建失败");
    let (process_tx, process_rx) = mpsc::unbounded_channel();
    Arc::new_cyclic(|weak: &Weak<App>| {
        let notify: crate::download::Notify = {
            let weak = weak.clone();
            Arc::new(move |task| {
                if let Some(app) = weak.upgrade() {
                    app.on_download_task(task);
                }
            })
        };
        App {
            ctx,
            core: Mutex::new(Core {
                settings,
                attached: BTreeSet::new(),
                probe_online: false,
                probe_suspended: false,
                host: HostState { follow: true, ..HostState::default() },
                lib: LibraryState::default(),
                scan: ScanState::default(),
                dl: DownloadTask::idle(),
                tray_supported: true,
                tray_signature: String::new(),
            }),
            version: watch::channel(0).0,
            downloader: Arc::new(Downloader::new(notify)),
            watcher: Watcher::default(),
            evaluate_lock: tokio::sync::Mutex::new(()),
            process_tx,
            process_rx: Mutex::new(Some(process_rx)),
            client,
            save_gen: AtomicU64::new(0),
        }
    })
}

/// 装配后的一次性启动：贡献注册、默认布局声明、渲染循环、探测轮询、托盘与首扫。
pub async fn initialize(app: &Arc<App>) {
    // 图标名只能取宿主图标形状表里的现有名字：未知名字宿主不报错，静默回落拼图图标
    let contributions = [
        (ContributionSlot::Panel, SURFACE_CONSOLE, "llama.cpp：控制台", "layout-dashboard"),
        (ContributionSlot::Panel, SURFACE_LIBRARY, "llama.cpp：模型商店", "download"),
        (ContributionSlot::Settings, SURFACE_SETTINGS, "llama.cpp", "server"),
    ];
    for (slot, id, label, icon) in contributions {
        if let Err(err) = app.ctx.register(slot, id, label, Some(icon), None).await {
            eprintln!("贡献注册失败（{label}）：{err}");
        }
    }
    declare_default_layout(app).await;
    for surface in [SURFACE_CONSOLE, SURFACE_LIBRARY, SURFACE_SETTINGS] {
        let build_app = Arc::clone(app);
        let build_surface = surface.to_string();
        app.ctx
            .ui()
            .mount(surface, move || {
                let core = build_app.core();
                views::build(&core, &build_surface).expect("已注册的面都有树")
            })
            .await;
    }
    tokio::spawn(render_loop(Arc::clone(app)));
    tokio::spawn(process_consumer(Arc::clone(app)));
    tokio::spawn(probe_loop(Arc::clone(app)));
    tray::spawn(app);
    // 首次探测出结果后做一次待命裁决：加载期若服务本就未运行，探测结果无变化、
    // 不会产生状态事件，必须显式裁决一次才会进入待命
    let probe_app = Arc::clone(app);
    tokio::spawn(async move {
        let settings = probe_app.core().settings.clone();
        let online = probe_once(&probe_app.client, &settings).await;
        controller::on_probe_result(&probe_app, online).await;
        controller::evaluate_watcher(&probe_app).await;
    });
    if !app.core().settings.models_dir.is_empty() {
        let app = Arc::clone(app);
        tokio::spawn(async move { scan_models(&app).await });
    }
}

/// 声明默认布局：首次启用把两面板摆进当前场景——左控制台 40%、右模型商店 60%。
/// 宿主只摆一次（记账），用户改名 / 删除后不补回；场景已含任一视图时只记账不新建。
async fn declare_default_layout(app: &Arc<App>) {
    let view_kind = |surface: &str| format!("plugin:{PLUGIN_ID}:{surface}");
    let spec = LayoutSpecNode::Split {
        direction: Axis::Horizontal,
        children: vec![
            LayoutSpecNode::Panel { views: vec![view_kind(SURFACE_CONSOLE)] },
            LayoutSpecNode::Panel { views: vec![view_kind(SURFACE_LIBRARY)] },
        ],
        sizes: vec![40.0, 60.0],
    };
    if let Err(err) = app.ctx.declare_default_layout("llama.cpp 面板", spec).await {
        eprintln!("默认布局声明失败：{err}");
    }
}

/// 进程输出事件的唯一消费者：入口只入队，这里按到达顺序逐行落地与收尾退出事件。
async fn process_consumer(app: Arc<App>) {
    let mut rx = match app.process_rx.lock().unwrap().take() {
        Some(rx) => rx,
        None => return,
    };
    while let Some(params) = rx.recv().await {
        controller::on_process_event(&app, params).await;
    }
}

async fn render_loop(app: Arc<App>) {
    let mut rx = app.version.subscribe();
    // 订阅前的唤醒不可感知（watch 不补发）：先按当前状态渲染一遍已挂载的面板
    render_attached(&app).await;
    loop {
        if rx.changed().await.is_err() {
            return;
        }
        render_attached(&app).await;
    }
}

async fn render_attached(app: &Arc<App>) {
    let attached: Vec<String> = app.core().attached.iter().cloned().collect();
    for surface in attached {
        match app.ctx.ui().notify(&surface).await {
            Ok(true) => {
                // 树已上行后再滚：新行已在宿主侧；指令只在新行落地后发一次
                if surface == SURFACE_CONSOLE {
                    follow_logs(app).await;
                }
            }
            Ok(false) => {}
            Err(err) => eprintln!("界面提交失败（{surface}）：{err}"),
        }
    }
}

/// 跟随最新：把日志列表滚到底。已在底部或已关闭跟随时不发指令。
async fn follow_logs(app: &Arc<App>) {
    let pending = {
        let core = app.core();
        let host = core.host();
        host.follow && !host.logs.is_empty() && host.log_seq != host.scrolled_seq
    };
    if !pending {
        return;
    }
    if let Err(err) = app.ctx.scroll_to(SURFACE_CONSOLE, "logs-scroll", ScrollEdge::End).await {
        // 失败不记账：这批新行的跟随留给下一次日志到达重试，否则会永久停在半路
        eprintln!("日志跟随滚动失败：{err}");
        return;
    }
    let last_seq = app.core().host().log_seq;
    app.core().host_mut().scrolled_seq = last_seq;
}

async fn probe_loop(app: Arc<App>) {
    loop {
        let suspended = app.core().probe_suspended;
        if !suspended {
            let settings = app.core().settings.clone();
            let online = probe_once(&app.client, &settings).await;
            controller::on_probe_result(&app, online).await;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// 模型目录扫描（设置里的 modelsDir 变化或用户刷新 / 下载落盘后触发）。
pub async fn scan_models(app: &Arc<App>) {
    {
        let mut core = app.core();
        if core.scan.scanning {
            return;
        }
        core.scan.scanning = true;
    }
    app.invalidate();
    let dir = app.core().settings.models_dir.clone();
    let result = tokio::task::spawn_blocking(move || list_models(&dir)).await;
    {
        let mut core = app.core();
        core.scan.scanning = false;
        match result {
            Ok(Ok(scan)) => {
                core.scan.error = String::new();
                core.scan.capped = scan.capped;
                core.scan.folders = group_by_folder(&scan, &core.settings.models_dir);
            }
            Ok(Err(error)) => {
                core.scan.error = error;
                core.scan.capped = false;
                core.scan.folders = Vec::new();
            }
            Err(err) => {
                core.scan.error = format!("扫描任务失败：{err}");
            }
        }
    }
    app.invalidate();
}

/// 宿主面挂载 / 移除：树由 SDK 在回调前重发，这里只维护本地面状态。
pub fn on_surface(app: &Arc<App>, surface_id: &str, attached: bool) {
    if attached {
        {
            let mut core = app.core();
            core.attached.insert(surface_id.to_string());
            // 控制台重新挂载：跟随开启时从底部开始（挂载前已有的行也算待跟随）
            if surface_id == SURFACE_CONSOLE {
                core.host_mut().scrolled_seq = 0;
            }
        }
        if surface_id == SURFACE_CONSOLE {
            // 重发的树已在宿主侧，列表此刻才能滚
            let app = Arc::clone(app);
            tokio::spawn(async move { follow_logs(&app).await });
        }
        // 模型商店首开自动加载首页列表（热门排序），无需先输入关键词
        if surface_id == SURFACE_LIBRARY {
            let idle = {
                let core = app.core();
                core.lib.results.is_empty() && !core.lib.searching && core.lib.query.is_empty()
            };
            if idle {
                let app = Arc::clone(app);
                tokio::spawn(async move {
                    run_search(&app, String::new(), SearchSort::Downloads, 1).await;
                });
            }
        }
    } else {
        app.core().attached.remove(surface_id);
    }
}

/// 宿主界面事件路由（按面与节点 id）。
pub async fn handle_view_event(app: &Arc<App>, params: ViewEventParams) {
    let surface = params.surface_id.as_str();
    let node = params.node_id.as_str();
    match (surface, node) {
        (SURFACE_CONSOLE, node) => console_event(app, node, &params.event).await,
        (SURFACE_LIBRARY, node) => library_event(app, node, &params.event).await,
        (SURFACE_SETTINGS, node) => settings_event(app, node, &params.event).await,
        _ => {}
    }
}

// ---------- 事件值取用 ----------

fn as_text(value: &EventValue) -> Option<&str> {
    match value {
        EventValue::Text(text) => Some(text),
        _ => None,
    }
}

fn as_index(value: &EventValue) -> Option<usize> {
    match value {
        EventValue::Number(index) if *index >= 0.0 => Some(*index as usize),
        _ => None,
    }
}

fn as_bool(value: &EventValue) -> Option<bool> {
    match value {
        EventValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn as_number(value: &EventValue) -> Option<f64> {
    match value {
        EventValue::Number(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// 数字输入事件 → 可选整数（None = 不传该旗标）。
/// 宿主把清空后的数字框上报为 [`EventValue::Null`]（与「值为 0」可区分），非数值形态与
/// 越出 min/max 边界（宿主已钳制，此处防御）同样收敛为不传。
fn parse_opt_u32(value: &EventValue, min: u32) -> Option<u32> {
    if matches!(value, EventValue::Null) {
        return None;
    }
    as_number(value)
        .map(|n| n as u64)
        .filter(|v| *v >= min as u64 && *v <= u32::MAX as u64)
        .map(|v| v as u32)
}

// ---------- 模型库面 ----------

async fn library_event(app: &Arc<App>, node: &str, event: &ViewEvent) {
    match node {
        "q" => {
            if matches!(event, ViewEvent::Submit { .. }) {
                let query = as_text(&event_value_of(event)).unwrap_or_default().to_string();
                search_current(app, query).await;
            } else if let Some(text) = as_text(&event_value_of(event)) {
                // 防抖拍同步权威值：工具行的「搜索」按钮读它，界面其他位置也按它渲染
                app.core().lib.query = text.to_string();
                app.invalidate();
            }
        }
        "search" => {
            let query = app.core().lib.query.clone();
            search_current(app, query).await;
        }
        "sort" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                let sort = SearchSort::from_index(index);
                app.core().lib.sort = sort;
                let query = app.core().lib.query.clone();
                run_search(app, query, sort, 1).await;
            }
        }
        "libmirror" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                set_mirror(app, index).await;
            }
        }
        "lib-results" => {
            // 滚到列表末端即续取下一页（界面只在末端留一行的余量内触发）
            if let ViewEvent::Scroll { offset, max_offset } = event {
                load_more_on_scroll(app, *offset, *max_offset).await;
            }
        }
        "more" => load_more(app).await,
        "dlinput" => {
            if matches!(event, ViewEvent::Submit { .. }) {
                let input = as_text(&event_value_of(event)).unwrap_or_default().to_string();
                download_direct(app, input).await;
            } else if let Some(text) = as_text(&event_value_of(event)) {
                // 同「q」：提交拍回写权威值，供下载入口读取
                app.core().lib.dl_input = text.to_string();
                app.invalidate();
            }
        }
        "dlgo" => {
            let input = app.core().lib.dl_input.clone();
            download_direct(app, input).await;
        }
        "dlresume" => {
            // 暂停任务的继续：按上次请求重新预检（.part 在，自动断点续传）
            let request = app.core().lib.last_download.clone();
            if let Some(request) = request {
                start_download(app, request, false).await;
            }
        }
        "dlrename" => {
            // 重名拒绝后的「改名保留」：按上次请求换带序号的名字重来
            let request = app.core().lib.last_download.clone();
            if let Some(request) = request {
                start_download(app, request, true).await;
            }
        }
        "dlpause" => app.downloader.cancel(),
        "dldismiss" => app.downloader.dismiss(),
        other => {
            // 检索结果卡（rcard{i}）与详情文件行（frow{i} / f{i}）：下标编码在节点 id 里
            if let Some(index) = other.strip_prefix("frow").and_then(|n| n.parse::<usize>().ok()) {
                // 文件行整行可聚焦：右键或回车/空格即下载，与行内下载钮同一动作
                if triggers_download(event) {
                    download_detail_file(app, index, false).await;
                }
            } else if let Some(index) = other.strip_prefix("rcard").and_then(|n| n.parse::<usize>().ok()) {
                open_detail(app, index).await;
            } else if let Some(index) = other.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
                download_detail_file(app, index, false).await;
            }
        }
    }
}

/// 文件行的键盘 / 右键是否触发下载：回车或空格按下、或右键。
fn triggers_download(event: &ViewEvent) -> bool {
    match event {
        ViewEvent::SecondaryClick => true,
        ViewEvent::Key { key, action, .. } => {
            matches!(action, KeyAction::Press)
                && matches!(key, Key::Named { name: NamedKey::Enter | NamedKey::Space })
        }
        _ => false,
    }
}

/// 复制到剪贴板：剪贴板没有界面反馈，成败各落一行日志（失败同时留 stderr 原因）。
async fn copy_text(app: &Arc<App>, text: String, what: &str) {
    if text.is_empty() {
        return;
    }
    match app.ctx.clipboard_write(&text).await {
        Ok(()) => app.log_status(format!("已复制{what}")),
        Err(err) => {
            eprintln!("复制{what}失败：{err}");
            app.log_line(format!("复制{what}失败：{err}"), LogTone::Error);
        }
    }
}

/// 以当前排序发起检索；空关键词不发起（避免清空输入即打接口）。
async fn search_current(app: &Arc<App>, query: String) {
    if query.trim().is_empty() {
        return;
    }
    run_search(app, query, SearchSort::current(app), 1).await;
}

/// 取下一页：在途、已知到底、还没有结果时都不发请求。
async fn load_more(app: &Arc<App>) {
    let next_pages = {
        let mut core = app.core();
        if core.lib.searching || core.lib.exhausted || core.lib.results.is_empty() {
            return;
        }
        let next = core.lib.page + 2;
        // 接口无游标、每次从头取满 limit，取数体积随页数线性增长：到顶即按到底收场
        if next > MAX_SEARCH_PAGES {
            core.lib.exhausted = true;
            drop(core);
            app.invalidate();
            return;
        }
        next
    };
    let query = app.core().lib.query.clone();
    let sort = app.core().lib.sort;
    run_search(app, query, sort, next_pages).await;
}

/// 滚动续页阈值：距末端不足该像素数即取下一页（宿主只给偏移与可滚余量，不给视口高度）。
const SCROLL_EDGE_PX: f32 = 24.0;

/// 滚到末端自动续页；`max_offset` 为 0（内容不足一屏）时不触发，避免首屏即连环取数。
async fn load_more_on_scroll(app: &Arc<App>, offset: f32, max_offset: f32) {
    if !max_offset.is_finite() || max_offset <= 0.0 || offset + SCROLL_EDGE_PX < max_offset {
        return;
    }
    load_more(app).await;
}

/// 按用户给出的地址下载：整条 URL 或 `owner/repo/文件名` 都收；地址清空即不发起。
async fn download_direct(app: &Arc<App>, input: String) {
    let input = input.trim().to_string();
    if input.is_empty() {
        return;
    }
    let request = DownloadRequest {
        input,
        dir: app.core().settings.models_dir.clone(),
        mirror_id: app.core().settings.download_mirror.clone(),
    };
    app.core().lib.dl_input = String::new();
    start_download(app, request, false).await;
}

/// 切换下载镜像站（模型库工具行与设置页共用同一份设置）。
async fn set_mirror(app: &Arc<App>, index: usize) {
    if let Some(mirror) = crate::mirrors::MIRRORS.get(index) {
        let next = {
            let mut core = app.core();
            core.settings.download_mirror = mirror.id.to_string();
            core.settings.clone()
        };
        apply_settings_and_save(app, next).await;
    }
}

/// 事件负载（submit/change 携带值；click/focus 无值）。
fn event_value_of(event: &ViewEvent) -> EventValue {
    match event {
        ViewEvent::Submit { value } | ViewEvent::Change { value } => value.clone(),
        _ => EventValue::Text(String::new()),
    }
}

impl SearchSort {
    fn current(app: &App) -> SearchSort {
        app.core().lib.sort
    }
}

async fn run_search(app: &Arc<App>, query: String, sort: SearchSort, pages: u32) {
    let pages = pages.min(MAX_SEARCH_PAGES);
    let generation = {
        let mut core = app.core();
        core.lib.search_gen += 1;
        core.lib.query = query.clone();
        core.lib.sort = sort;
        core.lib.page = pages - 1;
        core.lib.searching = true;
        core.lib.error = String::new();
        core.lib.search_gen
    };
    app.invalidate();
    let mirror = crate::mirrors::mirror_by_id(&app.core().settings.download_mirror);
    let limit = pages * PAGE_LIMIT;
    let client = app.client.clone();
    let result = search_models(&client, mirror, &query, sort, limit).await;
    {
        let mut core = app.core();
        // 在途期间已发起新一轮检索（换词、换排序、自动翻页并发）：本轮结果整批作废，
        // 否则旧关键词的列表会盖掉当前列表，并把 searching 抢先置假
        if core.lib.search_gen != generation {
            return;
        }
        core.lib.searching = false;
        match result {
            // 返回不足一页即到底：接口没有游标，靠「取满 limit」近似分页
            Ok(models) => {
                core.lib.exhausted = (models.len() as u32) < limit || pages >= MAX_SEARCH_PAGES;
                core.lib.results = models;
            }
            Err(error) => {
                core.lib.exhausted = false;
                core.lib.error = error;
            }
        }
    }
    app.invalidate();
}

async fn open_detail(app: &Arc<App>, index: usize) {
    let repo = {
        let core = app.core();
        let Some(summary) = core.lib.results.get(index) else { return };
        summary.repo.clone()
    };
    let mirror = crate::mirrors::mirror_by_id(&app.core().settings.download_mirror);
    let cached = {
        let core = app.core();
        // 在途时忽略新点击：两个在途请求各自落地会「后完成者胜」，详情可能停在先点的仓库
        if core.lib.detail_loading {
            return;
        }
        // 已展示同一仓库（重复点击）不重打接口
        if core.lib.detail.as_ref().is_some_and(|detail| detail.summary.repo == repo) {
            return;
        }
        core.lib.cached_detail(mirror.id, &repo)
    };
    if let Some(detail) = cached {
        app.core().lib.detail = Some(detail);
        app.invalidate();
        return;
    }
    app.core().lib.detail_loading = true;
    app.invalidate();
    let client = app.client.clone();
    let result = fetch_repo_detail(&client, mirror, &repo).await;
    {
        let mut core = app.core();
        core.lib.detail_loading = false;
        match result {
            Ok(detail) => {
                core.lib.cache_detail(mirror.id, &detail);
                core.lib.detail = Some(detail);
            }
            Err(error) => core.lib.error = error,
        }
    }
    app.invalidate();
}

async fn download_detail_file(app: &Arc<App>, index: usize, rename: bool) {
    let (repo, file) = {
        let core = app.core();
        let Some(detail) = core.lib.detail.as_ref() else { return };
        let Some(entry) = detail.files.get(index) else { return };
        (detail.summary.repo.clone(), entry.name.clone())
    };
    let dir = app.core().settings.models_dir.clone();
    let mirror_id = app.core().settings.download_mirror.clone();
    let request = DownloadRequest {
        input: format!("{repo}/{file}"),
        dir,
        mirror_id,
    };
    start_download(app, request, rename).await;
}

/// 预检并启动下载。重名拒绝时留下请求与可读原因，界面据此提供「改名保留」。
async fn start_download(app: &Arc<App>, request: DownloadRequest, rename: bool) {
    app.core().lib.last_download = Some(DownloadRequest {
        input: request.input.clone(),
        dir: request.dir.clone(),
        mirror_id: request.mirror_id.clone(),
    });
    app.invalidate();
    let mode = if rename { crate::download::PrepareMode::Rename } else { crate::download::PrepareMode::Skip };
    match app.downloader.prepare(&request, mode).await {
        Ok(prepared) => {
            app.core().lib.dl_conflict = false;
            app.downloader.start(prepared.task);
        }
        Err(err) => {
            let mut core = app.core();
            core.dl.status = DownloadStatus::Error;
            core.dl.status_text = err.error;
            core.lib.dl_conflict = err.existing_path.is_some();
            if let Some(existing) = err.existing_path {
                core.dl.status_text = format!("{}（可点「改名保留」换名下载）", existing);
            }
        }
    }
    app.invalidate();
}

// ---------- 控制台面 ----------

/// 选中态以启动字段（model_file）为准：界面拆在不同面也读同一份设置，选中态自然一致。
pub fn selected_folder_index(core: &Core) -> Option<usize> {
    let model_dir = settings::normalize_dir(&core.settings.model_file);
    let dir = match model_dir.rfind('/') {
        Some(idx) => model_dir[..idx].to_string(),
        None => String::new(),
    };
    core.scan
        .folders
        .iter()
        .position(|f| settings::normalize_dir(&f.full) == dir)
}

/// 选中一个模型文件夹：持久化参数里的量化/投影已不在当前文件夹时，落到第一项。
async fn select_folder(app: &Arc<App>, index: usize) {
    let next = {
        let mut core = app.core();
        let Some(folder) = core.scan.folders.get(index) else { return };
        let folder = folder.clone();
        let mut params = settings::params_for(&core.settings, &folder.full);
        let quant_names: Vec<String> =
            folder.quants.iter().map(|q| crate::modelscan::rel_name(&folder.rel, &q.name)).collect();
        let mm_names: Vec<String> =
            folder.mms.iter().map(|m| crate::modelscan::rel_name(&folder.rel, &m.name)).collect();
        if !quant_names.contains(&params.quant) {
            params.quant = quant_names.first().cloned().unwrap_or_default();
        }
        if !mm_names.contains(&params.mmproj) {
            params.mmproj = mm_names.first().cloned().unwrap_or_default();
        }
        core.settings.model_file = if params.quant.is_empty() {
            String::new()
        } else {
            crate::modelscan::join_model_path(&folder.full, &params.quant)
        };
        core.settings.mmproj_file = if params.mmproj.is_empty() {
            String::new()
        } else {
            crate::modelscan::join_model_path(&folder.full, &params.mmproj)
        };
        core.settings
            .model_params
            .insert(settings::normalize_dir(&folder.full), params);
        core.settings.clone()
    };
    apply_settings_and_save(app, next).await;
}

/// 改写选中文件夹的启动参数后的完整设置（量化决定启动用的模型文件）。
async fn update_params(app: &Arc<App>, change: impl FnOnce(&mut ModelLaunchParams, &ModelFolder)) {
    let next = {
        let mut core = app.core();
        let Some(index) = selected_folder_index(&core) else { return };
        let folder = core.scan.folders[index].clone();
        let key = settings::normalize_dir(&folder.full);
        let mut params = settings::params_for(&core.settings, &folder.full);
        change(&mut params, &folder);
        if params.quant.is_empty() {
            // 量化决定启动用模型；清空即不启动
            core.settings.model_file = String::new();
        } else {
            core.settings.model_file = crate::modelscan::join_model_path(&folder.full, &params.quant);
        }
        core.settings.mmproj_file = if params.mmproj.is_empty() {
            String::new()
        } else {
            crate::modelscan::join_model_path(&folder.full, &params.mmproj)
        };
        core.settings.model_params.insert(key, params);
        core.settings.clone()
    };
    apply_settings_and_save(app, next).await;
}

async fn console_event(app: &Arc<App>, node: &str, event: &ViewEvent) {
    match node {
        "start" => {
            let app = Arc::clone(app);
            tokio::spawn(async move { controller::start(&app).await });
        }
        "stop" => {
            let app = Arc::clone(app);
            tokio::spawn(async move { controller::stop(&app).await });
        }
        "rescan" => {
            let app = Arc::clone(app);
            tokio::spawn(async move { scan_models(&app).await });
        }
        "follow" => {
            if let Some(follow) = as_bool(&event_value_of(event)) {
                app.set_follow(follow);
            }
        }
        "clear" => app.clear_logs(),
        "copycmd" => {
            let line = app.core().host.start_line.clone();
            copy_text(app, line, "启动命令").await;
        }
        "copyaddr" => {
            let url = settings::base_url(&app.core().settings);
            copy_text(app, url, "服务地址").await;
        }
        "filter" => {
            if let Some(text) = as_text(&event_value_of(event)) {
                // 过滤只在本次会话生效：不落盘，也不触发取数
                app.core().scan.filter = text.to_string();
                app.invalidate();
            }
        }
        "clearsel" => clear_selection(app).await,
        "quant" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                update_params(app, |params, folder| {
                    if let Some(entry) = folder.quants.get(index) {
                        params.quant = crate::modelscan::rel_name(&folder.rel, &entry.name);
                    }
                })
                .await;
            }
        }
        "mmproj" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                update_params(app, |params, folder| {
                    // 0 = 不挂投影；其余映射到该文件夹的投影列表
                    params.mmproj = match index {
                        0 => String::new(),
                        n => folder
                            .mms
                            .get(n - 1)
                            .map(|entry| crate::modelscan::rel_name(&folder.rel, &entry.name))
                            .unwrap_or_default(),
                    };
                })
                .await;
            }
        }
        "ngl" => {
            let value = parse_opt_u32(&event_value_of(event), 0);
            update_params(app, |params, _| params.ngl = value).await;
        }
        "ctx" => {
            let value = parse_opt_u32(&event_value_of(event), 1);
            update_params(app, |params, _| params.ctx_size = value).await;
        }
        "threads" => {
            let value = parse_opt_u32(&event_value_of(event), 1);
            update_params(app, |params, _| params.threads = value).await;
        }
        "parallel" => {
            let value = parse_opt_u32(&event_value_of(event), 1);
            update_params(app, |params, _| params.parallel = value).await;
        }
        "cachek" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                update_params(app, |params, _| {
                    params.cache_type_k = crate::views::CACHE_K_OPTIONS
                        .get(index)
                        .copied()
                        .unwrap_or_default()
                        .to_string();
                })
                .await;
            }
        }
        "loadmode" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                update_params(app, |params, _| {
                    params.load_mode = crate::views::LOAD_MODE_OPTIONS
                        .get(index)
                        .copied()
                        .unwrap_or_default()
                        .to_string();
                })
                .await;
            }
        }
        "fa" => {
            let value = as_bool(&event_value_of(event)).unwrap_or(false);
            update_params(app, |params, _| params.flash_attn = value).await;
        }
        "jinja" => {
            let value = as_bool(&event_value_of(event)).unwrap_or(false);
            update_params(app, |params, _| params.jinja = value).await;
        }
        "nowebui" => {
            let value = as_bool(&event_value_of(event)).unwrap_or(false);
            update_params(app, |params, _| params.no_webui = value).await;
        }
        "extra" => {
            if let Some(text) = as_text(&event_value_of(event)) {
                let text = text.to_string();
                update_params(app, |params, _| params.extra_args = text).await;
            }
        }
        other => {
            // 模型卡片（fcard{i}）整卡可点选中：`i` 即 folders 里的真实下标（过滤只影响显示）
            if let Some(index) = other.strip_prefix("fcard").and_then(|n| n.parse::<usize>().ok()) {
                select_folder(app, index).await;
            }
        }
    }
}

/// 清除选择：只让启动字段落空（不启动、命令预览为空），各文件夹已记录的参数保持原样。
async fn clear_selection(app: &Arc<App>) {
    let next = {
        let mut core = app.core();
        core.settings.model_file = String::new();
        core.settings.mmproj_file = String::new();
        core.settings.clone()
    };
    apply_settings_and_save(app, next).await;
}

/// 设置变更统一收口：待命裁决 + 防抖落盘。
async fn apply_settings_and_save(app: &Arc<App>, next: LlamaSettings) {
    app.schedule_save();
    controller::apply_settings(app, next).await;
}

// ---------- 设置面 ----------

async fn settings_event(app: &Arc<App>, node: &str, event: &ViewEvent) {
    match node {
        "pickserver" | "pickmodels" => {
            let title = if node == "pickserver" {
                "选择 llama-server 所在文件夹"
            } else {
                "选择模型目录"
            };
            let ctx = app.ctx.clone();
            if let Ok(Some(path)) = ctx.pick_directory(Some(title)).await {
                let next = {
                    let mut core = app.core();
                    if node == "pickserver" {
                        core.settings.server_dir = path;
                    } else {
                        core.settings.models_dir = path;
                    }
                    core.settings.clone()
                };
                if node == "pickmodels" {
                    let app2 = Arc::clone(app);
                    tokio::spawn(async move { scan_models(&app2).await });
                }
                apply_settings_and_save(app, next).await;
            }
        }
        "host" => {
            if let Some(text) = as_text(&event_value_of(event)) {
                let text = text.trim().to_string();
                let next = {
                    let mut core = app.core();
                    core.settings.host = text;
                    core.settings.clone()
                };
                apply_settings_and_save(app, next).await;
            }
        }
        "port" => {
            if let Some(port) = parse_opt_u32(&event_value_of(event), 1).filter(|p| *p < 65536) {
                let next = {
                    let mut core = app.core();
                    core.settings.port = port as u16;
                    core.settings.clone()
                };
                apply_settings_and_save(app, next).await;
            }
        }
        "mirror" => {
            if let Some(index) = as_index(&event_value_of(event)) {
                set_mirror(app, index).await;
            }
        }
        "autostart" => {
            let value = as_bool(&event_value_of(event)).unwrap_or(false);
            let next = {
                let mut core = app.core();
                core.settings.auto_start_on_api_call = value;
                core.settings.clone()
            };
            apply_settings_and_save(app, next).await;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hf::{RepoDetail, RepoFile};

    fn detail(repo: &str, readme: &str) -> RepoDetail {
        RepoDetail {
            summary: ModelSummary {
                repo: repo.into(),
                downloads: 1,
                likes: 0,
                updated_at: String::new(),
            },
            files: vec![RepoFile { name: "a.gguf".into(), size: Some(1) }],
            readme: readme.into(),
            gated: false,
        }
    }

    #[test]
    fn detail_cache_hit_requires_same_mirror() {
        let mut lib = LibraryState::default();
        lib.cache_detail("hf-mirror", &detail("a/b", "镜像一"));
        assert_eq!(lib.cached_detail("hf-mirror", "a/b").unwrap().readme, "镜像一");
        assert!(lib.cached_detail("hf", "a/b").is_none(), "换镜像取的是另一站的文件树，不得复用");
        assert!(lib.cached_detail("hf-mirror", "a/c").is_none());
    }

    #[test]
    fn detail_cache_expires_and_overwrites() {
        let mut lib = LibraryState::default();
        lib.cache_detail("hf", &detail("a/b", "首版"));
        lib.detail_cache.front_mut().unwrap().at = epoch_secs() - DETAIL_CACHE_TTL;
        assert!(lib.cached_detail("hf", "a/b").is_none(), "超龄即未命中，重新取数");

        lib.cache_detail("hf", &detail("a/b", "重取后的新版"));
        assert_eq!(lib.cached_detail("hf", "a/b").unwrap().readme, "重取后的新版");
        assert_eq!(lib.detail_cache.len(), 1, "同键覆盖不重复占位");
    }

    #[test]
    fn detail_cache_evicts_oldest_beyond_capacity() {
        let mut lib = LibraryState::default();
        for i in 0..DETAIL_CACHE_MAX + 3 {
            lib.cache_detail("hf", &detail(&format!("a/repo{i}"), "x"));
        }
        assert_eq!(lib.detail_cache.len(), DETAIL_CACHE_MAX);
        assert!(lib.cached_detail("hf", "a/repo0").is_none(), "最旧的被丢弃");
        assert!(lib.cached_detail("hf", &format!("a/repo{}", DETAIL_CACHE_MAX + 2)).is_some());
    }

    #[test]
    fn cleared_number_event_means_no_flag() {
        // 线上契约：宿主把清空上报为 JSON null，untagged 枚举须落回 Null（否则清空无从识别）
        let parsed: EventValue = serde_json::from_str("null").unwrap();
        assert_eq!(parsed, EventValue::Null);
        assert_eq!(parse_opt_u32(&EventValue::Null, 1), None, "清空上行 null = 不传该旗标");
        assert_eq!(parse_opt_u32(&EventValue::Number(0.0), 0), Some(0));
        assert_eq!(parse_opt_u32(&EventValue::Number(8.0), 1), Some(8));
        assert_eq!(parse_opt_u32(&EventValue::Number(0.0), 1), None, "越出下限不传");
        assert_eq!(parse_opt_u32(&EventValue::Text(String::new()), 1), None);
    }

    #[test]
    fn carriage_return_refreshes_one_log_line() {
        let mut host = HostState::default();
        assert!(host.push_output(false, "load model 10%\r"));
        assert!(host.push_output(false, "load model 90%\r"));
        assert_eq!(host.logs.len(), 1, "连续 \\r 只占一行");
        assert_eq!(host.logs[0].text, "load model 90%");
        assert!(host.pending.is_some());
        // CRLF 的后半段收掉待命行：进度行完结，不额外占行
        assert!(!host.push_output(false, "\n"));
        assert!(host.pending.is_none());
        assert_eq!(host.logs.len(), 1);
        assert!(host.push_output(false, "server started\n"));
        assert_eq!(host.logs.len(), 2, "完结后的输出是新行");
    }

    #[test]
    fn newline_and_blank_segments() {
        let mut host = HostState::default();
        assert!(!host.push_output(false, "\n"), "空白行不占额度");
        assert!(host.logs.is_empty());
        assert!(!host.push_output(false, "   \r"), "纯空白回刷拍同样跳过");
        assert!(host.pending.is_none(), "空白拍不建待命行");
        assert!(host.push_output(false, "hello"));
        assert_eq!(host.logs.len(), 1, "EOF 末段按完结行落地");
    }

    #[test]
    fn status_line_between_refresh_beats_starts_new_progress_line() {
        let mut host = HostState::default();
        assert!(host.push_output(false, "load 10%\r"));
        let seq = host.pending.unwrap();
        host.push_line("插件状态行".into(), LogTone::Normal, None);
        assert!(host.push_output(false, "load 90%\r"));
        assert_eq!(host.logs.len(), 3, "状态行插在中间：覆盖失效，进度重新起行");
        assert_eq!(host.logs[0].seq, seq, "原进度行保持原位");
        assert_eq!(host.logs[2].text, "load 90%");
    }

    #[test]
    fn stderr_output_gets_prefix_tone_and_ansi_color() {
        let mut host = HostState::default();
        assert!(host.push_output(true, "10:00:00 E main: boom\n"));
        assert_eq!(host.logs[0].tone, LogTone::Error);
        assert_eq!(host.logs[0].text, "! 10:00:00 E main: boom");
        assert_eq!(host.last_stderr, "10:00:00 E main: boom");
        assert!(host.push_output(false, "\u{1b}[32mready\u{1b}[0m\n"));
        assert_eq!(host.logs[1].text, "ready", "转义被剥掉");
        assert_eq!(host.logs[1].ansi, Some(ColorToken::Success));
    }
}
