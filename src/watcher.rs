//! 外部调用自动启动的待命监听：服务未运行时由插件进程直接占住服务端口。
//!
//! 触发契约：仅真实 API 调用（POST/PUT/PATCH/DELETE 与 CORS 预检 OPTIONS）触发启动，
//! GET 类探测请求不触发、只落日志；触发连接由插件保持，等 llama-server 就绪
//! （/health 不再返回 503，最长 3 分钟）后原样转发、双向回传。
//! 插件停用即随进程退出，端口自然释放；`singleton` 声明保证全局只有一个插件实例，无端口竞争。

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};

use crate::settings::LlamaSettings;

/// 触发请求就绪等待上限（大模型加载会慢，但不无限等）。
const WAIT_READY: Duration = Duration::from_secs(180);
/// 就绪轮询间隔。
const READY_POLL: Duration = Duration::from_millis(500);
/// 单次就绪探测超时。
const READY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// 转发空闲上限：双向任一方向持续无数据达此时长即收场（客户端重试即可）。
const RELAY_IDLE: Duration = Duration::from_secs(300);
/// 请求头收集上限：超限按垃圾请求丢弃。
const MAX_HEAD: usize = 64 * 1024;
/// 头部收集超时：超时未收齐按垃圾请求丢弃。
const HEAD_TIMEOUT: Duration = Duration::from_secs(2);

/// 待命回调：插件侧的状态收口（日志 / 触发启动）。
#[derive(Clone)]
pub struct WatchHooks {
    /// 触发请求首行与调用方 User-Agent（日志定位用）。
    pub on_trigger: Arc<dyn Fn(String, String) + Send + Sync>,
    /// 待命期间收到的非触发请求（噪音），仅落日志。
    pub on_noise: Arc<dyn Fn(String) + Send + Sync>,
}

/// 待命监听持有者。arm/disarm 串行执行；arm 状态由持有者自持。
pub struct Watcher {
    state: Mutex<Option<Armed>>,
}

struct Armed {
    /// 停收信号：disarm 与「触发后让出端口」都经它结束 accept 循环（监听器随循环释放）。
    cancel: watch::Sender<bool>,
    host: String,
    port: u16,
}

impl Default for Watcher {
    fn default() -> Self {
        Self { state: Mutex::new(None) }
    }
}

impl Watcher {
    /// 进入待命：绑定服务端口并开始 accept。已在同一地址待命 = 无操作；否则先解除旧监听。
    /// 绑定失败返回可读原因（端口被占用等）。
    pub async fn arm(&self, settings: &LlamaSettings, hooks: WatchHooks) -> Result<(), String> {
        let host = if settings.host.trim().is_empty() { "127.0.0.1".to_string() } else { settings.host.trim().to_string() };
        let port = settings.port;
        if let Some(armed) = self.state.lock().await.as_ref() {
            if armed.host == host && armed.port == port {
                return Ok(());
            }
        }
        self.disarm().await;
        // 回环地址与 llama-server 同绑 127.0.0.1；局域网地址绑 0.0.0.0：绑特定 IP 收不到
        // 本机回环调用，DHCP 换址也会失效
        let bind_addr = if host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        };
        let listener = TcpListener::bind((bind_addr, port))
            .await
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::AddrInUse {
                    "端口已被占用".to_string()
                } else {
                    format!("待命监听绑定失败：{err}")
                }
            })?;
        let server_host = if host == "0.0.0.0" { "127.0.0.1".to_string() } else { host.clone() };
        let (cancel_tx, cancel_rx) = watch::channel(false);
        tokio::spawn(accept_loop(listener, cancel_rx, server_host, port, hooks));
        self.state.lock().await.replace(Armed { cancel: cancel_tx, host, port });
        Ok(())
    }

    /// 解除待命。返回是否真的解除了一个在待命的监听（供调用方决定要不要记日志）。
    /// 已被触发进入转发的连接不受影响（转发任务自持）。
    pub async fn disarm(&self) -> bool {
        match self.state.lock().await.take() {
            Some(armed) => {
                let _ = armed.cancel.send(true);
                true
            }
            None => false,
        }
    }
}

