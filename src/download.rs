//! 模型下载：单任务状态机（一次一个文件），只负责发起请求、看进度、收尾改名。
//!
//! 落盘到 `模型目录/发布者/模型名/`，同仓库的量化与投影同处一个文件夹，便于按文件夹配上 mmproj；
//! 仓库内子目录的文件压平成单段名。先写 `<文件名>.part`，下完才改名 `.gguf`——模型扫描因此
//! 看不到半截文件。取消只中止传输并保留 `.part`，再开始时从断点（Range）继续。
//!
//! 时序守卫（世代号）：取消与被新下载取代是两条独立路径，世代号在首个 await 之前自增；
//! 旧任务的迟到回调发现世代号不符即放弃，不得覆盖新状态。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use crate::mirrors::unique_name;
use crate::modelscan::file_name_of;

/// 下载任务状态。`paused` 表示已取消但 `.part` 保留，可继续。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    Idle,
    Downloading,
    Paused,
    Done,
    Error,
}

/// 下载任务快照（界面消费）。
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadTask {
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
    pub status_text: String,
}

impl DownloadTask {
    pub(crate) fn idle() -> Self {
        Self {
            status: DownloadStatus::Idle,
            repo: String::new(),
            file: String::new(),
            target: String::new(),
            url: String::new(),
            total_bytes: None,
            bytes: 0,
            status_text: String::new(),
        }
    }
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
}

/// 状态变化回调：每次快照变化调用一次（界面层据此刷新）。
pub type Notify = Arc<dyn Fn(DownloadTask) + Send + Sync>;

/// 传输停滞判定：时间窗内增量字节低于阈值即中止（模型下载不值得无限等死连接）。
#[derive(Clone, Copy)]
struct StallConfig {
    window: Duration,
    min_bytes: u64,
}

