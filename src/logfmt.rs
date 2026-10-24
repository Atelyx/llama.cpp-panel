//! 进程输出 → 日志行的文本处理：宿主按行投递（一条事件一行，行尾 `\n` 或 `\r`），
//! 这里定行语义、剥 ANSI 转义并按 stderr 行头定级别。`\r` 是回刷新屏：连续多拍
//! 覆盖同一逻辑行（终端行为），模型加载进度因此不把日志刷爆。

use atelyx_plugin_protocol::view::ColorToken;

use crate::app::LogTone;

/// 单行输出的字符上限：进程可能吐出不换行的一整段，截断后不把大块文本塞进界面树。
pub const OUTPUT_LINE_MAX: usize = 4096;

/// 一条输出事件的落地语义（宿主保证一条事件只有行尾一个终止符，EOF 末段可能没有）。
pub enum OutputEvent<'a> {
    /// 完结或独立的一行：`\n` 结尾、或 EOF 不带行尾的末段。
    Line(&'a str),
    /// 回刷的一拍：`\r` 结尾。首拍建行、后续拍覆盖同一逻辑行；内容为空是 CRLF 的后半段。
    Refresh(&'a str),
}

/// 按行尾符分类一条输出事件（内容已去掉行尾）。
pub fn classify(data: &str) -> OutputEvent<'_> {
    match data.as_bytes().last() {
        Some(b'\r') => OutputEvent::Refresh(data.trim_end_matches('\r')),
        _ => OutputEvent::Line(data.trim_end_matches('\n')),
    }
}

/// 按字符数截断（超长行不把大块文本塞进界面树）。
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// stderr 行着色：llama-server 把日志写 stderr，行头形如「时间戳 I|W|E 组件」；
/// 解析不出级别的 stderr 行按警告处理——它至少值得注意。
pub fn stderr_tone(line: &str) -> LogTone {
    let body = line.trim_start();
    let mut parts = body.split_whitespace();
    let _timestamp = parts.next();
    match parts.next() {
        Some("E") => LogTone::Error,
        Some("W") => LogTone::Warn,
        Some("I" | "D") => LogTone::Normal,
        _ => LogTone::Warn,
    }
}

/// SGR 基础前景色 → 主题 token（30-37 顺序；90-97 亮色归并同色，亮度差不进 token 表）。
const ANSI_COLORS: [ColorToken; 8] = [
    ColorToken::TextMuted, // 30 黑
    ColorToken::Danger,    // 31 红
    ColorToken::Success,   // 32 绿
    ColorToken::Warning,   // 33 黄
    ColorToken::Accent,    // 34 蓝
    ColorToken::Accent,    // 35 品红
    ColorToken::Accent,    // 36 青
    ColorToken::Text,      // 37 白
];

/// 剥掉 ANSI 转义序列并取行级主色：`ESC [ 参数 m`（SGR）记色，其余 CSI / OSC /
/// 两字符转义只剥不显。行级主色只在某一种前景色覆盖**过半**可见字符时生效
/// （整行一色的行照实着色；只给行头一个词上色的行不传染整行），否则返回 `None`
/// 交回级别与就绪行规则着色。
pub fn strip_ansi(text: &str) -> (String, Option<ColorToken>) {
    if !text.contains('\u{1b}') {
        return (text.to_string(), None);
    }
    let mut out = String::with_capacity(text.len());
    // 各基础色累计覆盖的可见字符数；当前色 run 在换色时结清
    let mut tally = [0usize; ANSI_COLORS.len()];
    let mut visible = 0usize;
    let mut run_color: Option<u8> = None;
    let mut run_len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            run_len += 1;
            visible += 1;
            continue;
        }
        match chars.next() {
            Some('[') => {
                let mut params = String::new();
                let mut final_byte = None;
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        final_byte = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if final_byte == Some('m') {
                    let mut next = run_color;
                    for code in sgr_codes(&params) {
                        next = match code {
                            0 | 39 => None,
                            30..=37 => Some((code - 30) as u8),
                            90..=97 => Some((code - 90) as u8),
                            _ => next, // 粗体 / 斜体 / 256 色等不进行级映射
                        };
                    }
                    if next != run_color {
                        if let Some(idx) = run_color {
                            tally[idx as usize] += run_len;
                        }
                        run_len = 0;
                        run_color = next;
                    }
                }
            }
            // OSC（标题等）：剥到 BEL 或字符串终止符
            Some(']') => {
                for c in chars.by_ref() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // 两字符转义（ESC + 单字节）：整体剥掉
            Some(_) => {}
            None => {}
        }
    }
    if let Some(idx) = run_color {
        tally[idx as usize] += run_len;
    }
    let color = tally
        .iter()
        .enumerate()
        .max_by_key(|(_, count)| **count)
        // 过半才作行级主色：行头一个词的着色不传染整行
        .filter(|(_, count)| **count * 2 > visible)
        .map(|(idx, _)| ANSI_COLORS[idx]);
    (out, color)
}

/// SGR 参数字符串 → 数字码序列（空段 = 重置；冒号子参数按分号同级处理）。
fn sgr_codes(params: &str) -> Vec<u16> {
    params
        .replace(':', ";")
        .split(';')
        .map(|part| part.trim().parse::<u16>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_reads_line_terminator() {
        assert!(matches!(classify("abc\n"), OutputEvent::Line("abc")));
        assert!(matches!(classify("abc\r"), OutputEvent::Refresh("abc")));
        assert!(matches!(classify("abc"), OutputEvent::Line("abc")), "EOF 末段按完结行");
        assert!(matches!(classify("\r"), OutputEvent::Refresh("")));
        assert!(matches!(classify("\n"), OutputEvent::Line("")));
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let (text, color) = strip_ansi("llama-server is listening");
        assert_eq!(text, "llama-server is listening");
        assert_eq!(color, None);
    }

    #[test]
    fn sgr_sequences_are_stripped_and_counted() {
        // 整行绿色：主色取绿
        let (text, color) = strip_ansi("\u{1b}[32mall green\u{1b}[0m");
        assert_eq!(text, "all green");
        assert_eq!(color, Some(ColorToken::Success));
        // 彩色行头 + 常规正文：正文更长，主色回落缺省
        let (text, color) = strip_ansi("\u{1b}[31merror\u{1b}[0m: something went wrong");
        assert_eq!(text, "error: something went wrong");
        assert_eq!(color, None);
        // 亮色归并基础色
        let (_, color) = strip_ansi("\u{1b}[91mred\u{1b}[0m");
        assert_eq!(color, Some(ColorToken::Danger));
    }

    #[test]
    fn non_sgr_escapes_are_stripped_without_color() {
        let (text, color) = strip_ansi("\u{1b}[2J\u{1b}[Hhello\u{1b}]0;title\u{7}");
        assert_eq!(text, "hello");
        assert_eq!(color, None);
    }

    #[test]
    fn bold_does_not_break_color_run() {
        let (text, color) = strip_ansi("\u{1b}[1;33mwarn\u{1b}[0m");
        assert_eq!(text, "warn");
        assert_eq!(color, Some(ColorToken::Warning));
    }

    #[test]
    fn stderr_tone_reads_level_letter() {
        assert_eq!(stderr_tone("10:00:00 E main: boom"), LogTone::Error);
        assert_eq!(stderr_tone("10:00:00 W main: hmm"), LogTone::Warn);
        assert_eq!(stderr_tone("10:00:00 I main: ok"), LogTone::Normal);
        assert_eq!(stderr_tone("no level here"), LogTone::Warn);
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("汉字汉字", 2), "汉字…");
    }
}
