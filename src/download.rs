//! 模型下载：多任务并行状态机（至多 3 路同时传输，其余排队）——发起请求、看进度、收尾改名。
//! 落盘到 `模型目录/发布者/模型名/`（仓库子目录文件压平），先写 `<文件名>.part`、下完才改名 `.gguf`。
//! 任务以自增 id 标识：被移除任务的迟到回调直接放弃；同目标活跃任务拒绝重复入队；取消保留 `.part` 续传。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use crate::mirrors::unique_name;
use crate::modelscan::file_name_of;

/// 同时传输的下载路数上限；其余任务排队等空位。
pub const PARALLEL_DOWNLOADS: usize = 3;

/// 下载任务状态。`paused` 表示已取消但 `.part` 保留，可继续。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Done,
    Error,
}

/// 下载任务快照（界面消费）。
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadTask {
    /// 任务标识：界面操作（暂停/继续/移除）按它路由。
    pub id: u64,
    pub status: DownloadStatus,
    /// 仓库全名。
    pub repo: String,
    /// 落盘文件名（纯文件名，不含目录；仓库子目录已压平）。
    pub file: String,
    /// 最终落盘全路径（/ 分隔）。
    pub target: String,
    /// 完整下载地址（含所选镜像域名）。
    pub url: String,
    /// 总字节数；来源未给出时为 None。
    pub total_bytes: Option<u64>,
    /// 已写入的字节数（含断点续传已存在的部分）。
    pub bytes: u64,
    /// 目标重名被拒：界面据此给「改名保留」。
    pub conflict: bool,
    pub status_text: String,
}

/// 下载请求（界面收集的用户输入）。
#[derive(Clone)]
pub struct DownloadRequest {
    /// 用户填写的下载地址或 owner/repo/文件名。
    pub input: String,
    /// 模型目录根；落盘为 `根/发布者/模型名/文件名`。
    pub dir: String,
    /// 镜像站 id。
    pub mirror_id: String,
}

/// 预检结果：成功带任务；失败带可读原因与已存在文件路径（重名时，供「改名保留」入口）。
pub type PrepareResult = Result<PreparedTask, PrepareError>;

#[derive(Debug)]
pub struct PreparedTask {
    pub task: DownloadTask,
}

#[derive(Debug)]
pub struct PrepareError {
    pub error: String,
    /// 重名拒绝时已有文件的完整路径。
    pub existing_path: Option<String>,
    /// 解析出的仓库与文件名（地址解析失败时为空，错误卡至少有话可说）。
    pub repo: String,
    pub file: String,
}

/// 状态变化回调：任务列表每次变化调用一次（界面层据此刷新）。
pub type Notify = Arc<dyn Fn(Vec<DownloadTask>) + Send + Sync>;

/// 传输停滞判定：时间窗内增量字节低于阈值即中止（模型下载不值得无限等死连接）。
#[derive(Clone, Copy)]
struct StallConfig {
    window: Duration,
    min_bytes: u64,
}

/// 一条任务条目：快照 + 原始请求 + 在跑任务的取消通道。
struct Entry {
    task: DownloadTask,
    /// 原始请求：继续与「改名保留」按它重来；预检失败落的错误卡也带它。
    request: Option<DownloadRequest>,
    /// Some = 传输中，停收信号经它送达；收尾（完成/失败/暂停）即置 None。
    cancel: Option<watch::Sender<bool>>,
}

/// 下载执行器。至多 [`PARALLEL_DOWNLOADS`] 路同时传输，其余排队；全部落盘都在插件进程内直写。
pub struct Downloader {
    client: reqwest::Client,
    inner: Mutex<Vec<Entry>>,
    next_id: Mutex<u64>,
    notify: Notify,
    stall: StallConfig,
}

/// 传输失败后的自动重试次数（网络抖动自愈；续传从头算已下字节，不浪费流量）。
const RETRIES: u32 = 3;
/// 建立连接的超时：站点只连不答时靠它尽早失败。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