/// 下载执行器。同一时刻至多一个任务在跑；全部落盘都在插件进程内直写。
pub struct Downloader {
    client: reqwest::Client,
    task: Arc<Mutex<DownloadTask>>,
    token: Arc<AtomicU64>,
    cancel_tx: watch::Sender<bool>,
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
        let (cancel_tx, _) = watch::channel(false);
        Self {
            client,
            task: Arc::new(Mutex::new(DownloadTask::idle())),
            token: Arc::new(AtomicU64::new(0)),
            cancel_tx,
            notify,
            stall: StallConfig { window: Duration::from_secs(60), min_bytes: 1024 * 60 },
        }
    }

    #[cfg(test)]
    fn with_stall(mut self, window: Duration, min_bytes: u64) -> Self {
        self.stall = StallConfig { window, min_bytes };
        self
    }

    fn current(&self) -> DownloadTask {
        self.task.lock().unwrap().clone()
    }

    fn set(&self, next: DownloadTask) {
        *self.task.lock().unwrap() = next.clone();
        (self.notify)(next);
    }

    /// 当前任务是否在下载（界面据此禁用重复开始）。
    pub fn is_downloading(&self) -> bool {
        self.task.lock().unwrap().status == DownloadStatus::Downloading
    }

    /// 预检：解析地址、确认落盘名与远端体积。该模型文件夹里已有同名文件即判定为已下载并拒绝
    /// （重下几个 GB 不可接受）；界面据此提供「改名保留」入口（`mode = Rename`）。
    pub async fn prepare(&self, req: &DownloadRequest, mode: PrepareMode) -> PrepareResult {
        let parsed = crate::mirrors::parse_target(&req.input).map_err(|error| PrepareError {
            error,
            existing_path: None,
        })?;
        let dir = req.dir.trim().replace('\\', "/");
        let dir = dir.trim_end_matches('/').to_string();
        if dir.is_empty() {
            return Err(PrepareError {
                error: "未设置模型目录，请在设置页选择".into(),
                existing_path: None,
            });
        }
        // 模型根目录必须先存在：设置写错时给明确原因，而下面的仓库子目录允许还没建
        std::fs::read_dir(&dir).map_err(|err| PrepareError {
            error: format!("读取模型目录失败：{err}（目录不存在或不是目录）"),
            existing_path: None,
        })?;

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
                status: DownloadStatus::Idle,
                repo: parsed.repo,
                target: format!("{repo_dir}/{final_name}"),
                file: final_name,
                url,
                total_bytes,
                bytes,
                status_text: String::new(),
            },
        })
    }

    /// 开始下载预检过的任务；进行中重复调用 = 无操作。
    pub fn start(self: &Arc<Self>, prepared: DownloadTask) {
        if self.is_downloading() {
            return;
        }
        let token = self.token.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel_tx.send(false);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run(token, prepared).await;
        });
    }

    /// 取消下载：中止传输，保留 `.part` 供续传。
    pub fn cancel(&self) {
        if self.is_downloading() {
            self.token.fetch_add(1, Ordering::SeqCst);
            let _ = self.cancel_tx.send(true);
            let mut task = self.current();
            task.status = DownloadStatus::Paused;
            task.status_text = "已暂停；再次下载从断点继续".into();
            self.set(task);
        }
    }

    /// 清理已完成/失败/暂停的任务卡片。
    pub fn dismiss(&self) {
        if self.is_downloading() {
            return;
        }
        self.set(DownloadTask::idle());
    }

    async fn run(self: Arc<Self>, token: u64, prepared: DownloadTask) {
        let part_path = format!("{}.part", prepared.target);
        let part_size = file_size(&part_path);
        // 临时文件已达完整体积（上次下载完了但改名失败）：直接改名，重发请求只会得到 Range 错误
        if let (Some(size), Some(total)) = (part_size, prepared.total_bytes) {
            if size >= total {
                let mut task = prepared.clone();
                task.status = DownloadStatus::Downloading;
                task.bytes = size;
                task.status_text = "文件已完整，正在改名落盘…".into();
                self.set_if_current(token, task);
                self.finish(token, &prepared, size).await;
                return;
            }
        }

        let mut task = prepared.clone();
        task.status = DownloadStatus::Downloading;
        task.bytes = part_size.unwrap_or(0);
        task.status_text = if task.bytes > 0 { "从断点继续…".into() } else { "启动下载…".into() };
        self.set_if_current(token, task);

        let mut attempt = 0u32;
        loop {
            if self.token.load(Ordering::SeqCst) != token {
                return; // 已被取消或被新下载取代
            }
            let part_now = file_size(&part_path).unwrap_or(0);
            let outcome = self
                .attempt_once(token, &prepared, &part_path, part_now)
                .await;
            match outcome {
                AttemptOutcome::Completed => {
                    self.finish(token, &prepared, file_size(&part_path).unwrap_or(0)).await;
                    return;
                }
                AttemptOutcome::Fatal(message) => {
                    self.fail(token, message);
                    return;
                }
                AttemptOutcome::Transient(message) => {
                    attempt += 1;
                    if attempt > RETRIES {
                        self.fail(token, message);
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    /// 一次传输尝试。返回三类结局：完成 / 致命失败 / 可重试失败。
    async fn attempt_once(
        &self,
        token: u64,
        prepared: &DownloadTask,
        part_path: &str,
        part_size: u64,
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

        let mut cancel_rx = self.cancel_tx.subscribe();
        let mut stream = response;
        let mut bytes = part_size;
        let mut window_bytes = part_size;
        let mut window_start = Instant::now();
        let mut last_notify = Instant::now();
        let mut notify_bytes = bytes;

        loop {
            if self.token.load(Ordering::SeqCst) != token {
                return AttemptOutcome::Fatal(String::new());
            }
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
                        let mut task = prepared.clone();
                        task.status = DownloadStatus::Downloading;
                        task.bytes = bytes;
                        task.status_text = progress_text(bytes, prepared.total_bytes, &format_rate(rate));
                        self.set_if_current(token, task);
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
                    let mut task = prepared.clone();
                    task.status = DownloadStatus::Downloading;
                    task.bytes = bytes;
                    task.status_text = progress_text(bytes, prepared.total_bytes, &format_rate(rate));
                    self.set_if_current(token, task);
                }
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        let _ = file.flush().await;
                        return AttemptOutcome::Fatal(String::new());
                    }
                }
            }
        }
        if file.flush().await.is_err() || file.sync_all().await.is_err() {
            return AttemptOutcome::Fatal("写文件失败：落盘校验失败".into());
        }
        if self.token.load(Ordering::SeqCst) != token {
            return AttemptOutcome::Fatal(String::new());
        }
        AttemptOutcome::Completed
    }

    /// 完成收尾：`.part` 改名为最终文件。改名失败给出可操作指引并保留临时文件。
    async fn finish(&self, token: u64, prepared: &DownloadTask, size: u64) {
        if self.token.load(Ordering::SeqCst) != token {
            return;
        }
        let part_path = format!("{}.part", prepared.target);
        let final_name = file_name_of(&prepared.target);
        if let Err(err) = tokio::fs::rename(&part_path, &prepared.target).await {
            let message = format!(
                "下载已完成但改名失败：{err}。文件已完整保存在 {part_path}，改名为 {final_name} 即可使用。"
            );
            self.fail(token, message);
            return;
        }
        let mut task = prepared.clone();
        task.status = DownloadStatus::Done;
        task.bytes = size;
        task.status_text = format!("已保存到 {}（{}）", prepared.target, crate::modelscan::format_size(Some(size)));
        self.set(task);
    }

    fn fail(&self, token: u64, message: String) {
        if self.token.load(Ordering::SeqCst) != token {
            return; // 旧任务的错误不该盖到新任务上
        }
        let mut task = self.current();
        // 空消息 = 主动取消：状态已由 cancel() 落定，这里不再覆盖
        if message.is_empty() {
            return;
        }
        task.status = DownloadStatus::Error;
        task.status_text = message;
        self.set(task);
    }

    fn set_if_current(&self, token: u64, task: DownloadTask) {
        if self.token.load(Ordering::SeqCst) == token {
            self.set(task);
        }
    }
}

