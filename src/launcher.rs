//! 启动命令组装与可执行文件定位。
//! 参数经宿主进程面直传操作系统（不经 shell 转述），附加参数里的元字符按字面到达 llama-server。
//! 用户选的是文件夹而非可执行文件——发布里文件名随平台变（Windows 带 .exe），按平台名去文件夹里找。

use crate::settings::{params_for, normalize_dir, LlamaSettings};

/// 平台对应的 llama-server 可执行文件名（插件按自身编译目标判定平台）。
pub fn executable_name() -> &'static str {
    #[cfg(windows)]
    {
        "llama-server.exe"
    }
    #[cfg(not(windows))]
    {
        "llama-server"
    }
}

/// 附加启动参数切分：空白分隔，双引号内保留空白并去掉引号。
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    for ch in text.chars() {
        if ch == '"' {
            quoted = !quoted;
            started = true;
            continue;
        }
        if !quoted && ch.is_whitespace() {
            if started {
                out.push(std::mem::take(&mut current));
            }
            started = false;
            continue;
        }
        current.push(ch);
        started = true;
    }
    if started {
        out.push(current);
    }
    out
}

/// 在所选文件夹里找 llama-server：先按平台标准名精确匹配（大小写不敏感），再退到
/// 同前缀的改名构建（如 llama-server-cuda.exe）；找不到返回 None，由调用方给提示。
pub fn find_server_executable(server_dir: &str) -> Result<Option<String>, String> {
    let folder = server_dir.trim().replace('\\', "/");
    let folder = folder.trim_end_matches('/').to_string();
    if folder.is_empty() {
        return Ok(None);
    }
    let read = std::fs::read_dir(&folder)
        .map_err(|err| format!("读取 llama-server 文件夹失败：{err}（目录不存在或不是目录）"))?;
    let files: Vec<String> = read
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let want = executable_name();
    if let Some(exact) = files.iter().find(|name| name.to_lowercase() == want.to_lowercase()) {
        return Ok(Some(format!("{folder}/{exact}")));
    }
    let mut renamed: Vec<String> = files
        .into_iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
            // Windows 只认 .exe：同目录还可能有 dll，前缀相同也不该被当成程序
            stem.starts_with("llama-server") && (!cfg!(windows) || lower.ends_with(".exe"))
        })
        .collect();
    renamed.sort();
    Ok(renamed.first().map(|name| format!("{folder}/{name}")))
}

/// 找不到可执行文件时的可操作提示。
pub fn missing_executable_message(server_dir: &str) -> String {
    let folder = server_dir.trim();
    if folder.is_empty() {
        return "未配置 llama-server 文件夹".into();
    }
    format!("未在 {folder} 找到 {}", executable_name())
}

/// llama-server 参数（不含程序名）。`--model` 是模型标志（llama.cpp server 惯用写法）。
pub fn build_start_tokens(settings: &LlamaSettings, server_exe: &str) -> Result<Vec<String>, String> {
    let model = settings.model_file.trim();
    if model.is_empty() {
        return Err("未选择模型文件".into());
    }
    // --host 必须显式传：llama-server 默认只绑 127.0.0.1，局域网设备调不了 API
    let host = if settings.host.trim().is_empty() { "127.0.0.1" } else { settings.host.trim() };
    let mut tokens = vec![
        server_exe.to_string(),
        "--host".into(),
        host.to_string(),
        "--model".into(),
        model.to_string(),
        "--port".into(),
        settings.port.to_string(),
    ];
    // 多模态模型的视觉投影：配置了就挂上（面板选中文件夹时会自动带出）
    let mmproj = settings.mmproj_file.trim();
    if !mmproj.is_empty() {
        tokens.push("--mmproj".into());
        tokens.push(mmproj.to_string());
    }
    // 启动参数按模型文件夹持久化：换模型即换参数。旗标拼写对照 llama-server 官方文档。
    let model_dir = model.replace('\\', "/");
    let dir = match model_dir.rfind('/') {
        Some(idx) => &model_dir[..idx],
        None => "",
    };
    let params = params_for(settings, dir);
    if let Some(ngl) = params.ngl {
        tokens.push("-ngl".into());
        tokens.push(ngl.to_string());
    }
    if let Some(ctx_size) = params.ctx_size {
        tokens.push("--ctx-size".into());
        tokens.push(ctx_size.to_string());
    }
    if let Some(threads) = params.threads {
        tokens.push("--threads".into());
        tokens.push(threads.to_string());
    }
    if let Some(parallel) = params.parallel {
        tokens.push("--parallel".into());
        tokens.push(parallel.to_string());
    }
    if !params.cache_type_k.is_empty() {
        tokens.push("--cache-type-k".into());
        tokens.push(params.cache_type_k.clone());
    }
    // flash-attn 是三态旗标，显式传 on 固定语义；mmap/mlock 的开关统一走 --load-mode
    if params.flash_attn {
        tokens.push("--flash-attn".into());
        tokens.push("on".into());
    }
    if params.jinja {
        tokens.push("--jinja".into());
    }
    if params.no_webui {
        tokens.push("--no-webui".into());
    }
    if !params.load_mode.is_empty() {
        tokens.push("--load-mode".into());
        tokens.push(params.load_mode.clone());
    }
    let api_key = params.api_key.trim();
    if !api_key.is_empty() {
        tokens.push("--api-key".into());
        tokens.push(api_key.to_string());
    }
    tokens.extend(split_args(&params.extra_args));
    Ok(tokens)
}