impl Downloader {
    pub fn new(notify: Notify) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("HTTP 客户端构建失败");
        Self {
            client,
            inner: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
            notify,
            stall: StallConfig { window: Duration::from_secs(60), min_bytes: 1024 * 60 },
        }
    }

    #[cfg(test)]
    fn with_stall(mut self, window: Duration, min_bytes: u64) -> Self {
        self.stall = StallConfig { window, min_bytes };
        self
    }

    /// 当前任务列表快照。
    pub fn tasks(&self) -> Vec<DownloadTask> {
        self.inner.lock().unwrap().iter().map(|entry| entry.task.clone()).collect()
    }

    fn allocate_id(&self) -> u64 {
        let mut next = self.next_id.lock().unwrap();
        *next += 1;
        *next
    }

    fn notify_snapshot(&self) {
        (self.notify)(self.tasks());
    }

    /// 预检：解析地址、确认落盘名与远端体积。该模型文件夹里已有同名文件即判定为已下载并拒绝
    /// （重下几个 GB 不可接受）；界面据此提供「改名保留」入口（`mode = Rename`）。
    pub async fn prepare(&self, req: &DownloadRequest, mode: PrepareMode) -> PrepareResult {
        let parsed = match crate::mirrors::parse_target(&req.input) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Err(PrepareError { error, existing_path: None, repo: String::new(), file: String::new() })
            }
        };
        let dir = req.dir.trim().replace('\\', "/");
        let dir = dir.trim_end_matches('/').to_string();
        if dir.is_empty() {
            return Err(PrepareError {
                error: "未设置模型目录，请在设置页选择".into(),
                existing_path: None,
                repo: parsed.repo,
                file: file_name_of(&parsed.file),
            });
        }
        // 模型根目录必须先存在：设置写错时给明确原因，而下面的仓库子目录允许还没建
        if let Err(err) = std::fs::read_dir(&dir) {
            return Err(PrepareError {
                error: format!("读取模型目录失败：{err}（目录不存在或不是目录）"),
                existing_path: None,
                repo: parsed.repo,
                file: file_name_of(&parsed.file),
            });
        }

        let repo_dir = format!("{dir}/{}", parsed.repo);
        let files = files_in(&repo_dir);
        // 仓库里的文件路径可能带子目录，落盘压平成单段名（与仓库文件夹同层摆放）
        let wanted = file_name_of(&parsed.file);
        let existing = files.iter().find(|name| name.to_lowercase() == wanted.to_lowercase());
        let existing_path = existing.as_ref().map(|name| format!("{repo_dir}/{name}"));
        if let (Some(existing_name), PrepareMode::Skip) = (&existing, mode) {
            return Err(PrepareError {
                error: format!("模型目录已有 {wanted}（{repo_dir}/{existing_name}），无需重复下载"),
                existing_path,
                repo: parsed.repo,
                file: wanted,
            });
        }

        // 体积只用于显示进度百分比：探测不到（镜像不支持 HEAD、网络抖动）也照下
        let mirror = crate::mirrors::mirror_by_id(&req.mirror_id);
        let url = crate::mirrors::download_url(mirror, &parsed.repo, &parsed.file, "main");
        let total_bytes = crate::hf::fetch_remote_info(&self.client, &url).await;

        let final_name = match (&existing, mode) {
            (Some(_), PrepareMode::Rename) => {
                let set: std::collections::HashSet<String> =
                    files.iter().map(|n| n.to_lowercase()).collect();
                unique_name(&set, &wanted)
            }
            _ => wanted.clone(),
        };
        // 上次中断留下的临时文件：预检就把已下字节数带出来，续传时进度从实际位置起算
        let part_name = format!("{final_name}.part");
        let bytes = files
            .iter()
            .find(|name| **name == part_name)
            .and_then(|name| std::fs::metadata(format!("{repo_dir}/{name}")).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(PreparedTask {
            task: DownloadTask {
                id: self.allocate_id(),
                status: DownloadStatus::Queued,
                repo: parsed.repo,
                target: format!("{repo_dir}/{final_name}"),
                file: final_name,
                url,
                total_bytes,
                bytes,
                conflict: false,
                status_text: String::new(),
            },
        })
    }

    /// 提交一次下载：预检成功入队（有并发空位即开传），预检失败落一张错误卡。
    /// 同目标的活跃任务（排队/下载中）拒绝重复；同目标暂停中视为「继续」；
    /// 同目标的已落定条目由新任务替代。
    pub async fn submit(self: &Arc<Self>, req: DownloadRequest, mode: PrepareMode) -> Result<(), String> {
        match self.prepare(&req, mode).await {
            Err(err) => {
                let mut text = err.error.clone();
                if let Some(existing) = &err.existing_path {
                    text = format!("{existing}（可点「改名保留」换名下载）");
                }
                let target = err.existing_path.clone().unwrap_or_default();
                let conflict = err.existing_path.is_some();
                self.enqueue_error(&req, err.repo, err.file, target, text, conflict);
                Ok(())
            }
            Ok(prepared) => self.enqueue(prepared.task, Some(req)),
        }
    }

    /// 入队一个预检过的任务：同目标去重后排队并补位开传。
    fn enqueue(self: &Arc<Self>, task: DownloadTask, request: Option<DownloadRequest>) -> Result<(), String> {
        {
            let mut entries = self.inner.lock().unwrap();
            // 同一输入此前的错误卡（如「未设置模型目录」）在用户修正后重提时被替代，而非残留堆积
            if let Some(request) = &request {
                entries.retain(|entry| {
                    !(entry.task.status == DownloadStatus::Error
                        && entry.request.as_ref().is_some_and(|r| r.input == request.input))
                });
            }
            if let Some(existing) = entries.iter().find(|entry| entry.task.target == task.target) {
                match existing.task.status {
                    DownloadStatus::Queued | DownloadStatus::Downloading => {
                        return Err(format!("已在下载列表：{}", task.file));
                    }
                    DownloadStatus::Paused => {
                        // 同目标再点一次 = 继续：回到队列，传输从断点起
                        if let Some(entry) = entries.iter_mut().find(|entry| entry.task.target == task.target) {
                            entry.task.status = DownloadStatus::Queued;
                            entry.task.status_text = "排队等待…".into();
                        }
                        drop(entries);
                        self.notify_snapshot();
                        self.pump();
                        return Ok(());
                    }
                    DownloadStatus::Done | DownloadStatus::Error => {
                        entries.retain(|entry| entry.task.target != task.target);
                    }
                }
            }
            entries.push(Entry { task, request, cancel: None });
        }
        self.notify_snapshot();
        self.pump();
        Ok(())
    }

    /// 预检失败也落一张错误卡：任务列表是下载入口的唯一反馈面。
    /// 同一输入的旧错误卡被替代而非堆积。
    fn enqueue_error(
        self: &Arc<Self>,
        request: &DownloadRequest,
        repo: String,
        file: String,
        target: String,
        status_text: String,
        conflict: bool,
    ) {
        {
            let mut entries = self.inner.lock().unwrap();
            entries.retain(|entry| {
                !(entry.task.status == DownloadStatus::Error
                    && entry.request.as_ref().is_some_and(|r| r.input == request.input))
            });
            entries.push(Entry {
                task: DownloadTask {
                    id: self.allocate_id(),
                    status: DownloadStatus::Error,
                    repo,
                    file,
                    target,
                    url: String::new(),
                    total_bytes: None,
                    bytes: 0,
                    conflict,
                    status_text,
                },
                request: Some(request.clone()),
                cancel: None,
            });
        }
        self.notify_snapshot();
    }

    /// 有空位就把队头补上：置下载中、发取消通道、起传输任务。
    fn pump(self: &Arc<Self>) {
        let to_start = {
            let mut entries = self.inner.lock().unwrap();
            let free = PARALLEL_DOWNLOADS.saturating_sub(
                entries.iter().filter(|entry| entry.cancel.is_some()).count(),
            );
            let mut started = Vec::new();
            for slot in entries
                .iter_mut()
                .filter(|entry| entry.task.status == DownloadStatus::Queued)
                .take(free)
            {
                let (cancel_tx, cancel_rx) = watch::channel(false);
                slot.cancel = Some(cancel_tx);
                slot.task.status = DownloadStatus::Downloading;
                slot.task.status_text = "启动下载…".into();
                started.push((slot.task.id, slot.task.clone(), cancel_rx));
            }
            started
        };
        if to_start.is_empty() {
            return;
        }
        for (id, task, cancel_rx) in to_start {
            let this = Arc::clone(self);
            tokio::spawn(async move { this.run(id, task, cancel_rx).await });
        }
        self.notify_snapshot();
    }

    /// 暂停：中止当前传输并保留 `.part`。不补位排队任务——用户放慢节奏的意图优先。
    pub fn pause(&self, id: u64) {
        let cancel = {
            self.inner
                .lock()
                .unwrap()
                .iter()
                .find(|entry| entry.task.id == id)
                .and_then(|entry| entry.cancel.clone())
        };
        if let Some(cancel) = cancel {
            let _ = cancel.send(true);
        }
    }

    /// 继续：暂停中的任务回到队列，按并发空位开传（断点续传）。
    pub fn resume(self: &Arc<Self>, id: u64) {
        let requeued = {
            let mut entries = self.inner.lock().unwrap();
            match entries.iter_mut().find(|entry| entry.task.id == id) {
                Some(entry) if entry.task.status == DownloadStatus::Paused && entry.cancel.is_none() => {
                    entry.task.status = DownloadStatus::Queued;
                    entry.task.status_text = "排队等待…".into();
                    true
                }
                _ => false,
            }
        };
        if requeued {
            self.notify_snapshot();
            self.pump();
        }
    }

    /// 移除任务条目；传输中的先停收（.part 保留，可再次下载续传），腾出的并发位立即补队头。
    pub fn remove(self: &Arc<Self>, id: u64) {
        let cancel = {
            let mut entries = self.inner.lock().unwrap();
            let cancel = entries.iter().find(|entry| entry.task.id == id).and_then(|entry| entry.cancel.clone());
            entries.retain(|entry| entry.task.id != id);
            cancel
        };
        let was_running = cancel.is_some();
        if let Some(cancel) = cancel {
            let _ = cancel.send(true);
        }
        self.notify_snapshot();
        if was_running {
            self.pump();
        }
    }

    /// 清空已完成与已失败的任务卡（传输中的不受影响）。
    pub fn clear_finished(&self) {
        {
            self.inner
                .lock()
                .unwrap()
                .retain(|entry| !matches!(entry.task.status, DownloadStatus::Done | DownloadStatus::Error));
        }
        self.notify_snapshot();
    }

    /// 任务条目的原始请求（「改名保留」按它重来）。
    pub fn request_of(&self, id: u64) -> Option<DownloadRequest> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.task.id == id)
            .and_then(|entry| entry.request.clone())
    }

    /// 落定一个任务的状态并释放传输通道；条目已被移除即放弃（迟到回调不得写回）。
    fn settle(&self, id: u64, status: DownloadStatus, status_text: String, conflict: bool) -> bool {
        {
            let mut entries = self.inner.lock().unwrap();
            let Some(entry) = entries.iter_mut().find(|entry| entry.task.id == id) else {
                return false;
            };
            entry.cancel = None;
            entry.task.status = status;
            entry.task.conflict = conflict;
            entry.task.status_text = status_text;
        }
        self.notify_snapshot();
        true
    }

    /// 任务落定并腾出并发位：除「暂停」（用户放慢节奏的意图优先）外立即从队头补位。
    fn end_lane(self: &Arc<Self>, id: u64, status: DownloadStatus, status_text: String, conflict: bool) {
        if self.settle(id, status, status_text, conflict) && status != DownloadStatus::Paused {
            self.pump();
        }
    }

    /// 推进一次传输中的进度读数（任务已被暂停/移除则放弃）。
    fn progress(&self, id: u64, bytes: u64, status_text: String) {
        {
            let mut entries = self.inner.lock().unwrap();
            let Some(entry) = entries.iter_mut().find(|entry| entry.task.id == id) else {
                return;
            };
            if entry.cancel.is_none() {
                return;
            }
            entry.task.bytes = bytes;
            entry.task.status_text = status_text;
        }
        self.notify_snapshot();
    }

    async fn run(self: Arc<Self>, id: u64, prepared: DownloadTask, mut cancel_rx: watch::Receiver<bool>) {
        // 目标位置已有同名文件（预检后用户手工放入）：不静默覆盖，走「改名保留」
        if std::path::Path::new(&prepared.target).exists() {
            self.end_lane(
                id,
                DownloadStatus::Error,
                format!("目标已存在同名文件：{}（可点「改名保留」换名下载）", prepared.target),
                true,
            );
            return;
        }
        let part_path = format!("{}.part", prepared.target);
        let part_size = file_size(&part_path);
        // 临时文件已达完整体积（上次下载完了但改名失败）：直接改名，重发请求只会得到 Range 错误
        if let (Some(size), Some(total)) = (part_size, prepared.total_bytes) {
            if size >= total {
                self.progress(id, size, "文件已完整，正在改名落盘…".into());
                self.finish(id, &prepared, size).await;
                return;
            }
        }

        self.progress(
            id,
            part_size.unwrap_or(0),
            if part_size.unwrap_or(0) > 0 { "从断点继续…".into() } else { "启动下载…".into() },
        );

        let mut attempt = 0u32;
        loop {
            if *cancel_rx.borrow() {
                self.end_lane(id, DownloadStatus::Paused, "已暂停；再次下载从断点继续".into(), false);
                return;
            }
            let part_now = file_size(&part_path).unwrap_or(0);
            match self.attempt_once(id, &prepared, &part_path, part_now, &mut cancel_rx).await {
                AttemptOutcome::Completed => {
                    self.finish(id, &prepared, file_size(&part_path).unwrap_or(0)).await;
                    return;
                }
                AttemptOutcome::Fatal(message) => {
                    self.end_lane(id, DownloadStatus::Error, message, false);
                    return;
                }
                AttemptOutcome::Cancelled => {
                    self.end_lane(id, DownloadStatus::Paused, "已暂停；再次下载从断点继续".into(), false);
                    return;
                }
                AttemptOutcome::Transient(message) => {
                    attempt += 1;
                    if attempt > RETRIES {
                        self.end_lane(id, DownloadStatus::Error, message, false);
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    /// 一次传输尝试。返回四类结局：完成 / 致命失败 / 可重试失败 / 被暂停。
    async fn attempt_once(
        &self,
        id: u64,
        prepared: &DownloadTask,
        part_path: &str,
        part_size: u64,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> AttemptOutcome {
        let mut request = self.client.get(&prepared.url);
        if part_size > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={part_size}-"));
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => return AttemptOutcome::Transient(format!("请求失败：{err}")),
        };
        let status = response.status();
        if part_size > 0 && status == reqwest::StatusCode::OK {
            // 请求了续传却拿到 200：服务端不支持 Range，续下去只会产出损坏文件
            return AttemptOutcome::Fatal("服务器不支持分段下载（无法断点续传）".into());
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            return AttemptOutcome::Fatal(
                "已下载内容超出远端体积：删除目标目录中的 .part 临时文件后重新下载".into(),
            );
        }
        if !status.is_success() {
            return AttemptOutcome::Fatal(format!(
                "服务器返回错误状态 {}（文件不存在或无权限）",
                status.as_u16()
            ));
        }

        // 落盘目录层级不存在时补建（下载到「发布者/模型名」这类新目录用）
        if let Some(parent) = std::path::Path::new(part_path).parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return AttemptOutcome::Fatal(format!("创建落盘目录失败：{err}"));
            }
        }
        let file = if part_size > 0 {
            tokio::fs::OpenOptions::new().append(true).open(part_path).await
        } else {
            tokio::fs::File::create(part_path).await
        };
        let mut file = match file {
            Ok(file) => file,
            Err(err) => {
                return AttemptOutcome::Fatal(format!(
                    "写文件失败：{err}（请检查目标目录是否存在、写权限与磁盘空间）"
                ))
            }
        };

        let mut stream = response;
        let mut bytes = part_size;
        let mut window_bytes = part_size;
        let mut window_start = Instant::now();
        let mut last_notify = Instant::now();
        let mut notify_bytes = bytes;

        loop {
            let deadline = tokio::time::Instant::from_std(window_start + self.stall.window);
            tokio::select! {
                chunk = stream.chunk() => {
                    let chunk = match chunk {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break, // 流正常结束
                        Err(err) => return AttemptOutcome::Transient(format!("接收数据失败：连接中断（{err}）")),
                    };
                    if let Err(err) = file.write_all(&chunk).await {
                        return AttemptOutcome::Fatal(format!(
                            "写文件失败：{err}（请检查磁盘空间）"
                        ));
                    }
                    bytes += chunk.len() as u64;
                    let now = Instant::now();
                    if now.duration_since(last_notify) >= Duration::from_millis(600) {
                        let dt = now.duration_since(last_notify).as_secs_f64();
                        let rate = (bytes - notify_bytes) as f64 / dt;
                        notify_bytes = bytes;
                        last_notify = now;
                        self.progress(id, bytes, progress_text(bytes, prepared.total_bytes, &format_rate(rate)));
                    }
                    // 停滞窗口推进：窗口期满检查增量
                    if now.duration_since(window_start) >= self.stall.window {
                        if bytes - window_bytes < self.stall.min_bytes {
                            return AttemptOutcome::Fatal(
                                "网络停滞：持续 60 秒几乎无新数据，已中止（再次下载从断点继续）".into(),
                            );
                        }
                        window_start = now;
                        window_bytes = bytes;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    if bytes - window_bytes < self.stall.min_bytes {
                        return AttemptOutcome::Fatal(
                            "网络停滞：持续无新数据，已中止（再次下载从断点继续）".into(),
                        );
                    }
                    window_start = Instant::now();
                    window_bytes = bytes;
                }
                // 进度到点即推（数据稀疏时也把已收字节数与当前速度给到界面）
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(last_notify + Duration::from_millis(600))) => {
                    let dt = last_notify.elapsed().as_secs_f64();
                    let rate = (bytes - notify_bytes) as f64 / dt.max(0.001);
                    notify_bytes = bytes;
                    last_notify = Instant::now();
                    self.progress(id, bytes, progress_text(bytes, prepared.total_bytes, &format_rate(rate)));
                }
                // 暂停：落定状态由这里收口（cancel 发送方不写状态，避免与传输竞态）
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        let _ = file.flush().await;
                        return AttemptOutcome::Cancelled;
                    }
                }
            }
        }
        if file.flush().await.is_err() || file.sync_all().await.is_err() {
            return AttemptOutcome::Fatal("写文件失败：落盘校验失败".into());
        }
        if *cancel_rx.borrow() {
            return AttemptOutcome::Cancelled;
        }
        AttemptOutcome::Completed
    }

    /// 完成收尾：`.part` 改名为最终文件，腾出的并发位立即补队头。
    async fn finish(self: &Arc<Self>, id: u64, prepared: &DownloadTask, size: u64) {
        let part_path = format!("{}.part", prepared.target);
        let final_name = file_name_of(&prepared.target);
        if let Err(err) = tokio::fs::rename(&part_path, &prepared.target).await {
            // 改名失败给出可操作指引并保留临时文件
            let message = format!(
                "下载已完成但改名失败：{err}。文件已完整保存在 {part_path}，改名为 {final_name} 即可使用。"
            );
            self.end_lane(id, DownloadStatus::Error, message, false);
            return;
        }
        self.end_lane(
            id,
            DownloadStatus::Done,
            format!("已保存到 {}（{}）", prepared.target, crate::modelscan::format_size(Some(size))),
            false,
        );
    }
}

