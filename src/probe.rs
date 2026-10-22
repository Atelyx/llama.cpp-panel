//! 连接探测：直连本机 llama-server 判定「服务在不在」。
//! 只关心可达性，无长连接；轮询保持状态新鲜，探测失败一律落 offline 而不抛。

use crate::settings::{base_url, LlamaSettings};

/// 探测端点按版本兼容顺序：/api/models → /api/version /（Web UI）。
const PROBE_PATHS: [&str; 3] = ["/api/models", "/api/version", "/"];

/// 单次探测超时：本机服务，慢于此即视作不可用。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 探测一次；任一端点 2xx = 服务在线。单点失败（超时/拒绝）试下一个，全失败才判 offline。
pub async fn probe_once(client: &reqwest::Client, settings: &LlamaSettings) -> bool {
    let base = base_url(settings);
    for path in PROBE_PATHS {
        let url = format!("{base}{path}");
        if let Ok(response) = client.get(&url).timeout(PROBE_TIMEOUT).send().await {
            if response.status().is_success() {
                return true;
            }
        }
    }
    false
}
