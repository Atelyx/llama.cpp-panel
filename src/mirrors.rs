//! 镜像站表与地址解析。
//! 镜像是整站内容镜像（仓库与文件名一致），换镜像 = 换域名：取用户输入的路径与所选域名重新拼装。

/// 镜像站（HF 通用镜像，纯域名替换）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorSource {
    pub id: &'static str,
    pub label: &'static str,
    /// 站点根地址（无尾斜杠）。
    pub origin: &'static str,
    pub note: &'static str,
}

pub const MIRRORS: [MirrorSource; 2] = [
    MirrorSource {
        id: "hf-mirror",
        label: "hf-mirror.com（国内镜像）",
        origin: "https://hf-mirror.com",
        note: "HuggingFace 整站镜像，国内可直连。",
    },
    MirrorSource {
        id: "hf",
        label: "huggingface.co（官方）",
        origin: "https://huggingface.co",
        note: "官方站点；国内直连通常失败。",
    },
];

pub const DEFAULT_MIRROR_ID: &str = MIRRORS[0].id;

pub fn mirror_by_id(id: &str) -> MirrorSource {
    MIRRORS.iter().copied().find(|m| m.id == id).unwrap_or(MIRRORS[0])
}

/// 仓库与文件名。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoTarget {
    /// 仓库全名，形如 `owner/name`。
    pub repo: String,
    /// 文件名（不含子目录）。
    pub file: String,
}

/// URL 路径段编码：只留 RFC 3986 非保留字符，`!'()*` 属子分隔符一并转义，其余按字节转义。
fn any_to_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b'!' | b'\'' | b'(' | b')' | b'*' => out.push_str(&format!("%{:02X}", byte)),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn query_value(value: &str) -> String {
    any_to_segment(value)
}

/// 片段解码：粘贴的地址可能带 %20 一类转义；解不开就按原样用。
fn decode_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).and_then(|h| {
                std::str::from_utf8(h).ok().and_then(|s| u8::from_str_radix(s, 16).ok())
            });
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_string())
}

/// 从用户输入解析仓库与文件名：接受整条下载地址（`/resolve|blob/<分支>/<路径>`）或
/// `owner/repo/文件名` 简写，不猜其它形态。`resolve`/`blob` 后那一段是分支名，须跳过；
/// 仓库文件可带子目录（落盘时由下载器压平成单段名），解析结果原样保留子路径。
pub fn parse_target(input: &str) -> Result<RepoTarget, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("请填写模型下载地址或 owner/repo/文件名".into());
    }

    // 非完整地址按 `owner/repo/文件名` 简写解析
    let path = match extract_url_path(text) {
        Some(path) => path,
        None => text.to_string(),
    };

    let segments: Vec<String> =
        path.split('/').map(decode_segment).filter(|s| !s.is_empty()).collect();
    // 路径段不得逃出仓库命名：`..` 与分隔符拒绝（落盘路径由仓库段拼装）
    if segments.iter().any(|s| s == ".." || s == ".") {
        return Err("地址不能包含相对路径段（. / ..）".into());
    }
    if segments.iter().any(|s| s.contains('\\')) {
        return Err("地址不能包含反斜杠".into());
    }

    // `resolve|blob` 标记定位仓库（前两段）与文件（分支名之后全部段）；无标记按简写取前两段
    let marker = segments.iter().position(|s| s == "resolve" || s == "blob");
    let (repo, file) = match marker {
        Some(m) if m >= 2 && m + 2 < segments.len() => {
            (segments[m - 2..m].join("/"), segments[m + 2..].join("/"))
        }
        _ => {
            if segments.len() < 3 {
                return Err("地址缺少仓库或文件名：需要 owner/repo/文件名".into());
            }
            (segments[0..2].join("/"), segments[2..].join("/"))
        }
    };

    if !repo.contains('/') {
        return Err("地址缺少仓库或文件名：需要 owner/repo/文件名".into());
    }
    if file.is_empty() || !file.to_lowercase().ends_with(".gguf") {
        return Err(format!("仅支持下载 .gguf 模型文件（收到「{file}」）"));
    }
    Ok(RepoTarget { repo, file })
}

/// 完整地址取 URL 路径部分；不是合法完整地址返回 None（按简写解析）。
fn extract_url_path(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+') {
        return None;
    }
    let (_, path) = rest.split_once('/')?;
    Some(format!("/{path}"))
}

/// 仓库文件下载地址（镜像域名 + 仓库路径；每段单独编码）。
pub fn download_url(mirror: MirrorSource, repo: &str, file: &str, revision: &str) -> String {
    let origin = mirror.origin;
    let repo_path = repo.split('/').map(any_to_segment).collect::<Vec<_>>().join("/");
    format!("{origin}/{repo_path}/resolve/{}/{}", any_to_segment(revision), any_to_segment(file))
}

/// 仓库 API 地址（tree / 详情）。
pub fn repo_api_url(mirror: MirrorSource, repo: &str, suffix: &str) -> String {
    let origin = mirror.origin;
    let repo_path = repo.split('/').map(any_to_segment).collect::<Vec<_>>().join("/");
    format!("{origin}/api/models/{repo_path}{suffix}")
}

/// 模型检索地址。
pub fn search_api_url(mirror: MirrorSource, query: &str, sort: &str, limit: u32) -> String {
    let mut parts = vec![
        "full=false".to_string(),
        format!("limit={limit}"),
        format!("sort={}", query_value(sort)),
        "direction=-1".to_string(),
    ];
    let query = query.trim();
    if !query.is_empty() {
        parts.insert(1, format!("search={}", query_value(query)));
    }
    let origin = mirror.origin;
    format!("{origin}/api/models?{}", parts.join("&"))
}

