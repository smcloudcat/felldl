//! 持久化：任务逐个存为 JSON，设置单独一份。写入采用 tmp + rename 保证原子性。

use crate::model::{Settings, Task, TaskState};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Store {
    pub root: PathBuf,
    /// 串行化所有写入：多个 worker（FTP/SFTP/BT/后台 tick）并发保存同一任务时，
    /// 避免交错写 tmp/重命名导致记录丢失。
    write_lock: Mutex<()>,
    /// 唯一 tmp 文件后缀，避免并发写同一 tmp 互相覆盖
    tmp_seq: AtomicU64,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks"))?;
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
            tmp_seq: AtomicU64::new(0),
        })
    }

    fn tasks_dir(&self) -> PathBuf {
        self.root.join("tasks")
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    fn task_path(&self, id: &str) -> PathBuf {
        self.tasks_dir().join(format!("{id}.json"))
    }

    pub fn save_task(&self, task: &Task) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(task)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        write_atomic(&self.task_path(&task.id), &data, &self.tmp_seq)
    }

    pub fn delete_task(&self, id: &str) {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ = fs::remove_file(self.task_path(id));
        // 顺带清掉可能残留的 tmp，避免垃圾文件
        let seq_dir = self.tasks_dir();
        if let Ok(rd) = fs::read_dir(&seq_dir) {
            let prefix = format!("{id}.json");
            for entry in rd.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(".tmp") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    pub fn load_tasks(&self) -> Vec<Task> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(self.tasks_dir()) else {
            return out;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(data) = fs::read(&p) else { continue };
            match serde_json::from_slice::<Task>(&data) {
                Ok(mut t) => {
                    // 进程重启后，进行中的任务一律回落为暂停，等待用户/自动恢复
                    if matches!(t.state, TaskState::Downloading | TaskState::Probing) {
                        t.state = TaskState::Paused;
                    }
                    if t.state == TaskState::Queued {
                        t.state = TaskState::Paused;
                    }
                    t.speed = 0;
                    out.push(t);
                }
                Err(_) => {
                    // 损坏的任务文件直接丢弃，避免整个列表加载失败
                    let _ = fs::remove_file(&p);
                }
            }
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        out
    }

    pub fn save_settings(&self, s: &Settings) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(s)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        write_atomic(&self.settings_path(), &data, &self.tmp_seq)
    }

    pub fn load_settings(&self) -> Settings {
        match fs::read(self.settings_path()) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|_| {
                // 兼容旧版本/字段缺失
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }
}

fn write_atomic(path: &Path, data: &[u8], seq: &AtomicU64) -> io::Result<()> {
    // 唯一 tmp 名：并发写者不会互相覆盖
    let unique = seq.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()))
        .with_extension(format!("{unique}.tmp"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, data)?;
    // Windows 上用 MoveFileExW 原子替换已存在目标；Unix rename 本身就是原子替换
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(lp_existing: *const u16, lp_new: *const u16, dw_flags: u32) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        let to_wide =
            |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(std::iter::once(0)).collect() };
        let src = to_wide(&tmp);
        let dst = to_wide(path);
        if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), MOVEFILE_REPLACE_EXISTING) } == 0 {
            let err = io::Error::last_os_error();
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskState;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("felldl-store-{tag}-{}", crate::model::now_ms()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn roundtrip_task() {
        let dir = tmp_dir("rt");
        let store = Store::new(&dir).unwrap();
        let mut t = Task {
            id: "abc".into(),
            url: "https://e.com/f.bin".into(),
            kind: crate::model::TaskKind::Http,
            mirrors: vec!["https://mirror.com/f.bin".into()],
            file_name: "f.bin".into(),
            save_dir: "C:/dl".into(),
            save_path: "C:/dl/f.bin".into(),
            size: Some(1000),
            downloaded: 400,
            state: TaskState::Downloading,
            error: None,
            connections: 8,
            segments: vec![],
            supports_range: true,
            etag: None,
            last_modified: None,
            content_type: None,
            referer: None,
            cookie: None,
            user_agent: None,
            speed_limit: 0,
            scheduled_at: None,
            created_at: 1,
            completed_at: None,
            speed: 12345,
            peers: 0,
            upload_speed: 0,
            bt_info_hash: None,
            category: Some("压缩包".into()),
            username: Some("u".into()),
            password: Some("p".into()),
            interval_secs: 60,
        };
        store.save_task(&t).unwrap();
        let loaded = store.load_tasks();
        assert_eq!(loaded.len(), 1);
        // 新字段也要原样往返
        assert_eq!(loaded[0].category.as_deref(), Some("压缩包"));
        assert_eq!(loaded[0].interval_secs, 60);
        // 重启后进行中 → 暂停
        assert_eq!(loaded[0].state, TaskState::Paused);
        assert_eq!(loaded[0].downloaded, 400);
        assert_eq!(loaded[0].speed, 0);
        t.downloaded = 500;
        let _ = t;
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_roundtrip_and_default_on_missing() {
        let dir = tmp_dir("st");
        let store = Store::new(&dir).unwrap();
        assert_eq!(store.load_settings().default_connections, 16);
        let mut s = Settings::default();
        s.default_connections = 32;
        store.save_settings(&s).unwrap();
        assert_eq!(store.load_settings().default_connections, 32);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_task_is_skipped() {
        let dir = tmp_dir("corrupt");
        let store = Store::new(&dir).unwrap();
        fs::write(dir.join("tasks").join("bad.json"), b"{not json").unwrap();
        assert!(store.load_tasks().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