enum AttemptOutcome {
    Completed,
    /// 空消息 = 被取消或被取代（不落状态）。
    Fatal(String),
    Transient(String),
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
    struct TestServer {
        url: String,
    }

    impl TestServer {
        async fn spawn(body: Vec<u8>, ignore_range: bool, truncate_at: Option<usize>) -> Self {
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
        let notify: Notify = Arc::new(move |task| sink.lock().unwrap().push(task));
        (notify, seen)
    }

    fn req(url: &str, dir: &str) -> DownloadRequest {
        DownloadRequest {
            input: url.to_string(),
            dir: dir.to_string(),
            mirror_id: crate::mirrors::DEFAULT_MIRROR_ID.into(),
        }
    }

    /// 测试里 prepare 的地址解析不适用裸 URL（须 gguf 且可解析），直接构造任务。
    fn task_for(url: &str, dir: &str, total: Option<u64>) -> DownloadTask {
        DownloadTask {
            status: DownloadStatus::Idle,
            repo: "owner/repo".into(),
            file: "file.gguf".into(),
            target: format!("{dir}/owner/repo/file.gguf"),
            url: url.to_string(),
            total_bytes: total,
            bytes: 0,
            status_text: String::new(),
        }
    }

    #[tokio::test]
    async fn fresh_download_completes_and_renames() {
        let dir = temp_dir("fresh");
        std::fs::create_dir_all(&dir).unwrap();
        let server = TestServer::spawn(vec![b'A'; 3000], false, None).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&server.url(), &dir, Some(3000));
        downloader.start(task);
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
        let server = TestServer::spawn(vec![b'B'; 3000], false, None).await;
        // 预置 1000 字节的断点文件
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![0u8; 1000]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&server.url(), &dir, Some(3000));
        downloader.start(task);
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
        let server = TestServer::spawn(vec![b'C'; 2000], false, Some(500)).await;
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&server.url(), &dir, Some(2000));
        downloader.start(task);
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
        let server = TestServer::spawn(vec![b'D'; 1000], true, None).await;
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![0u8; 100]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&server.url(), &dir, Some(1000));
        downloader.start(task);
        wait_status(&downloader, DownloadStatus::Error).await;
        let task = downloader.current();
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
        let server = TestServer::spawn(vec![b'E'; 500], false, None).await;
        std::fs::write(format!("{dir}/owner/repo/file.gguf.part"), vec![b'P'; 1000]).unwrap();
        let (notify, _seen) = collector();
        let downloader = StdArc::new(Downloader::new(notify));
        let task = task_for(&server.url(), &dir, Some(1000));
        downloader.start(task);
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
        let downloader = StdArc::new(
            Downloader::new(notify).with_stall(Duration::from_millis(500), 1),
        );
        let task = task_for(&format!("http://{addr}/file.gguf"), &dir, Some(100000));
        downloader.start(task);
        wait_status(&downloader, DownloadStatus::Error).await;
        assert!(downloader.current().status_text.contains("停滞"), "{}", downloader.current().status_text);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancel_keeps_part_and_reports_paused() {
        let dir = temp_dir("cancel");
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
        let task = task_for(&format!("http://{addr}/file.gguf"), &dir, Some(100000));
        downloader.start(task);
        wait_downloading(&downloader).await;
        downloader.cancel();
        wait_status(&downloader, DownloadStatus::Paused).await;
        let task = downloader.current();
        assert!(task.bytes >= 1000, "已收到的字节计入进度：{}", task.bytes);
        assert!(
            std::path::Path::new(&format!("{dir}/owner/repo/file.gguf.part")).exists(),
            "断点文件保留"
        );
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
        let request = req("owner/repo/file.gguf", &dir);

        let err = downloader.prepare(&request, PrepareMode::Skip).await.unwrap_err();
        assert!(err.error.contains("无需重复下载"), "{}", err.error);
        // 落盘路径统一 / 分隔
        let expected = format!("{}/owner/repo/file.gguf", dir.replace('\\', "/"));
        assert_eq!(err.existing_path, Some(expected));

        let prepared = downloader.prepare(&request, PrepareMode::Rename).await.unwrap();
        assert_eq!(prepared.task.file, "file (1).gguf");
        std::fs::remove_dir_all(&dir).ok();
    }

    async fn wait_status(downloader: &StdArc<Downloader>, status: DownloadStatus) {
        for _ in 0..600 {
            if downloader.current().status == status {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("等待状态 {status:?} 超时，当前 {:?}", downloader.current());
    }

    async fn wait_downloading(downloader: &StdArc<Downloader>) {
        for _ in 0..600 {
            if downloader.is_downloading() && downloader.current().bytes > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等待进入下载态超时，当前 {:?}", downloader.current());
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
}