/// 模型卡（README）原文地址。
pub fn readme_url(mirror: MirrorSource, repo: &str, revision: &str) -> String {
    let origin = mirror.origin;
    let repo_path = repo.split('/').map(any_to_segment).collect::<Vec<_>>().join("/");
    format!("{origin}/{repo_path}/raw/{}/README.md", any_to_segment(revision))
}

/// 目标目录里找一个不冲突的文件名：已存在时插 `(1)`、`(2)`…（大小写不敏感）。
pub fn unique_name(existing: &std::collections::HashSet<String>, name: &str) -> String {
    let lower = |s: &str| s.to_lowercase();
    if !existing.contains(&lower(name)) {
        return name.to_string();
    }
    // 扩展名后缀按字节切（".gguf" 各大小写形态同字节数），避免大小写映射后的字节偏移错位
    let (stem, ext) = if name.len() >= 5 && lower(name).ends_with(".gguf") {
        (&name[..name.len() - 5], &name[name.len() - 5..])
    } else {
        (name, "")
    };
    for i in 1..1000 {
        let candidate = format!("{stem} ({i}){ext}");
        if !existing.contains(&lower(&candidate)) {
            return candidate;
        }
    }
    format!("{stem}-{}{ext}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_accepts_full_url_and_shorthand() {
        let url = "https://hf-mirror.com/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf";
        let target = parse_target(url).unwrap();
        assert_eq!(target.repo, "Qwen/Qwen2.5-7B-Instruct-GGUF");
        assert_eq!(target.file, "qwen2.5-7b-instruct-q4_k_m.gguf");

        let short = parse_target("Qwen/Qwen2.5-7B-Instruct-GGUF/Qwen2.5-7B-Instruct-Q4_K_M.gguf").unwrap();
        assert_eq!(short.repo, "Qwen/Qwen2.5-7B-Instruct-GGUF");
        assert_eq!(short.file, "Qwen2.5-7B-Instruct-Q4_K_M.gguf");

        // 仓库文件带子目录：文件路径原样保留（下载地址按子路径拼，落盘时压平）
        let sub = parse_target("Qwen/Qwen2.5-7B/mmparts/model.gguf").unwrap();
        assert_eq!(sub.repo, "Qwen/Qwen2.5-7B");
        assert_eq!(sub.file, "mmparts/model.gguf");
        let sub_url = parse_target("https://hf-mirror.com/Qwen/Qwen2.5-7B/resolve/main/mmparts/model.gguf").unwrap();
        assert_eq!(sub_url.repo, "Qwen/Qwen2.5-7B");
        assert_eq!(sub_url.file, "mmparts/model.gguf");

        // 残地址（resolve/main 后没有文件）：文件名段非 gguf，报错
        let stub = parse_target("https://hf-mirror.com/Qwen/Qwen2.5/resolve/main");
        assert!(stub.is_err());
        assert!(stub.unwrap_err().contains("仅支持下载 .gguf"));
    }

    #[test]
    fn parse_target_rejects_bad_input() {
        assert!(parse_target("").is_err());
        assert!(parse_target("Qwen/Qwen2.5").is_err(), "缺文件名");
        assert!(parse_target("owner/repo/model.bin").is_err(), "非 gguf");
        // 残地址（resolve/main 后没有文件）：文件名段非 gguf，报错
        let stub = parse_target("https://hf-mirror.com/Qwen/Qwen2.5/resolve/main");
        assert!(stub.is_err());
        assert!(stub.unwrap_err().contains("仅支持下载 .gguf"));
        // 相对路径段拒绝（落盘路径由仓库段拼装，不得逃出模型目录）
        assert!(parse_target("a/b/../../evil/model.gguf").is_err());
    }

    #[test]
    fn parse_target_decodes_escaped_segments() {
        let target = parse_target("https://hf-mirror.com/foo%20bar/repo/resolve/main/a%20b.gguf").unwrap();
        assert_eq!(target.repo, "foo bar/repo");
        assert_eq!(target.file, "a b.gguf");
    }

    #[test]
    fn download_url_encodes_each_segment() {
        let url = download_url(mirror_by_id("hf"), "Foo Bar/Repo", "a b.gguf", "main");
        assert_eq!(url, "https://huggingface.co/Foo%20Bar/Repo/resolve/main/a%20b.gguf");
    }

    #[test]
    fn search_url_inserts_query_when_present() {
        let with_query = search_api_url(MIRRORS[0], "qwen 3", "downloads", 20);
        assert_eq!(
            with_query,
            "https://hf-mirror.com/api/models?full=false&search=qwen%203&limit=20&sort=downloads&direction=-1"
        );
        let without = search_api_url(MIRRORS[0], "  ", "likes", 20);
        assert!(without.contains("&limit=20&sort=likes"));
        assert!(!without.contains("search="));
    }

    #[test]
    fn unique_name_avoids_conflicts_case_insensitively() {
        let existing: std::collections::HashSet<String> =
            ["model.gguf", "model (1).gguf"].iter().map(|s| s.to_string()).collect();
        assert_eq!(unique_name(&existing, "other.gguf"), "other.gguf");
        // 序号插入沿用输入名的原大小写扩展
        assert_eq!(unique_name(&existing, "MODEL.GGUF"), "MODEL (2).GGUF");
    }
}