enum AttemptOutcome {
    Completed,
    Fatal(String),
    Transient(String),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareMode {
    /// 同名文件已存在即拒绝。
    Skip,
    /// 同名文件存在时改用带序号的新名字。
    Rename,
}

/// 列出目录里的普通文件名；目录不存在或读不了都算空表（首次下载该仓库时目录还没建）。
fn files_in(dir: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|read| {
            read.flatten()
                .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn file_size(path: &str) -> Option<u64> {
    std::fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// 进度文案：有总体积时带百分比，没有时只报已下字节数。
fn progress_text(bytes: u64, total: Option<u64>, rate: &str) -> String {
    let downloaded = crate::modelscan::format_size(Some(bytes));
    match total {
        Some(total) if total > 0 => {
            let percent = (bytes as f64 / total as f64 * 100.0).min(100.0);
            format!(
                "{} / {}（{percent:.1}%）{}",
                downloaded,
                crate::modelscan::format_size(Some(total)),
                rate
            )
        }
        _ => format!("{downloaded}（未知大小）{rate}"),
    }
}

fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec <= 0.0 {
        return String::new();
    }
    format!("，{}/s", crate::modelscan::format_size(Some(bytes_per_sec as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    /// 极简测试 HTTP 服务器：按场景应答，支持 Range（206 续传）与「忽略 Range」两种模式。
    /// 截断（truncate_at）只作用于首次连接——模拟传输中断后，续传连接完整送达。
    /// 延迟（delay_ms）在应答前挂起，供并发路数断言留出观察窗口。
    struct TestServer {
        url: String,
    }

    impl TestServer {
        async fn spawn(body: Vec<u8>, ignore_range: bool, truncate_at: Option<usize>, delay_ms: u64) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let ignore_range = Arc::new(std::sync::atomic::AtomicBool::new(ignore_range));
            let truncate_at = Arc::new(std::sync::atomic::AtomicUsize::new(truncate_at.unwrap_or(0)));
            let connection_no = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else { break };
                    let body = body.clone();
                    let ignore_range = ignore_range.clone();
                    let truncate_at = truncate_at.clone();
                    let connection_no = connection_no.clone();
                    tokio::spawn(async move {
                        // 读到头部结束
                        let mut buf = Vec::new();
                        let mut byte = [0u8; 1];
                        use tokio::io::AsyncReadExt;
                        loop {
                            if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                                return;
                            }
                            buf.push(byte[0]);
                            if buf.ends_with(b"\r\n\r\n") {
                                break;
                            }
                        }
                        let head = String::from_utf8_lossy(&buf);
                        let has_range = head.to_lowercase().contains("range: bytes=");
                        let offset = if has_range {
                            head.to_lowercase()
                                .split("range: bytes=")
                                .nth(1)
                                .and_then(|rest| rest.split('-').next())
                                .and_then(|n| n.parse::<usize>().ok())
                                .unwrap_or(0)
                        } else {
                            0
                        };
                        let use_range = has_range && !ignore_range.load(std::sync::atomic::Ordering::SeqCst);
                        let payload = &body[offset.min(body.len())..];
                        if delay_ms > 0 {
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        }
                        // 截断只对首次连接生效（模拟中断）；续传连接完整送达
                        let this_no =
                            connection_no.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let cut = if this_no == 0 {
                            truncate_at.load(std::sync::atomic::Ordering::SeqCst)
                        } else {
                            0
                        };
                        let send = if cut > 0 { &payload[..cut.min(payload.len())] } else { payload };
                        let status = if use_range { "206 Partial Content" } else { "200 OK" };
                        let total = body.len() - offset.min(body.len());
                        let header = if use_range {
                            format!(
                                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n\r\n",
                                send.len(),
                                offset,
                                body.len() - 1,
                                body.len()
                            )
                        } else {
                            format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", total.max(send.len()))
                        };
                        let _ = socket.write_all(header.as_bytes()).await;
                        let _ = socket.write_all(send).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self { url: format!("http://{addr}/file.gguf") }
        }

        fn url(&self) -> String {
            self.url.clone()
        }
    }

    fn collector() -> (Notify, StdArc<Mutex<Vec<DownloadTask>>>) {
        let seen: StdArc<Mutex<Vec<DownloadTask>>> = Default::default();
        let sink = seen.clone();
        let notify: Notify = Arc::new(move |tasks| *sink.lock().unwrap() = tasks);
        (notify, seen)
    }

    /// 构造一个不经预检的任务（测试里直接控制地址与体积）并入队。
    fn task_for(downloader: &Arc<Downloader>, url: &str, dir: &str, total: Option<u64>) -> DownloadTask {
        DownloadTask {
            id: downloader.allocate_id(),
            status: DownloadStatus::Queued,
            repo: "owner/repo".into(),
            file: "file.gguf".into(),
            target: format!("{dir}/owner/repo/file.gguf"),
            url: url.to_string(),
            total_bytes: total,
            bytes: 0,
            conflict: false,
            status_text: String::new(),
        }
    }

    async fn wait_status(downloader: &StdArc<Downloader>, status: DownloadStatus) {
        for _ in 0..600 {
            if downloader.tasks().iter().any(|task| task.status == status) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("等待状态 {status:?} 超时，当前 {:?}", downloader.tasks());
    }

    async fn wait_all_done(downloader: &StdArc<Downloader>, count: usize) {
        for _ in 0..600 {
            let tasks = downloader.tasks();
            if tasks.len() == count && tasks.iter().all(|task| task.status == DownloadStatus::Done) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("等待全部完成超时，当前 {:?}", downloader.tasks());
    }

    fn temp_dir(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("llama-panel-dl-{tag}-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    /// 字符串路径 → PathBuf（测试里用 join 拼子目录）。
    fn dir_path(dir: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(dir)
    }

    #[tokio::test]
    async fn fresh_download_completes_and_renames() {
        let dir = temp_dir("fresh");
        std::fs::create_dir_all(&dir).unwrap();
        let server = TestServer::spawn(vec![b'A'; 3000], false, None, 0).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(3000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Done).await;
        let final_path = format!("{dir}/owner/repo/file.gguf");
        assert_eq!(std::fs::metadata(&final_path).unwrap().len(), 3000, "改名后的完整文件");
        assert!(!std::path::Path::new(&format!("{final_path}.part")).exists(), "临时文件已改名");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn interrupted_download_resumes_from_part() {
        let dir = temp_dir("resume");
        std::fs::create_dir_all(dir_path(&dir).join("owner/repo")).unwrap();
        let server = TestServer::spawn(vec![b'B'; 3000], false, None, 0).await;
        // 预置 1000 字节的断点文件
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![0u8; 1000]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(3000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Done).await;
        let final_path = format!("{dir}/owner/repo/file.gguf");
        let data = std::fs::read(&final_path).unwrap();
        assert_eq!(data.len(), 3000);
        // 断点前的 1000 字节是预置内容，后 2000 字节来自服务器（Range 从 1000 起）
        assert_eq!(&data[1000..], &vec![b'B'; 2000][..]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn transient_drop_retries_and_completes() {
        let dir = temp_dir("retry");
        std::fs::create_dir_all(&dir).unwrap();
        let server = TestServer::spawn(vec![b'C'; 2000], false, Some(500), 0).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(2000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Done).await;
        let data = std::fs::read(format!("{dir}/owner/repo/file.gguf")).unwrap();
        assert_eq!(data.len(), 2000, "中断后经续传重试补全");
        assert_eq!(&data[500..], &vec![b'C'; 1500][..]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ignored_range_is_a_fatal_error() {
        let dir = temp_dir("no-range");
        std::fs::create_dir_all(dir_path(&dir).join("owner/repo")).unwrap();
        let server = TestServer::spawn(vec![b'D'; 1000], true, None, 0).await;
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![0u8; 100]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(1000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Error).await;
        let task = downloader.tasks().into_iter().find(|_| true).unwrap();
        assert!(task.status_text.contains("不支持分段下载"), "{}", task.status_text);
        // 临时文件保留供用户处置
        assert!(std::path::Path::new(&format!("{dir}/owner/repo/file.gguf.part")).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn part_complete_skips_transfer_and_renames() {
        let dir = temp_dir("complete");
        std::fs::create_dir_all(dir_path(&dir).join("owner/repo")).unwrap();
        // 服务器写坏数据：若发起传输内容会错，断言只依赖改名结果
        let server = TestServer::spawn(vec![b'E'; 500], false, None, 0).await;
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![b'P'; 1000]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(1000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Done).await;
        let data = std::fs::read(format!("{dir}/owner/repo/file.gguf")).unwrap();
        assert_eq!(data, vec![b'P'; 1000], "临时文件已完整时直接改名，不再传输");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stall_aborts_and_keeps_part() {
        let dir = temp_dir("stall");
        std::fs::create_dir_all(&dir).unwrap();
        // 手工搭一个「发一点就挂着」的服务器：发 10 字节后既不续发也不关连接
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            use tokio::io::AsyncReadExt;
            loop {
                if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                    break;
                }
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\nABCDEFGHIJ")
                .await;
            // 之后什么都不发也不关：靠测试里 0.5 秒停滞窗口触发中止
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let (notify, _seen) = collector();
        let downloader =
            StdArc::new(Downloader::new(notify).with_stall(Duration::from_millis(500), 1));
        downloader
            .enqueue(task_for(&downloader, &format!("http://{addr}/file.gguf"), &dir, Some(100000)), None)
            .unwrap();
        wait_status(&downloader, DownloadStatus::Error).await;
        assert!(
            downloader.tasks().iter().any(|task| task.status_text.contains("停滞")),
            "{}",
            downloader.tasks()[0].status_text
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn pause_keeps_part_and_resumes_by_resubmit() {
        let dir = temp_dir("pause");
        std::fs::create_dir_all(&dir).unwrap();
        // 慢流服务器：发 1000 字节后停住（不关连接、不续发），留出取消窗口
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            use tokio::io::AsyncReadExt;
            loop {
                if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                    break;
                }
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n")
                .await;
            let _ = socket.write_all(&vec![b'H'; 1000]).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&downloader, &format!("http://{addr}/file.gguf"), &dir, Some(100000));
        downloader.enqueue(task.clone(), None).unwrap();
        // 等进入下载态并收到数据
        for _ in 0..300 {
            if downloader.tasks().iter().any(|task| task.status == DownloadStatus::Downloading && task.bytes > 0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let id = task.id;
        downloader.pause(id);
        wait_status(&downloader, DownloadStatus::Paused).await;
        assert!(
            std::path::Path::new(&format!("{dir}/owner/repo/file.gguf.part")).exists(),
            "断点文件保留"
        );
        // 同目标再点一次 = 继续：回到队列并重新开传
        downloader.enqueue(task_for(&downloader, &format!("http://{addr}/file.gguf"), &dir, Some(100000)), None).unwrap();
        wait_status(&downloader, DownloadStatus::Downloading).await;
        assert_eq!(downloader.tasks().len(), 1, "同目标不产生第二个任务");
        downloader.remove(id);
        assert!(downloader.tasks().is_empty(), "移除后清掉任务");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn parallel_lanes_cap_and_backfill() {
        let dir = temp_dir("parallel");
        std::fs::create_dir_all(&dir).unwrap();
        // 应答前挂 400ms：留出「3 路在传、第 4 个排队」的观察窗口
        let server = TestServer::spawn(vec![b'F'; 100], false, None, 400).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        for i in 0..PARALLEL_DOWNLOADS + 1 {
            let mut task = task_for(&downloader, &server.url(), &dir, Some(100));
            // 目标各不相同才不会被同目标去重拦下
            task.target = format!("{dir}/owner/repo/file{i}.gguf");
            task.file = format!("file{i}.gguf");
            downloader.enqueue(task, None).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let tasks = downloader.tasks();
        assert_eq!(tasks.iter().filter(|task| task.status == DownloadStatus::Downloading).count(), PARALLEL_DOWNLOADS);
        assert_eq!(tasks.iter().filter(|task| task.status == DownloadStatus::Queued).count(), 1, "超出路数的排队");
        wait_all_done(&downloader, PARALLEL_DOWNLOADS + 1).await;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn target_appearing_midflight_is_conflict_not_overwrite() {
        let dir = temp_dir("conflict");
        std::fs::create_dir_all(dir_path(&dir).join("owner/repo")).unwrap();
        let server = TestServer::spawn(vec![b'G'; 100], false, None, 300).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&downloader, &server.url(), &dir, Some(100));
        downloader.enqueue(task, None).unwrap();
        // 传输窗口里目标位被手工放入同名文件：收尾不得覆盖它
        std::fs::write(format!("{dir}/owner/repo/file.gguf"), b"manual").unwrap();
        wait_status(&downloader, DownloadStatus::Error).await;
        let task = downloader.tasks().into_iter().find(|_| true).unwrap();
        assert!(task.conflict, "目标被占属重名冲突，界面据此给「改名保留」");
        assert_eq!(std::fs::read(format!("{dir}/owner/repo/file.gguf")).unwrap(), b"manual", "已有文件未被覆盖");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn prepare_rejects_duplicate_and_renames_on_request() {
        let dir = temp_dir("prepare");
        std::fs::create_dir_all(dir_path(&dir).join("owner/repo")).unwrap();
        std::fs::write(dir_path(&dir).join("owner/repo/file.gguf"), b"existing").unwrap();
        let (notify, _seen) = collector();
        let downloader = Downloader::new(notify);
        // prepare 的输入走 parse_target：owner/repo/文件名 简写
        let request = DownloadRequest {
            input: "owner/repo/file.gguf".into(),
            dir: dir.clone(),
            mirror_id: crate::mirrors::DEFAULT_MIRROR_ID.into(),
        };

        let err = downloader.prepare(&request, PrepareMode::Skip).await.unwrap_err();
        assert!(err.error.contains("无需重复下载"), "{}", err.error);
        assert_eq!(err.repo, "owner/repo", "错误卡至少知道仓库与文件名");
        // 落盘路径统一 / 分隔
        let expected = format!("{}/owner/repo/file.gguf", dir.replace('\\', "/"));
        assert_eq!(err.existing_path, Some(expected));

        let prepared = downloader.prepare(&request, PrepareMode::Rename).await.unwrap();
        assert_eq!(prepared.task.file, "file (1).gguf");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn failed_task_backfills_queued_lane() {
        let dir = temp_dir("backfill");
        std::fs::create_dir_all(&dir).unwrap();
        let slow = TestServer::spawn(vec![b'J'; 100], false, None, 400).await;
        // 404 应答服务器：任务一开传即致命失败，失败路径腾出的并发位必须补队头
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = dead.accept().await else { break };
                let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
                let _ = socket.shutdown().await;
            }
        });
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        // 3 路慢任务占满，失败任务与第 5 个任务排队；失败补位后第 5 个才下得来
        let mut targets = Vec::new();
        for i in 0..5 {
            let mut task = task_for(&downloader, &slow.url(), &dir, Some(100));
            task.target = format!("{dir}/owner/repo/file{i}.gguf");
            task.file = format!("file{i}.gguf");
            if i == 3 {
                task.url = format!("http://{dead_addr}/file.gguf");
            }
            targets.push(task.target.clone());
            downloader.enqueue(task, None).unwrap();
        }
        for _ in 0..600 {
            let tasks = downloader.tasks();
            if tasks.len() == 5
                && tasks.iter().all(|task| {
                    matches!(task.status, DownloadStatus::Done | DownloadStatus::Error)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let tasks = downloader.tasks();
        assert_eq!(tasks.iter().filter(|task| task.status == DownloadStatus::Done).count(), 4, "排队任务经失败补位全部下完");
        assert_eq!(tasks.iter().filter(|task| task.status == DownloadStatus::Error).count(), 1, "只有 404 任务失败");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn clear_finished_keeps_active_tasks() {
        let dir = temp_dir("clear");
        std::fs::create_dir_all(&dir).unwrap();
        let server = TestServer::spawn(vec![b'H'; 100], false, None, 300).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        downloader.enqueue(task_for(&downloader, &server.url(), &dir, Some(100)), None).unwrap();
        // 落一张错误卡（同输入去重只留一张）
        downloader
            .enqueue_error(
                &DownloadRequest { input: "bad/input".into(), dir: dir.clone(), mirror_id: "hf-mirror".into() },
                String::new(),
                String::new(),
                String::new(),
                "地址无法解析".into(),
                false,
            );
        downloader.clear_finished();
        assert_eq!(downloader.tasks().len(), 1, "传输中的任务不受清空影响");
        wait_status(&downloader, DownloadStatus::Done).await;
        downloader.clear_finished();
        assert!(downloader.tasks().is_empty(), "完成的任务被清掉");
        std::fs::remove_dir_all(&dir).ok();
    }
}