/// accept 主循环：逐连接分类；触发即停收新连接（端口让给 llama-server）并转入转发。
async fn accept_loop(
    listener: TcpListener,
    mut cancel: watch::Receiver<bool>,
    server_host: String,
    port: u16,
    hooks: WatchHooks,
) {
    loop {
        let (socket, _) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                // accept 持续报错（fd 耗尽等）时退避，避免紧循环空转
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            },
            changed = cancel.changed() => {
                // disarm 或已触发让出：循环结束，监听器就地释放
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
                continue;
            }
        };
        let Some((socket, buf, head_end)) = collect_head(socket).await else { continue };
        let text = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let first = text.lines().next().unwrap_or_default().trim().to_string();
        let (method, _path) = split_request_line(&first);
        if !is_trigger_method(&method) {
            (hooks.on_noise)(first);
            // 非触发请求不服务：直接断开
            let mut socket = socket;
            let _ = socket.shutdown().await;
            continue;
        }
        let ua = extract_user_agent(&text);
        // 触发：停收新连接（循环结束即释放监听器，端口随后交给 llama-server），转发该连接
        (hooks.on_trigger)(first, ua);
        tokio::spawn(relay(socket, buf, server_host, port));
        return;
    }
}

/// 收集请求头：返回（连接，含同包到达的主体字节在内的完整缓冲，头结束偏移）。
/// 主体字节必须保留——触发请求的 body 常与头部同包到达，转发时缺了它客户端会挂等。
async fn collect_head(mut socket: TcpStream) -> Option<(TcpStream, Vec<u8>, usize)> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + HEAD_TIMEOUT;
    loop {
        let read = tokio::select! {
            read = socket.read(&mut buf) => read.ok()?,
            _ = tokio::time::sleep_until(deadline) => return None,
        };
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&buf[..read]);
        // 终止符按「包含」判（首包可能一并带来主体字节）
        if let Some(end) = find_head_end(&head) {
            return Some((socket, head, end));
        }
        if head.len() > MAX_HEAD {
            return None;
        }
    }
}

/// 请求头结束位置（`\r\n\r\n` 优先于 `\n\n`；都不含返回 None）。
fn find_head_end(head: &[u8]) -> Option<usize> {
    head.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| head.windows(2).position(|w| w == b"\n\n"))
}

/// 拆请求行：返回大写方法与去查询串的路径。
fn split_request_line(first: &str) -> (String, String) {
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_uppercase();
    let target = parts.next().unwrap_or_default();
    let path = target.split('?').next().unwrap_or_default().to_string();
    (method, path)
}

/// 仅真实 API 调用触发：POST/PUT/PATCH/DELETE 与 CORS 预检 OPTIONS；
/// GET 类探测、无方法行（垃圾请求）一律不触发。
fn is_trigger_method(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS")
}

fn extract_user_agent(head: &str) -> String {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("user-agent").then(|| value.trim().to_string())
        })
        .unwrap_or_default()
}

/// 保持触发连接：轮询 /health 直到 llama-server 就绪（或超时 / 客户端离开），
/// 然后把已收到的请求（头 + 可能同包到达的主体前缀）原样转发并双向回传。
async fn relay(mut client: TcpStream, head_and_body: Vec<u8>, server_host: String, port: u16) {
    // 已收到的字节先攒着；客户端可能还有主体数据在路上，一并攒入
    let mut pending = head_and_body;
    let mut buf = [0u8; 8192];
    let deadline = tokio::time::Instant::now() + WAIT_READY;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return; // 客户端重试即可
        }
        tokio::select! {
            read = client.read(&mut buf) => {
                match read {
                    Ok(0) => return, // 客户端离开了
                    Ok(n) => pending.extend_from_slice(&buf[..n]),
                    Err(_) => return,
                }
            }
            _ = tokio::time::sleep(READY_POLL) => {}
        }
        if server_ready(&server_host, port).await {
            break;
        }
    }
    let Ok(mut upstream) = TcpStream::connect((server_host.as_str(), port)).await else {
        return;
    };
    if upstream.write_all(&pending).await.is_err() {
        return;
    }
    // 双向回传：任一方向持续无数据达空闲上限即收场
    let mut upstream_buf = [0u8; 8192];
    let mut last_activity = tokio::time::Instant::now();
    loop {
        if last_activity.elapsed() > RELAY_IDLE {
            return;
        }
        tokio::select! {
            read = client.read(&mut buf) => match read {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if upstream.write_all(&buf[..n]).await.is_err() { return; }
                    last_activity = tokio::time::Instant::now();
                }
            },
            read = upstream.read(&mut upstream_buf) => match read {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if client.write_all(&upstream_buf[..n]).await.is_err() { return; }
                    last_activity = tokio::time::Instant::now();
                }
            },
        }
    }
}

