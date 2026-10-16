//! Turning markdown (release notes, mod descriptions) into plain text for
//! the launcher, and formatting counts.

/// Strips common markdown and HTML noise; keeps paragraphs and list items.
pub fn clean_markdown(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut blank = 0;
    for line in text.replace("\r\n", "\n").lines() {
        let t = line.trim_end();
        if t.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        let mut l = if in_code { t.to_string() } else { inline(t.trim_start()) };
        if !in_code {
            let trimmed = l.trim_start_matches('#').trim_start().to_string();
            if trimmed.len() != l.len() {
                l = trimmed;
            }
            if let Some(rest) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).or_else(|| l.strip_prefix("+ ")) {
                l = format!("• {rest}");
            }
            if l.trim_start().starts_with('>') {
                l = l.trim_start().trim_start_matches('>').trim_start().to_string();
            }
            if l.chars().all(|c| matches!(c, '-' | '=' | '*' | '_' | ' ')) && !l.is_empty() {
                l.clear();
            }
        }
        if l.trim().is_empty() {
            blank += 1;
            if blank == 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank = 0;
        out.push_str(&l);
        out.push('\n');
    }
    let out = out.trim().to_string();
    if max_chars > 0 && out.chars().count() > max_chars {
        let cut: String = out.chars().take(max_chars).collect();
        let cut = cut.rsplit_once(char::is_whitespace).map(|(a, _)| a).unwrap_or(&cut);
        return format!("{cut}...");
    }
    out
}

fn inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_tag = false;
    while i < b.len() {
        let c = b[i];
        if in_tag {
            if c == '>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }
        if c == '<' && b.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic() || *n == '/' || *n == '!') {
            in_tag = true;
            i += 1;
            continue;
        }
        // Images ![alt](url) are dropped; links [text](url) keep the text.
        if (c == '!' && b.get(i + 1) == Some(&'[')) || c == '[' {
            let start = if c == '!' { i + 1 } else { i };
            if let Some(close) = b[start..].iter().position(|&x| x == ']').map(|p| p + start) {
                if b.get(close + 1) == Some(&'(') {
                    if let Some(end) = b[close..].iter().position(|&x| x == ')').map(|p| p + close) {
                        if c != '!' {
                            out.extend(&b[start + 1..close]);
                        }
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if matches!(c, '*' | '`') || (c == '_' && (i == 0 || b.get(i + 1).is_none_or(|n| !n.is_alphanumeric()) || !b[i - 1].is_alphanumeric())) {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// One line for a list row.
pub fn preview_line(text: &str, max_chars: usize) -> String {
    let s = clean_markdown(text, 0).replace('\n', " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max_chars {
        let cut: String = s.chars().take(max_chars).collect();
        return format!("{}...", cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut));
    }
    s
}

/// `12,345`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let first = match s.len() % 3 {
        0 => 3.min(s.len()),
        r => r,
    };
    let mut out = s[..first].to_string();
    for chunk in s.as_bytes()[first..].chunks(3) {
        out.push(',');
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    out
}

/// `950`, `1.2k`, `3M`.
pub fn short_count(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let (v, u) = if n < 1_000_000 { (n as f64 / 1e3, "k") } else { (n as f64 / 1e6, "M") };
    let r = (v * 10.0).round() / 10.0;
    if r.fract() == 0.0 { format!("{}{u}", r as u64) } else { format!("{r:.1}{u}") }
}

/// "3 minutes ago" style age for a timestamp.
pub fn ago(checked_at: u64) -> String {
    let s = crate::cache::now().saturating_sub(checked_at);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => plural(s / 60, "minute"),
        3600..=86399 => plural(s / 3600, "hour"),
        _ => plural(s / 86400, "day"),
    }
}

fn plural(n: u64, unit: &str) -> String {
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_to_text() {
        let md = "# Title\n\nSome **bold** and `code` with a [link](http://x) and ![img](http://i).\n\n\n- one\n- two\n\n```\nraw *text*\n```\n<!-- hidden --><b>tag</b> snake_case_name";
        let t = clean_markdown(md, 0);
        assert_eq!(t, "Title\n\nSome bold and code with a link and .\n\n• one\n• two\n\nraw *text*\ntag snake_case_name");
        assert_eq!(preview_line("## Hello\nworld  again", 9), "Hello...");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!((thousands(0).as_str(), thousands(999).as_str(), thousands(1000).as_str()), ("0", "999", "1,000"));
        assert_eq!(short_count(1250), "1.3k");
        assert_eq!(short_count(2_000_000), "2M");
    }
}
