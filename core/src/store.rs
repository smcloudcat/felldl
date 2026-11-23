//! 持久化：任务逐个存为 JSON，设置单独一份。写入采用 tmp + rename 保证原子性。

use crate::model::{Settings, Task, TaskState};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks"))?;
        Ok(Self { root })
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
        write_atomic(&self.task_path(&task.id), &data)
    }

    pub fn delete_task(&self, id: &str) {
        let _ = fs::remove_file(self.task_path(id));
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
        write_atomic(&self.settings_path(), &data)
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

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, data)?;
    // Windows 上 rename 到已存在文件会失败，先删目标
    #[cfg(windows)]
    {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    fs::rename(&tmp, path)?;
    Ok(())
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
        };
        store.save_task(&t).unwrap();
        let loaded = store.load_tasks();
        assert_eq!(loaded.len(), 1);
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
