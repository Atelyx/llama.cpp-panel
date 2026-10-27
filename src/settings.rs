//! 插件设置：`state` 能力面的单对象持久化。
//! 缺字段一律补默认值；读取时收敛非法值（手改或半写的脏数据不把插件带偏）。

use serde::{Deserialize, Serialize};

/// 单个模型文件夹的启动参数（表单值，随模型文件夹持久化）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelLaunchParams {
    /// 选中的量化文件名（相对模型文件夹）。
    pub quant: String,
    /// 选中的视觉投影文件名（相对；空 = 不挂投影）。
    pub mmproj: String,
    /// GPU 卸载层数（-ngl）；None = 不传该参数。
    pub ngl: Option<u32>,
    /// 上下文长度（--ctx-size）；None = 不传。
    pub ctx_size: Option<u32>,
    /// 生成线程数（--threads）；None = 不传。
    pub threads: Option<u32>,
    /// 并行槽位数（--parallel）；None = 不传。
    pub parallel: Option<u32>,
    /// KV 缓存 K 量化类型（--cache-type-k）；空 = 不传。
    pub cache_type_k: String,
    /// Flash Attention（--flash-attn on）。
    pub flash_attn: bool,
    /// Jinja 聊天模板（--jinja）。
    pub jinja: bool,
    /// 隐藏内置 WebUI（--no-webui）。
    pub no_webui: bool,
    /// 模型载入模式（--load-mode）；空 = 不传。
    pub load_mode: String,
    /// API 密钥（--api-key）；空 = 不启用。连接探测按它自动携带，避免把密钥混进附加参数。
    pub api_key: String,
    /// 其余附加参数，原样追加在末尾。
    pub extra_args: String,
}

/// 插件设置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LlamaSettings {
    /// llama-server 监听地址（默认本机）。
    pub host: String,
    /// 监听端口：连接探测目标，也是托管启动传给 llama-server 的端口。
    pub port: u16,
    /// llama-server 所在文件夹：启动时在其中按平台名查找可执行文件。
    pub server_dir: String,
    /// 模型目录：扫描 *.gguf 的根目录。
    pub models_dir: String,
    /// 启动用的模型文件（全路径），由面板选中模型的量化选择写入。
    pub model_file: String,
    /// 视觉投影文件（全路径，可选）；设置后启动时追加 --mmproj。
    pub mmproj_file: String,
    /// 外部调用自动启动：服务未运行时插件占住服务端口，检测到真实调用即自动启动。
    pub auto_start_on_api_call: bool,
    /// 模型下载用的镜像站 id（`mirrors::MIRRORS`）。
    pub download_mirror: String,
    /// 各模型文件夹的启动参数，key = 归一后的模型文件夹全路径。
    pub model_params: std::collections::BTreeMap<String, ModelLaunchParams>,
}

impl Default for LlamaSettings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            server_dir: String::new(),
            models_dir: String::new(),
            model_file: String::new(),
            mmproj_file: String::new(),
            auto_start_on_api_call: false,
            download_mirror: crate::mirrors::DEFAULT_MIRROR_ID.into(),
            model_params: Default::default(),
        }
    }
}

/// KV 缓存 K 量化的合法取值（未知值收敛为不传）。
pub const KV_K_TYPES: [&str; 7] = ["f16", "bf16", "q8_0", "q5_1", "q5_0", "q4_1", "q4_0"];
/// 载入模式的合法取值（未知值收敛为不传）。
pub const LOAD_MODES: [&str; 6] = ["auto", "none", "mmap", "mlock", "mmap+mlock", "dio"];

/// 状态文件形状（顶层带版本号）。
#[derive(Serialize, Deserialize, Default)]
struct PluginState {
    version: u32,
    settings: Option<LlamaSettings>,
}

const STATE_VERSION: u32 = 1;

impl LlamaSettings {
    /// 收敛非法值：端口越界、镜像 id 不存在、量化/载入模式拼写不对等回落默认或剔除。
    pub fn sanitize(mut self) -> Self {
        let host = self.host.trim().to_string();
        self.host = if host.is_empty() { "127.0.0.1".into() } else { host };
        self.server_dir = self.server_dir.trim().to_string();
        self.models_dir = self.models_dir.trim().to_string();
        self.model_file = self.model_file.trim().to_string();
        self.mmproj_file = self.mmproj_file.trim().to_string();
        if !crate::mirrors::MIRRORS.iter().any(|m| m.id == self.download_mirror) {
            self.download_mirror = crate::mirrors::DEFAULT_MIRROR_ID.into();
        }
        self.model_params.retain(|key, params| {
            if !KV_K_TYPES.contains(&params.cache_type_k.as_str()) {
                params.cache_type_k.clear();
            }
            if !LOAD_MODES.contains(&params.load_mode.as_str()) {
                params.load_mode.clear();
            }
            !key.trim().is_empty()
        });
        self
    }
}