/// 含空白或引号时加引号（仅供展示的命令行预览用）。
pub fn shell_quote(value: &str) -> String {
    if value.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// 供界面展示的命令行（仅为可读，执行不走它）。
pub fn describe_start_command(settings: &LlamaSettings, server_exe: &str) -> String {
    build_start_tokens(settings, server_exe)
        .unwrap_or_default()
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 启动参数：程序 = 定位到的可执行文件，其余为参数；工作目录取模型所在目录，
/// 让 llama-server 的相对路径（如缓存）落到模型目录旁。返回 (程序, 参数, 工作目录)。
pub fn start_command(settings: &LlamaSettings, server_exe: &str) -> Result<(String, Vec<String>, Option<String>), String> {
    let tokens = build_start_tokens(settings, server_exe)?;
    let model_dir = normalize_dir(&settings.model_file);
    let cwd = model_dir.rfind('/').map(|idx| model_dir[..idx].to_string());
    let args = tokens.into_iter().skip(1).collect();
    Ok((server_exe.to_string(), args, cwd.filter(|c| !c.is_empty())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ModelLaunchParams;

    fn settings_with(model_file: &str, params: ModelLaunchParams) -> LlamaSettings {
        let mut settings = LlamaSettings { model_file: model_file.into(), ..Default::default() };
        if let Some(dir) = model_file.rfind('/') {
            settings.model_params.insert(normalize_dir(&model_file[..dir]), params);
        }
        settings
    }

    #[test]
    fn start_tokens_cover_flags_in_order() {
        let params = ModelLaunchParams {
            quant: "m.gguf".into(),
            mmproj: String::new(),
            ngl: Some(33),
            ctx_size: Some(4096),
            threads: None,
            parallel: Some(2),
            cache_type_k: "q8_0".into(),
            flash_attn: true,
            jinja: true,
            no_webui: true,
            load_mode: "mmap".into(),
            api_key: "sk-test".into(),
            extra_args: "  --verbose  --api-key \"abc def\" ".into(),
        };
        let settings = settings_with("C:/models/q/m.gguf", params);
        let tokens = build_start_tokens(&settings, "C:/srv/llama-server.exe").unwrap();
        assert_eq!(
            tokens,
            vec![
                "C:/srv/llama-server.exe",
                "--host", "127.0.0.1",
                "--model", "C:/models/q/m.gguf",
                "--port", "8080",
                "-ngl", "33",
                "--ctx-size", "4096",
                "--parallel", "2",
                "--cache-type-k", "q8_0",
                "--flash-attn", "on",
                "--jinja",
                "--no-webui",
                "--load-mode", "mmap",
                "--api-key", "sk-test",
                "--verbose",
                "--api-key", "abc def",
            ]
        );
    }

    #[test]
    fn start_requires_model_file() {
        let settings = LlamaSettings::default();
        assert!(build_start_tokens(&settings, "srv").is_err());
    }

    #[test]
    fn split_args_keeps_quoted_whitespace() {
        assert_eq!(split_args("a b  \"c d\""), vec!["a", "b", "c d"]);
        assert_eq!(split_args("\"\""), vec![""], "空引号也是一个参数");
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn shell_quote_only_when_needed() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("a b"), "\"a b\"");
        assert_eq!(shell_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn find_executable_prefers_exact_then_prefix() {
        let dir = std::env::temp_dir().join(format!("llama-panel-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_server_executable(dir.to_str().unwrap()).unwrap().is_none());
        std::fs::write(dir.join("llama-server-cuda.exe"), b"x").unwrap();
        let found = find_server_executable(dir.to_str().unwrap()).unwrap().unwrap();
        assert!(found.ends_with("llama-server-cuda.exe"));
        std::fs::write(dir.join("llama-server.exe"), b"x").unwrap();
        let found = find_server_executable(dir.to_str().unwrap()).unwrap().unwrap();
        assert!(found.ends_with("llama-server.exe"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
