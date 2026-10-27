//! 连接探测：直连本机 llama-server 判定「服务在不在」。
//! 只关心可达性，无长连接；轮询保持状态新鲜，探测失败一律落 offline 而不抛。

use crate::launcher::split_args;
use crate::settings::{base_url, normalize_dir, params_for, LlamaSettings};

/// 探测端点按兼容顺序：/health 是权威就绪信号（llama-server 加载期回 503、就绪回 200）
/// 且豁免 API 密钥；/api/models、/api/version、/ 只是连通性补充（前两者在设置了
/// API 密钥的服务上要求鉴权，/ 在隐藏 WebUI 时不存在）。
const PROBE_PATHS: [&str; 4] = ["/health", "/api/models", "/api/version", "/"];

/// 单次探测超时：本机服务，慢于此即视作不可用。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 探测一次；任一端点 2xx = 服务在线。单点失败（超时/拒绝）试下一个，全失败才判 offline。
/// 附加参数声明了 API 密钥（--api-key）时随请求携带，密钥保护的端点不再误判离线。
pub async fn probe_once(client: &reqwest::Client, settings: &LlamaSettings) -> bool {
    let base = base_url(settings);
    let api_key = api_key_of(settings);
    for path in PROBE_PATHS {
        let mut request = client.get(format!("{base}{path}")).timeout(PROBE_TIMEOUT);
        if let Some(key) = &api_key {
            request = request.bearer_auth(key);
        }
        if let Ok(response) = request.send().await {
            if response.status().is_success() {
                return true;
            }
        }
    }
    false
}

/// 当前模型声明的 API 密钥：优先取专用的「API 密钥」字段，未填再从附加参数里认
/// `--api-key 值` 或 `--api-key=值`（旧写法兼容）；都没有返回 None。
fn api_key_of(settings: &LlamaSettings) -> Option<String> {
    let model_dir = normalize_dir(&settings.model_file);
    let dir = model_dir.rfind('/').map(|idx| &model_dir[..idx]).unwrap_or("");
    let params = params_for(settings, dir);
    let dedicated = params.api_key.trim();
    if !dedicated.is_empty() {
        return Some(dedicated.to_string());
    }
    let args = split_args(&params.extra_args);
    let spaced = args
        .iter()
        .position(|arg| arg == "--api-key")
        .and_then(|idx| args.get(idx + 1))
        .cloned();
    spaced.or_else(|| {
        args.iter()
            .find_map(|arg| arg.strip_prefix("--api-key=").map(Into::into))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with_extra(extra: &str) -> LlamaSettings {
        let mut settings = LlamaSettings {
            model_file: "C:/models/q/m.gguf".into(),
            ..Default::default()
        };
        settings
            .model_params
            .insert("C:/models/q".into(), crate::settings::ModelLaunchParams {
                extra_args: extra.into(),
                ..Default::default()
            });
        settings
    }

    #[test]
    fn api_key_read_from_extra_args() {
        assert_eq!(api_key_of(&settings_with_extra("--api-key sk-123")), Some("sk-123".into()));
        assert_eq!(api_key_of(&settings_with_extra("--api-key=sk-123")), Some("sk-123".into()));
        assert_eq!(
            api_key_of(&settings_with_extra("--verbose --api-key \"a b\" --jinja")),
            Some("a b".into()),
            "带引号的密钥按整体取值"
        );
        assert_eq!(api_key_of(&settings_with_extra("--jinja")), None);
        assert_eq!(api_key_of(&settings_with_extra("--api-key")), None, "缺值不误取旗标");
    }

    #[test]
    fn dedicated_api_key_field_wins_over_extra_args() {
        let mut settings = settings_with_extra("--api-key sk-from-extra");
        settings
            .model_params
            .get_mut("C:/models/q")
            .unwrap()
            .api_key = "sk-dedicated".into();
        assert_eq!(api_key_of(&settings), Some("sk-dedicated".into()));
        // 专用字段留空时回落到附加参数里的旧写法
        settings
            .model_params
            .get_mut("C:/models/q")
            .unwrap()
            .api_key = "  ".into();
        assert_eq!(api_key_of(&settings), Some("sk-from-extra".into()));
    }
}
