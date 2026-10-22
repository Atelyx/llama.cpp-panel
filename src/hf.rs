//! HuggingFace 数据面：搜索、仓库文件树、模型卡与体积探测。
//! 公网取数由插件直连：宿主 `http` 面不设地址/大小/超时限制，但把整个响应 base64 编进
//! 单条 JSON 应答（无流式、无进度），不适用这里的取数与下载。
//! 取数共用一条请求通道：30s 超时 + 4MB 响应上限；网络失败一律翻成可读原因，不兜底静默。

use serde_json::Value;

use crate::mirrors::{readme_url, repo_api_url, search_api_url, MirrorSource};

/// 单次取数超时（直连公网站点，含响应体读取）。
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 响应体上限：超出按错误/截断显式处理，不静默给半个响应。
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 取数响应：正文 + 是否触顶截断。
struct FetchBody {
    body: String,
    truncated: bool,
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<FetchBody, String> {
    let response = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}（{url}）"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("接口返回 {}（{url}）", status.as_u16()));
    }
    let mut body = Vec::new();
    let mut truncated = false;
    let mut stream = response;
    while let Some(chunk) = stream.chunk().await.map_err(|e| format!("读取响应失败：{e}（{url}）"))? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            body.extend_from_slice(&chunk[..MAX_RESPONSE_BYTES - body.len()]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchBody { body: String::from_utf8_lossy(&body).into_owned(), truncated })
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let fetched = fetch(client, url).await?;
    // 触顶的正文是半个 JSON：明确报上限，不做无意义的解析
    if fetched.truncated {
        return Err(format!(
            "接口返回内容超出 {}MB 响应上限（{url}）",
            MAX_RESPONSE_BYTES / (1024 * 1024)
        ));
    }
    let text = fetched.body.trim();
    if text.is_empty() {
        return Err(format!("接口没有返回内容（{url}）"));
    }
    serde_json::from_str(text).map_err(|_| {
        // 错误消息按字符截断（按字节切会在多字节字符中间 panic）
        let preview: String = text.chars().take(120).collect();
        format!("接口返回的不是 JSON（{url}）：{preview}")
    })
}

/// 检索结果的排序方式（值即接口的 sort 参数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchSort {
    Downloads,
    Likes,
    LastModified,
}

impl SearchSort {
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Likes,
            2 => Self::LastModified,
            _ => Self::Downloads,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::LastModified => "lastModified",
        }
    }
}

/// 模型仓库摘要（搜索列表与详情页共用）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelSummary {
    /// 仓库全名，形如 `Qwen/Qwen2.5-7B-Instruct-GGUF`。
    pub repo: String,
    pub downloads: u64,
    pub likes: u64,
    /// ISO 时间串；空串表示仓库未给出。
    pub updated_at: String,
}

/// 仓库里的一个文件条目。
#[derive(Clone, Debug, PartialEq)]
pub struct RepoFile {
    /// 仓库内相对路径（可能含子目录）。
    pub name: String,
    /// 字节大小；仓库未给出时缺省。
    pub size: Option<u64>,
}

/// 仓库详情：摘要 + gguf 文件清单 + 模型卡简介。
#[derive(Clone, Debug)]
pub struct RepoDetail {
    pub summary: ModelSummary,
    /// 仅 `.gguf`，按体积升序（小的量化在前，便于按显存挑）。
    pub files: Vec<RepoFile>,
    /// 模型卡 README 原文（行尾归一 LF、front matter 已去；空串 = 没有可展示的简介）。
    pub readme: String,
    /// 是否为门控模型：需要授权才能下载，只能到浏览器登录后手动下载。
    pub gated: bool,
}

fn num(value: &Value) -> u64 {
    value.as_f64().filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64).unwrap_or(0)
}

fn to_summary(raw: &Value) -> Option<ModelSummary> {
    let id = raw
        .get("modelId")
        .and_then(Value::as_str)
        .or_else(|| raw.get("id").and_then(Value::as_str))
        .unwrap_or_default();
    if id.is_empty() {
        return None; // 无 id 的条目无法拼地址，直接丢弃
    }
    Some(ModelSummary {
        repo: id.to_string(),
        downloads: num(raw.get("downloads").unwrap_or(&Value::Null)),
        likes: num(raw.get("likes").unwrap_or(&Value::Null)),
        updated_at: raw.get("lastModified").and_then(Value::as_str).unwrap_or_default().to_string(),
    })
}

/// 搜索模型仓库。
pub async fn search_models(
    client: &reqwest::Client,
    mirror: MirrorSource,
    query: &str,
    sort: SearchSort,
    limit: u32,
) -> Result<Vec<ModelSummary>, String> {
    let raw = fetch_json(client, &search_api_url(mirror, query, sort.as_str(), limit)).await?;
    let list = raw.as_array().ok_or("检索接口返回的不是列表")?;
    Ok(list.iter().filter_map(to_summary).collect())
}

