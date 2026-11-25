//! Android 路径 → FileProvider content:// URI 的映射（纯函数，可在桌面端跑测试）。
//! name 必须与 `res/xml/file_paths.xml` 保持一致。
//!
//! 桌面构建里只有测试会用到这些函数，故允许 dead_code。
#![allow(dead_code)]

/// 把绝对路径映射成 FileProvider 的 content:// URI。
/// 必须与 `res/xml/file_paths.xml` 里的 name 一一对应。
pub fn content_uri(pkg: &str, path: &str) -> Option<String> {
    let p = path.trim().replace('\\', "/");
    let p = if let Some(rest) = p.strip_prefix("/sdcard/") {
        format!("/storage/emulated/0/{rest}")
    } else {
        p
    };
    let roots: [(String, &str); 6] = [
        (format!("/storage/emulated/0/Android/data/{pkg}/files"), "ext_files"),
        (format!("/storage/emulated/0/Android/data/{pkg}/cache"), "ext_cache"),
        (format!("/data/user/0/{pkg}/files"), "files"),
        (format!("/data/data/{pkg}/files"), "files"),
        (format!("/data/user/0/{pkg}/cache"), "cache"),
        (format!("/data/data/{pkg}/cache"), "cache"),
    ];
    for (base, name) in roots.iter() {
        if let Some(rel) = p.strip_prefix(&format!("{base}/")) {
            return Some(format!(
                "content://{pkg}.fileprovider/{name}/{}",
                encode_path(rel)
            ));
        }
    }
    // 公共存储（Download / Movies / DCIM ...）
    if let Some(rel) = p.strip_prefix("/storage/emulated/0/") {
        return Some(format!("content://{pkg}.fileprovider/sd/{}", encode_path(rel)));
    }
    None
}

/// 文件夹在 DocumentsUI 里的 URI（primary 卷）
pub fn document_uri(pkg: &str, dir: &str) -> Option<(String, String)> {
    let _ = pkg;
    let p = dir.trim().replace('\\', "/");
    let rel = p.strip_prefix("/storage/emulated/0/")?;
    Some((
        format!(
            "content://com.android.externalstorage.documents/document/primary%3A{}",
            encode_path(rel).replace('/', "%2F")
        ),
        "vnd.android.document/directory".to_string(),
    ))
}

/// 只编码路径里必须编码的字符，保留 `/`
fn encode_path(s: &str) -> String {
    s.split('/')
        .map(|seg| {
            let mut out = String::with_capacity(seg.len());
            for b in seg.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        out.push(b as char)
                    }
                    _ => out.push_str(&format!("%{b:02X}")),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn mime_of(path: &str) -> &'static str {
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "flv" => "video/x-flv",
        "ts" => "video/mp2t",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "aac" => "audio/aac",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "apk" => "application/vnd.android.package-archive",
        "epub" => "application/epub+zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        // 未知类型交给系统让用户自己挑应用
        _ => "*/*",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_storage_maps_to_sd_root() {
        let uri = content_uri("com.felldl.app", "/storage/emulated/0/Download/a b.mp4").unwrap();
        assert_eq!(
            uri,
            "content://com.felldl.app.fileprovider/sd/Download/a%20b.mp4"
        );
    }

    #[test]
    fn app_private_dir_preferred_over_public() {
        let uri = content_uri(
            "com.felldl.app",
            "/storage/emulated/0/Android/data/com.felldl.app/files/Download/x.zip",
        )
        .unwrap();
        assert_eq!(
            uri,
            "content://com.felldl.app.fileprovider/ext_files/Download/x.zip"
        );
    }

    #[test]
    fn sdcard_alias_normalized() {
        let uri = content_uri("com.felldl.app", "/sdcard/Movies/v.mp4").unwrap();
        assert!(uri.ends_with("/sd/Movies/v.mp4"));
    }

    #[test]
    fn unknown_root_rejected() {
        assert!(content_uri("com.felldl.app", "/data/local/tmp/a.bin").is_none());
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(mime_of("/a/b/c.MP4"), "video/mp4");
        assert_eq!(mime_of("/a/b/unknown.xyz"), "*/*");
    }
}
