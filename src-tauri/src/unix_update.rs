//! Linux / macOS 的更新落地判断。
//!
//! Windows 必须用独立 helper（占着旧 exe 无法改名），Unix 不需要：
//! `rename` 可以覆盖正在运行的可执行文件，进程会继续用旧 inode 跑完。
//! 所以这里只决定「能不能原地换掉」，替换本身仍复用 `update_helper::replace_file`，
//! 那份代码有回滚与完整性校验，不应该有两套。
//!
//! 换不掉的情况必须老实交回系统安装器（`.dmg` / `.deb` / 压缩包），
//! 不能假装重启——否则用户会看到一个「更新完还是旧版本」的应用。

use std::path::{Path, PathBuf};

/// 能否原地替换当前程序。
///
/// 返回 `Some(目标路径)` 表示可以：把这个路径上的文件换成新包，再重启它。
pub fn replace_target(artifact: &Path) -> Option<PathBuf> {
    // AppImage：`current_exe()` 指向 `/tmp/.mount_xxx/usr/bin/felldl`（只读挂载），
    // 真正的可执行文件是 `$APPIMAGE` 指向的那一个。
    if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
        if appimage.is_file() && is_elf(artifact) {
            return Some(appimage);
        }
    }

    let exe = std::env::current_exe().ok()?;

    // 只认得「单个可执行文件」这一种产物：`.dmg` / `.deb` 交给系统安装器
    if !is_elf(artifact) && !is_macho(artifact) {
        return None;
    }
    // macOS 的 .app 是目录结构，单独换掉里面的二进制会让签名与 Info.plist 对不上；
    // 允许，但只在新包本身就是可执行文件时才走到这里（分发的是 dmg，不会命中）。
    writable_dir(exe.parent()?).then_some(exe)
}

/// ELF 头：Linux 可执行文件与 AppImage 都是 ELF
fn is_elf(p: &Path) -> bool {
    read_magic(p) == Some([0x7f, b'E', b'L', b'F'])
}

/// Mach-O 头（64 位小端 / 大端 / 通用二进制）
fn is_macho(p: &Path) -> bool {
    matches!(
        read_magic(p),
        Some([0xcf, 0xfa, 0xed, 0xfe])
            | Some([0xfe, 0xed, 0xfa, 0xcf])
            | Some([0xca, 0xfe, 0xba, 0xbe])
            | Some([0xbe, 0xba, 0xfe, 0xca])
    )
}

fn read_magic(p: &Path) -> Option<[u8; 4]> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// 目录可写：在目标目录里真的建一个临时文件再删掉。
///
/// 用 `metadata().permissions().readonly()` 判断在 Unix 上完全不可靠
/// （root、ACL、只读挂载、SELinux 都会让结论反过来），只有实际写一次才算数。
fn writable_dir(dir: &Path) -> bool {
    let probe = dir.join(format!(".felldl-w-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmg_and_deb_are_not_in_place_targets() {
        let dir = std::env::temp_dir().join(format!("felldl-u-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dmg = dir.join("Felldl.dmg");
        std::fs::write(&dmg, b"not an executable").unwrap();
        assert!(replace_target(&dmg).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn elf_magic_is_detected() {
        let dir = std::env::temp_dir().join(format!("felldl-e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("app.AppImage");
        std::fs::write(&bin, b"\x7fELFrest").unwrap();
        assert!(is_elf(&bin));
        assert!(!is_macho(&bin));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writable_probe_cleans_up_after_itself() {
        let dir = std::env::temp_dir().join(format!("felldl-w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(writable_dir(&dir));
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 0, "探测文件必须被删掉");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