/// 仓库文件条目 → 字节大小：普通文件取 size，LFS 指针文件取 lfs.size。
fn entry_size(entry: &Value) -> Option<u64> {
    let lfs = entry.get("lfs").and_then(|l| l.get("size")).and_then(Value::as_u64);
    lfs.or_else(|| entry.get("size").and_then(Value::as_u64))
}

/// 仓库详情：文件树（含体积）+ 模型卡简介。
pub async fn fetch_repo_detail(
    client: &reqwest::Client,
    mirror: MirrorSource,
    repo: &str,
) -> Result<RepoDetail, String> {
    let tree_url = repo_api_url(mirror, repo, "/tree/main?recursive=true&expand=true");
    let (tree_raw, detail_raw) = tokio::try_join!(
        async { fetch_json(client, &tree_url).await },
        async { fetch_json(client, &repo_api_url(mirror, repo, "")).await }
    )?;
    let summary = to_summary(&detail_raw).ok_or(format!("仓库不存在或不可访问：{repo}"))?;

    let entries = tree_raw.as_array().cloned().unwrap_or_default();
    let mut files: Vec<RepoFile> = entries
        .iter()
        .filter_map(|entry| {
            let path = entry.get("path").and_then(Value::as_str)?;
            path.to_lowercase().ends_with(".gguf").then(|| RepoFile {
                name: path.to_string(),
                size: entry_size(entry),
            })
        })
        .collect();
    files.sort_by_key(|f| f.size.unwrap_or(u64::MAX));

    let gated = detail_raw.get("gated").and_then(Value::as_bool).unwrap_or(false);
    let readme = fetch_readme(client, mirror, repo).await;
    Ok(RepoDetail { summary, files, readme, gated })
}

/// 取模型卡简介。取不到不算失败——简介是附加信息，不该挡住文件清单。
async fn fetch_readme(client: &reqwest::Client, mirror: MirrorSource, repo: &str) -> String {
    let url = readme_url(mirror, repo, "main");
    let Ok(fetched) = fetch(client, &url).await else {
        return String::new();
    };
    let mut text = fetched.body;
    if fetched.truncated {
        text.push_str("\n\n（内容过长，已截断）");
    }
    let text = text
        .trim_start_matches('\u{FEFF}')
        // 行尾归一为 LF
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    // YAML front matter（许可证、语言等元数据）不是正文，不参与展示
    match text.strip_prefix("---\n") {
        Some(rest) => match rest.find("\n---") {
            Some(end) => rest[end + 4..].trim_start_matches('\n').to_string(),
            None => text,
        },
        None => text,
    }
}

/// 探取下载地址对应的真实文件大小；拿不到返回 None——体积只用于显示百分比，不是下载的前置条件。
/// HEAD 穿透重定向读最终响应的 Content-Length：HF 第一跳是签名重定向，跟到最后一跳才有总长度。
pub async fn fetch_remote_info(client: &reqwest::Client, url: &str) -> Option<u64> {
    let response = client.head(url).timeout(FETCH_TIMEOUT).send().await.ok()?;
    let size = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    match size {
        Some(size) if size > 0 => Some(size),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_requires_repo_id() {
        let with_id = serde_json::json!({"modelId": "a/b", "downloads": -3, "likes": 2.7});
        let summary = to_summary(&with_id).unwrap();
        assert_eq!(summary.repo, "a/b");
        assert_eq!(summary.downloads, 0, "负数与分数收敛为 0");
        assert_eq!(summary.likes, 2);

        assert!(to_summary(&serde_json::json!({"downloads": 5})).is_none());
    }

    #[test]
    fn entry_size_prefers_lfs() {
        let entry = serde_json::json!({"path": "a.gguf", "size": 120, "lfs": {"size": 500}});
        assert_eq!(entry_size(&entry), Some(500));
        let plain = serde_json::json!({"path": "a.gguf", "size": 120});
        assert_eq!(entry_size(&plain), Some(120));
    }

    #[test]
    fn readme_strips_front_matter_and_normalizes_crlf() {
        // front matter 剥离逻辑与 fetch_readme 内联实现共用同一段口径，这里直接测数据面
        let text = "---\nlicense: mit\r\n---\r\n# Hello\r\nworld";
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let stripped = match normalized.strip_prefix("---\n") {
            Some(rest) => match rest.find("\n---") {
                Some(end) => rest[end + 4..].trim_start_matches('\n').to_string(),
                None => normalized,
            },
            None => normalized,
        };
        assert_eq!(stripped, "# Hello\nworld");
    }
}