/// 读取设置。返回（设置，读取是否成功）：
/// - 读取成功但形状不认识（手改或半写）：按默认值起用（不把半截状态带偏），照常可写回；
/// - 能力面读取失败：按默认值起用且返回 `false`，调用方应暂停落盘，
///   防止以默认值覆盖磁盘上尚好的设置。
pub async fn load(ctx: &atelyx_plugin_sdk::Context) -> (LlamaSettings, bool) {
    match ctx.state_read().await {
        Ok(value) => match serde_json::from_value::<PluginState>(value) {
            Ok(state) => (state.settings.map(LlamaSettings::sanitize).unwrap_or_default(), true),
            Err(err) => {
                eprintln!("设置形状无法识别，按默认值起用：{err}");
                (LlamaSettings::default(), true)
            }
        },
        Err(err) => {
            eprintln!("读取设置失败，按默认值起用：{err}");
            (LlamaSettings::default(), false)
        }
    }
}

/// 整体覆盖写入。失败返回可读原因（能力面故障时磁盘可能仍是旧值，调用方须给可见提示）。
pub async fn save(ctx: &atelyx_plugin_sdk::Context, settings: &LlamaSettings) -> Result<(), String> {
    let payload = PluginState { version: STATE_VERSION, settings: Some(settings.clone()) };
    let value = serde_json::to_value(&payload).map_err(|err| format!("设置序列化失败：{err}"))?;
    ctx.state_write(value)
        .await
        .map_err(|err| format!("设置落盘失败：{err}（本次变更可能没有保存）"))
}

/// 拼 HTTP 基址（末尾不带斜杠）。监听 0.0.0.0（局域网可调 API）时探测走本机回环；
/// 地址未填（输入框清空中）同样按本机回环处理。
pub fn base_url(settings: &LlamaSettings) -> String {
    let host = settings.host.trim();
    let probe_host = if host.is_empty() || host == "0.0.0.0" { "127.0.0.1" } else { host };
    format!("http://{probe_host}:{}", settings.port)
}

/// 模型文件夹路径归一（反斜杠→/、去尾分隔符），作 modelParams 的 key。
pub fn normalize_dir(dir: &str) -> String {
    let trimmed = dir.trim().replace('\\', "/");
    trimmed.trim_end_matches('/').to_string()
}

/// 取模型文件夹的持久化启动参数；没有记录给空白默认。
pub fn params_for(settings: &LlamaSettings, model_dir: &str) -> ModelLaunchParams {
    settings
        .model_params
        .get(&normalize_dir(model_dir))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_falls_back_to_defaults_on_bad_values() {
        let mut settings = LlamaSettings {
            host: "  ".into(),
            download_mirror: "no-such-mirror".into(),
            ..Default::default()
        };
        settings.model_params.insert(
            "C:/models/a".into(),
            ModelLaunchParams { cache_type_k: "bogus".into(), load_mode: "bogus".into(), ..Default::default() },
        );
        let out = settings.sanitize();
        assert_eq!(out.host, "127.0.0.1");
        assert_eq!(out.download_mirror, crate::mirrors::DEFAULT_MIRROR_ID);
        let params = &out.model_params["C:/models/a"];
        assert!(params.cache_type_k.is_empty() && params.load_mode.is_empty());
    }

    #[test]
    fn normalize_dir_unifies_separators_and_trailing_slashes() {
        assert_eq!(normalize_dir("C:\\models\\qwen\\"), "C:/models/qwen");
        assert_eq!(normalize_dir(" /m/n/ "), "/m/n");
    }

    #[test]
    fn base_url_probes_loopback_for_wildcard_host() {
        let mut settings = LlamaSettings { host: "0.0.0.0".into(), port: 8081, ..Default::default() };
        assert_eq!(base_url(&settings), "http://127.0.0.1:8081");
        settings.host = "192.168.1.5".into();
        assert_eq!(base_url(&settings), "http://192.168.1.5:8081");
    }

    #[test]
    fn roundtrip_keeps_camel_case_shape() {
        let settings = LlamaSettings::default();
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["downloadMirror"], crate::mirrors::DEFAULT_MIRROR_ID);
        assert!(json["autoStartOnApiCall"] == false);
        let back: LlamaSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, settings);
    }
}
