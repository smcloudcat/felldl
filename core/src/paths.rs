//! 任务产物的归属登记（PathRegistry）
//!
//! 要解决的问题：`delete_file` 的删盘动作依赖任务 JSON 里的 `save_path`。
//! 如果这份数据被改过（磁盘上的任务文件被手工编辑、旧版本的脏数据、将来某个
//! 写错字段的迁移），删除任务就会去删一个跟这个任务毫无关系的路径——最坏情况
//! 是 `/etc/passwd` 这种绝对路径，或者借 `..` 跳出下载目录。
//!
//! 做法：任务第一次进入内存索引时登记「输出根 + 文件名」，删除时只认登记过的
//! 拼接结果。登记的是**当时的那份路径**，后来 JSON 再怎么改都不影响已登记条目，
//! 这也顺带挡住了「校验通过之后、删除执行之前把路径换掉」的窗口。
//!
//! 没有登记条目时（理论上不该发生：启动加载与新建都走 `index_task`）退化为
//! 「用任务自己的 save_dir/file_name/save_path 三者一致性校验」，不满足就拒绝删除，
//! 而不是退回旧行为直接删——宁可留一个删不掉的半成品，也不能删错文件。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::TaskId;

/// 一个任务允许删除的产物。
#[derive(Debug, Clone)]
pub struct Owned {
    /// 登记时的输出根（必须绝对路径、且是 `save_path` 的父目录）
    pub root: PathBuf,
    /// 允许删除的具体文件：最终文件与 `.part` 临时文件
    pub files: Vec<PathBuf>,
}

#[derive(Default)]
pub struct PathRegistry {
    inner: Mutex<HashMap<TaskId, Owned>>,
}

impl PathRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记任务的产物。返回是否登记成功（路径不合法时不登记，也不会保护它）。
    ///
    /// 合法性要求：
    /// * `save_dir` 是绝对路径；
    /// * `file_name` 是**单段**文件名（不含分隔符、不是 `.`/`..`）——
    ///   带 `../` 的文件名足以跳出 `save_dir`；
    /// * `save_path` 正好等于 `save_dir/file_name`（不允许两者指向不同地方）。
    pub fn register(&self, id: &str, save_dir: &str, file_name: &str, save_path: &str) -> bool {
        let Some(owned) = derive(save_dir, file_name, save_path) else {
            return false;
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(id.to_string(), owned);
        true
    }

    /// 删除前取「这个任务允许删的文件」。`None` 表示路径不可信，调用方应拒绝删盘。
    ///
    /// 有登记条目就只用登记值：任务 JSON 后续被改成什么都影响不到它。
    pub fn allowed(&self, id: &str, save_dir: &str, file_name: &str, save_path: &str) -> Option<Vec<PathBuf>> {
        if let Some(e) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
        {
            return Some(e.files);
        }
        // 没登记：用任务自身三字段交叉校验（不访问磁盘，纯词法比较）
        derive(save_dir, file_name, save_path).map(|o| o.files)
    }

    /// 任务被删除后清掉登记，避免长跑进程里越攒越多。
    pub fn release(&self, id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// 校验并拼出产物清单。纯函数，便于单测。
fn derive(save_dir: &str, file_name: &str, save_path: &str) -> Option<Owned> {
    let root = Path::new(save_dir);
    if save_dir.trim().is_empty() || !root.is_absolute() {
        return None;
    }
    if !is_single_segment(file_name) {
        return None;
    }
    let expect = root.join(file_name);
    if Path::new(save_path) != expect {
        return None;
    }
    let part = crate::util::part_path(&expect);
    Some(Owned {
        root: root.to_path_buf(),
        files: vec![expect, part],
    })
}

/// 单段文件名：不含分隔符，不是 `.`/`..`，也不为空。
fn is_single_segment(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        // Windows 上 `C:foo` 是相对盘符路径，不能当文件名
        && !name.contains(':')
        && Path::new(name).components().count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 平台无关的绝对根目录（Windows 上 `/data/dl` 不算绝对路径）
    fn root(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("felldl-paths-{tag}"))
            .to_string_lossy()
            .to_string()
    }

    fn joined(root: &str, name: &str) -> String {
        Path::new(root).join(name).to_string_lossy().to_string()
    }

    #[test]
    fn registers_normal_task_and_returns_file_and_part() {
        let r = PathRegistry::new();
        let d = root("ok");
        let p = joined(&d, "a.bin");
        assert!(r.register("t1", &d, "a.bin", &p));
        let files = r
            .allowed("t1", &d, "a.bin", &p)
            .expect("登记过的任务应可删");
        assert_eq!(files.len(), 2);
        assert!(files[0].to_string_lossy().ends_with("a.bin"));
        assert!(files[1].to_string_lossy().ends_with("a.bin.part"));
        r.release("t1");
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn rejects_path_traversal_and_mismatched_paths() {
        let r = PathRegistry::new();
        let d = root("bad");
        let outside = std::env::temp_dir().join("passwd").to_string_lossy().to_string();
        // 文件名越界
        assert!(!r.register("t", &d, "../passwd", &outside));
        // 目标不在 save_dir 里
        assert!(!r.register("t", &d, "a.bin", &outside));
        // 相对 save_dir
        assert!(!r.register("t", "data/dl", "a.bin", "data/dl/a.bin"));
        // Windows 盘符相对路径（`C:evil.bin` 不是单段文件名）
        assert!(!r.register("t", &d, "C:evil.bin", &joined(&d, "C:evil.bin")));
        // 没有登记时同样拒绝
        assert!(r.allowed("missing", &d, "a.bin", &outside).is_none());
    }

    #[test]
    fn registered_entry_wins_over_later_mutation() {
        let r = PathRegistry::new();
        let d = root("mut");
        let p = joined(&d, "a.bin");
        assert!(r.register("t", &d, "a.bin", &p));
        // 任务 JSON 事后被改成指向别处：删除时仍只认登记值
        let evil = std::env::temp_dir().join("evil.bin").to_string_lossy().to_string();
        let files = r
            .allowed("t", &d, "a.bin", &evil)
            .expect("已登记任务不受后续字段改动影响");
        assert!(files.iter().all(|f| f.starts_with(&d)));
    }

    #[test]
    fn unregistered_task_falls_back_to_self_consistency() {
        let r = PathRegistry::new();
        let d = root("fb");
        let p = joined(&d, "a.bin");
        // 三字段自洽 → 放行（启动加载等极端情况下 registry 可能还没登记）
        assert!(r.allowed("t", &d, "a.bin", &p).is_some());
        // 不自洽 → 拒绝
        assert!(r
            .allowed("t", &d, "a.bin", &joined(&d, "b.bin"))
            .is_none());
    }
}