/// 就绪探测：/health 的状态行非空且不含 503 = 服务就绪（llama-server 加载模型期间回 503）。
async fn server_ready(server_host: &str, port: u16) -> bool {
    let probe = tokio::time::timeout(READY_PROBE_TIMEOUT, async {
        let mut socket = TcpStream::connect((server_host, port)).await.ok()?;
        socket.write_all(b"GET /health HTTP/1.0\r\n\r\n").await.ok()?;
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(1), socket.read_to_end(&mut buf)).await;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let first = text.lines().next().unwrap_or_default().to_string();
        (!first.is_empty() && !first.contains(" 503 ")).then_some(())
    })
    .await;
    probe.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_method_filter() {
        for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS", "post"] {
            assert!(is_trigger_method(&method.to_uppercase()), "{method} 应触发");
        }
        for method in ["GET", "HEAD", "", "TRACE"] {
            assert!(!is_trigger_method(method), "{method} 不应触发");
        }
    }

    #[test]
    fn request_line_splits_method_and_path() {
        let (method, path) = split_request_line("POST /v1/chat/completions?x=1 HTTP/1.1");
        assert_eq!(method, "POST");
        assert_eq!(path, "/v1/chat/completions");
        let (method, path) = split_request_line("GET /health");
        assert_eq!(method, "GET");
        assert_eq!(path, "/health");
        assert_eq!(split_request_line("").0, "");
    }

    #[test]
    fn user_agent_is_case_insensitive() {
        let head = "POST /v1 HTTP/1.1\r\nHOST: x\r\nUSER-AGENT:  my-client/1.0\r\n\r\n";
        assert_eq!(extract_user_agent(head), "my-client/1.0");
        assert_eq!(extract_user_agent("POST / HTTP/1.1\r\n\r\n"), "");
    }

    #[tokio::test]
    async fn arm_binds_and_disarm_releases_port() {
        use crate::settings::LlamaSettings;
        let hooks = WatchHooks { on_trigger: Arc::new(|_, _| {}), on_noise: Arc::new(|_| {}) };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let settings = LlamaSettings { port, ..Default::default() };
        let watcher = Watcher::default();
        watcher.arm(&settings, hooks.clone()).await.unwrap();
        assert!(watcher.disarm().await);
        assert!(!watcher.disarm().await, "二次解除 = 无操作");
    }

    #[tokio::test]
    async fn arm_fails_readably_when_port_is_taken() {
        use crate::settings::LlamaSettings;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let settings = LlamaSettings { port, ..Default::default() };
        let hooks = WatchHooks { on_trigger: Arc::new(|_, _| {}), on_noise: Arc::new(|_| {}) };
        let watcher = Watcher::default();
        let err = watcher.arm(&settings, hooks).await.unwrap_err();
        assert!(err.contains("端口已被占用"), "{err}");
    }

    #[tokio::test]
    async fn noise_requests_logged_and_trigger_forwarded() {
        use crate::settings::LlamaSettings;
        // 假 llama-server：健康后回显请求
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _upstream_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = upstream.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let ready = !head.contains("503");
                    let reply = if head.starts_with("GET /health") {
                        if ready { "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok" } else { "HTTP/1.1 503 Loading\r\nContent-Length: 0\r\n\r\n" }
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\necho-ok"
                    };
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });

        let watcher_port_bind = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let watch_port = watcher_port_bind.local_addr().unwrap().port();
        drop(watcher_port_bind);

        let triggered: Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
        let noise: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let t_sink = triggered.clone();
        let n_sink = noise.clone();
        let hooks = WatchHooks {
            on_trigger: Arc::new(move |line, ua| t_sink.lock().unwrap().push((line, ua))),
            on_noise: Arc::new(move |line| n_sink.lock().unwrap().push(line)),
        };

        let settings = LlamaSettings { port: watch_port, ..Default::default() };
        let watcher = Watcher::default();
        watcher.arm(&settings, hooks).await.unwrap();

        // GET 探测：落噪音、不断链回包也不触发
        let mut client = TcpStream::connect(("127.0.0.1", watch_port)).await.unwrap();
        client.write_all(b"GET /api/models HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(noise.lock().unwrap().iter().any(|l| l.contains("GET /api/models")));
        assert!(triggered.lock().unwrap().is_empty(), "GET 不触发");

        // POST：触发后由 relay 轮询 /health 就绪再转发
        let mut client2 = TcpStream::connect(("127.0.0.1", watch_port)).await.unwrap();
        client2
            .write_all(b"POST /v1/chat/completions HTTP/1.1\r\nUser-Agent: tester\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        {
            let trig = triggered.lock().unwrap();
            assert_eq!(trig.len(), 1);
            assert!(trig[0].0.contains("POST /v1/chat/completions"));
            assert_eq!(trig[0].1, "tester");
        }
        // 触发后端口让出：新连接应被拒绝（llama-server 接管前的窗口）
        watcher.disarm().await;
        let _ = client2.shutdown().await;
    }
}
