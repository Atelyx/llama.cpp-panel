//! 模型目录扫描：递归收集 *.gguf（忽略大小写），条目名为相对 modelsDir 的路径。
//! 深度/目录数/条目数三重上限，触顶标 capped；子目录读不了跳过，根目录读不了整体失败。
//! 落盘路径统一用 `/` 连接（llama-server 接受正斜杠）。

use serde::Serialize;

/// 扫描条目（模型或视觉投影）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModelEntry {
    /// 相对 modelsDir 的路径（/ 分隔，含子目录）。
    pub name: String,
    /// 字节大小；取不到时缺省。
    pub size: Option<u64>,
}

/// 一次扫描的产物：普通模型与视觉投影分开返回（llama-server 的 --mmproj 单独传）。
#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub models: Vec<ModelEntry>,
    pub projectors: Vec<ModelEntry>,
    pub capped: bool,
}

/// 递归深度上限：模型库按类别分层足够，再深基本是扫错目录。
const MAX_DEPTH: usize = 6;
/// 目录访问数上限：挡符号链接环与异常深的树。
const MAX_DIRS: usize = 300;
/// 收集的 gguf 条目上限：界面列表与启动选择的合理规模。
const MAX_ENTRIES: usize = 600;

/// 扫描失败原因（根目录不存在/不可读等）。
pub type ScanError = String;

/// 递归扫描。根为空串 = 未选择目录，返回空产物而非错误。
pub fn list_models(models_dir: &str) -> Result<ScanResult, ScanError> {
    let root = models_dir.trim().replace('\\', "/");
    let root = root.trim_end_matches('/').to_string();
    if root.is_empty() {
        return Ok(ScanResult::default());
    }

    let mut out = ScanResult::default();
    let mut dirs = 0usize;
    walk(
        std::path::Path::new(&root),
        "",
        0,
        &mut out,
        &mut dirs,
    )?;
    let by_path = |a: &ModelEntry, b: &ModelEntry| a.name.cmp(&b.name);
    out.models.sort_by(by_path);
    out.projectors.sort_by(by_path);
    Ok(out)
}

fn walk(
    abs: &std::path::Path,
    rel: &str,
    depth: usize,
    out: &mut ScanResult,
    dirs: &mut usize,
) -> Result<(), ScanError> {
    if depth > MAX_DEPTH || *dirs >= MAX_DIRS || out.models.len() + out.projectors.len() >= MAX_ENTRIES {
        out.capped = true;
        return Ok(());
    }
    *dirs += 1;
    // 根目录失败 = 整体失败；子目录失败（权限/占用）跳过，不拖垮整次扫描
    let read = match std::fs::read_dir(abs) {
        Ok(read) => read,
        Err(err) => {
            if rel.is_empty() {
                return Err(format!("读取模型目录失败：{err}（目录不存在或不是目录）"));
            }
            return Ok(());
        }
    };
    for entry in read.flatten() {
        if out.models.len() + out.projectors.len() >= MAX_ENTRIES {
            out.capped = true;
            return Ok(());
        }
        let Ok(file_type) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if file_type.is_dir() {
            walk(&entry.path(), &child_rel, depth + 1, out, dirs)?;
            continue;
        }
        if !name.to_lowercase().ends_with(".gguf") {
            continue;
        }
        let size = entry.metadata().ok().map(|m| m.len());
        let item = ModelEntry { name: child_rel, size };
        // mmproj 视觉投影的识别口径：名字开头或分隔符（`-`/`_`/`.`/空白）之后的 mmproj 都算
        if is_projector_name(&name) {
            out.projectors.push(item);
        } else {
            out.models.push(item);
        }
    }
    Ok(())
}

/// 文件名是否为视觉投影：`mmproj` 出现在名字开头或分隔符（`-`/`_`/`.`/空白）之后。
fn is_projector_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let needle = "mmproj";
    let mut from = 0;
    while let Some(idx) = lower[from..].find(needle) {
        let abs = from + idx;
        let boundary = abs == 0
            || matches!(lower.as_bytes()[abs - 1], b'-' | b'_' | b'.' | b' ' | b'\t');
        if boundary {
            return true;
        }
        from = abs + needle.len();
    }
    false
}

/// 相对路径 → 所在子目录（无目录部分返回空串；反斜杠归一为 /）。
pub fn dir_name_of(path: &str) -> String {
    let norm = path.replace('\\', "/");
    match norm.rfind('/') {
        Some(idx) => norm[..idx].to_string(),
        None => String::new(),
    }
}

/// 一个模型文件夹：同目录的量化文件与视觉投影聚在一起。
#[derive(Clone, Debug)]
pub struct ModelFolder {
    /// 相对 modelsDir 的目录路径（空串 = 根目录）。
    pub rel: String,
    /// 全路径（/ 分隔）。
    pub full: String,
    pub quants: Vec<ModelEntry>,
    pub mms: Vec<ModelEntry>,
}

