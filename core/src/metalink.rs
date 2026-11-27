//! Metalink（.metalink / .meta4）解析
//!
//! 提取 `<file name="...">` 与其中全部 `<url>` 标签；
//! 兼容 Metalink 4 与 Metalink 3（两者 URL 标签结构相同）。

/// 解析结果：(urls, file_name)
pub fn parse(text: &str) -> Option<(Vec<String>, Option<String>)> {
    // 文件名：metalink4 用 <file name="x">，metalink3 用 <file name="x">（相同）
    let name = find_first_attr(text, "<file", "name");
    let mut urls = Vec::new();
    let mut rest = text;
    while let Some(start) = find_tag(rest, "<url") {
        let after_open = match rest[start..].find('>') {
            Some(p) => start + p + 1,
            None => break,
        };
        let end = match rest[after_open..].find("</url>") {
            Some(p) => after_open + p,
            None => break,
        };
        let content = rest[after_open..end].trim();
        let u = content.trim().to_string();
        if !u.is_empty()
            && (u.starts_with("http://")
                || u.starts_with("https://")
                || u.starts_with("ftp://")
                || u.starts_with("ftps://"))
        {
            if !urls.contains(&u) {
                urls.push(u);
            }
        }
        rest = &rest[end + 6..];
    }
    if urls.is_empty() {
        return None;
    }
    Some((urls, name))
}

/// 在 s 中找 tag 前缀（要求后跟空白或 >，避免匹配到 <urls> 之类的其他标签）
fn find_tag(s: &str, prefix: &str) -> Option<usize> {
    let mut from = 0;
    loop {
        let idx = s[from..].find(prefix)?;
        let abs = from + idx;
        let rest = &s[abs + prefix.len()..];
        let ok = rest.starts_with('>')
            || rest
                .chars()
                .next()
                .map(|c| c.is_whitespace())
                .unwrap_or(false);
        if ok {
            return Some(abs);
        }
        from = abs + prefix.len();
    }
}

/// 找第一个形如 `<prefix ... attr="value">` 的属性值
fn find_first_attr<'a>(s: &'a str, prefix: &str, attr: &str) -> Option<String> {
    let start = find_tag(s, prefix)?;
    let tag_end = s[start..].find('>')? + start;
    let tag = &s[start..=tag_end.min(s.len() - 1)];
    // 在 tag 内找 attr="value" / attr='value'
    let mut i = 0;
    while let Some(p) = tag[i..].find(attr) {
        let abs = i + p + attr.len();
        let after = &tag[abs..];
        let after = after.trim_start();
        if let Some(rest) = after.strip_prefix("=\"") {
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
        if let Some(rest) = after.strip_prefix("='") {
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
        i = abs;
        if i >= tag.len() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metalink4_basic() {
        let xml = r##"<?xml version="1.0" encoding="utf-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <files>
    <file name="ubuntu-24.04.iso">
      <size>3123412345</size>
      <url location="cn" preference="90">https://mirror.example.com/ubuntu-24.04.iso</url>
      <url location="us" preference="80">http://dl.example.org/pub/ubuntu-24.04.iso</url>
      <urls></urls>
    </file>
  </files>
</metalink>"##;
        let (urls, name) = parse(xml).unwrap();
        assert_eq!(name.as_deref(), Some("ubuntu-24.04.iso"));
        assert_eq!(urls.len(), 2);
        assert!(urls[0].starts_with("https://mirror.example.com"));
    }

    #[test]
    fn metalink_ftp_urls_kept() {
        let xml = r##"<metalink><files><file name="a.bin">
        <url>ftp://ftp.example.com/a.bin</url>
        <url>javascript:alert(1)</url>
        </file></files></metalink>"##;
        let (urls, name) = parse(xml).unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "ftp://ftp.example.com/a.bin");
        assert_eq!(name.as_deref(), Some("a.bin"));
    }

    #[test]
    fn metalink_empty_rejected() {
        assert!(parse("<html>not a metalink</html>").is_none());
    }
}