/// 扫描结果按文件夹聚合：文件夹 = 含至少一个非 mmproj 的 gguf；投影挂到同目录文件夹
/// （没有量化文件的目录不成为模型文件夹——孤零零的投影无处可挂）。
pub fn group_by_folder(scan: &ScanResult, models_dir: &str) -> Vec<ModelFolder> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, (Vec<ModelEntry>, Vec<ModelEntry>)> = BTreeMap::new();
    for m in &scan.models {
        map.entry(dir_name_of(&m.name)).or_default().0.push(m.clone());
    }
    for p in &scan.projectors {
        let dir = dir_name_of(&p.name);
        if let Some(bucket) = map.get_mut(&dir) {
            bucket.1.push(p.clone());
        }
    }
    map.into_iter()
        .map(|(rel, (quants, mms))| ModelFolder {
            full: join_model_path(models_dir, &rel),
            rel,
            quants,
            mms,
        })
        .collect()
}

/// modelsDir + 相对路径 → 模型全路径（用 / 连接）。
pub fn join_model_path(models_dir: &str, name: &str) -> String {
    let dir = models_dir.trim().trim_end_matches('/');
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 条目在文件夹内的相对名（条目名以文件夹路径为前缀；不符合前缀约定时原样返回）。
pub fn rel_name(folder_rel: &str, entry_name: &str) -> String {
    if folder_rel.is_empty() {
        return entry_name.to_string();
    }
    let prefix = format!("{folder_rel}/");
    match entry_name.strip_prefix(&prefix) {
        Some(rest) => rest.to_string(),
        None => entry_name.to_string(),
    }
}

/// 取路径的文件名部分（正/反斜杠都认）。
pub fn file_name_of(path: &str) -> String {
    let norm = path.replace('\\', "/");
    norm.rsplit('/').next().unwrap_or_default().to_string()
}

/// 字节 → 人类可读大小（十进制单位，与模型发布页标注口径一致；无值返回空串）。
pub fn format_size(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else { return String::new() };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 将 YYYY-MM-DD 开头的 ISO 时间串压成日期；解析不出返回原样。
pub fn short_date(iso: &str) -> String {
    if iso.len() >= 10 && iso.as_bytes()[4] == b'-' && iso.as_bytes()[7] == b'-' {
        iso[..10].to_string()
    } else {
        iso.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &std::path::Path, size: usize) {
        std::fs::write(path, vec![0u8; size]).unwrap();
    }

    #[test]
    fn scan_collects_models_and_projectors_with_sizes() {
        let dir = std::env::temp_dir().join(format!("llama-panel-scan-{}", std::process::id()));
        let sub = dir.join("Qwen/Qwen2.5");
        std::fs::create_dir_all(&sub).unwrap();
        write(&sub.join("model-Q4_K_M.gguf"), 100);
        write(&sub.join("mmproj-F16.gguf"), 50);
        write(&sub.join("Model-mmproj-Q8_0.gguf"), 60);
        write(&sub.join("readme.txt"), 5);
        // 名字开头的 mmproj 前缀即算投影（与识别口径一致，无分隔符要求）
        write(&sub.join("mmprojx-7b.gguf"), 10);

        let scan = list_models(dir.to_str().unwrap()).unwrap();
        assert_eq!(scan.models.len(), 1);
        assert_eq!(scan.projectors.len(), 3);
        assert!(!scan.capped);
        let quant = scan.models.iter().find(|m| m.name.ends_with("model-Q4_K_M.gguf")).unwrap();
        assert_eq!(quant.size, Some(100));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_reports_error_for_missing_root_and_empty_for_unset() {
        assert!(list_models("Z:/definitely/not/here").is_err());
        let empty = list_models("  ").unwrap();
        assert!(empty.models.is_empty() && empty.projectors.is_empty());
    }

    #[test]
    fn group_by_folder_attaches_projectors_only_to_quant_dirs() {
        let scan = ScanResult {
            models: vec![
                ModelEntry { name: "a/q1.gguf".into(), size: None },
                ModelEntry { name: "a/q2.gguf".into(), size: None },
            ],
            projectors: vec![
                ModelEntry { name: "a/mmproj.gguf".into(), size: None },
                ModelEntry { name: "lonely/mmproj.gguf".into(), size: None },
            ],
            capped: false,
        };
        let folders = group_by_folder(&scan, "M:");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].rel, "a");
        assert_eq!(folders[0].full, "M:/a");
        assert_eq!(folders[0].quants.len(), 2);
        assert_eq!(folders[0].mms.len(), 1);
    }

    #[test]
    fn projector_boundary_rules() {
        assert!(is_projector_name("mmproj-F16.gguf"));
        assert!(is_projector_name("Model-mmproj-Q8_0.gguf"));
        assert!(is_projector_name("model.mmproj.gguf"));
        // 字母紧跟其后不是分隔符时不算投影（防误伤）
        assert!(!is_projector_name("xmmproj-7b.gguf"));
    }

    #[test]
    fn format_size_uses_decimal_units() {
        assert_eq!(format_size(Some(999)), "999 B");
        assert_eq!(format_size(Some(1000)), "1.0 KB");
        assert_eq!(format_size(Some(4_700_000_000)), "4.7 GB");
        assert_eq!(format_size(None), "");
    }
}
