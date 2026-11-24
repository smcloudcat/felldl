//! 持久化：任务逐个存为 JSON，设置单独一份。写入采用 tmp + rename 保证原子性。
//!
//! 高频进度保存（后台 tick / BT / FTP/SFTP worker）走**合并队列**：
//! 同一任务在队列里只保留最新快照，由专用后台线程批量落盘，
//! 既避免阻塞 Tokio 工作线程，也消除同一任务被多处重复写盘。

use crate::model::{Settings, Task, TaskState};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

pub struct Store {
    pub root: PathBuf,
    /// 串行化所有写入：多个 worker（FTP/SFTP/BT/后台 tick）并发保存同一任务时，
    /// 避免交错写 tmp/重命名导致记录丢失。
    write_lock: Mutex<()>,
    /// 唯一 tmp 文件后缀，避免并发写同一 tmp 互相覆盖
    tmp_seq: AtomicU64,
    /// 合并写队列：key=task id，value=(revision, Option<Task>)。
    /// Some(task) 表示保存、None 表示删除墓碑；revision 用于防止
    /// 「删除后旧的 in-flight 写入把任务复活」。
    queue: Arc<Queue>,
}

type Queue = (Mutex<QueueState>, Condvar);

#[derive(Default)]
struct QueueState {
    map: HashMap<String, (u64, Option<Task>)>,
    /// writer 线程是否正在处理一批数据
    working: bool,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks"))?;
        let queue: Arc<Queue> = Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        // 专用 writer 线程：唯一真正落盘者，批量、去重、串行
        let wq = queue.clone();
        let wroot = root.clone();
        std::thread::Builder::new()
            .name("felldl-store-writer".into())
            .spawn(move || writer_loop(wroot, wq))
            .ok();
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
            tmp_seq: AtomicU64::new(0),
            queue,
        })
    }

    fn next_rev(&self) -> u64 {
        self.tmp_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// 热路径用：把任务放进合并队列，立即返回（不阻塞调用者）。
    /// 同一 id 的旧快照会被新快照覆盖，从而合并多次进度更新。
    pub fn enqueue_save(&self, task: &Task) {
        self.enqueue_save_owned(task.clone());
    }

    /// 同 `enqueue_save`，但直接接管所有权，省掉热路径上的一次深拷贝
    /// （Task 内含 segments 向量，任务多/分片多时拷贝并不便宜）。
    pub fn enqueue_save_owned(&self, task: Task) {
        let rev = self.next_rev();
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.map.insert(task.id.clone(), (rev, Some(task)));
        cv.notify_one();
    }

    /// 阻塞直到队列已排空（测试/退出时使用）
    pub fn flush(&self) {
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !g.map.is_empty() || g.working {
            g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
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

    /// 同步保存（状态变更/终态等需要立即持久化的场景）：入队并等待落盘完成。
    /// 走同一 writer 队列，天然与高频进度保存、删除墓碑按 revision 串行。
    pub fn save_task(&self, task: &Task) -> io::Result<()> {
        self.enqueue_save(task);
        self.flush();
        Ok(())
    }

    pub fn delete_task(&self, id: &str) {
        // 通过队列写入"墓碑"：writer 线程串行处理，天然与在途进度保存排序，
        // 不会出现「先删除、随后旧快照又写回来」的复活问题。
        let rev = self.next_rev();
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.map.insert(id.to_string(), (rev, None));
        cv.notify_all();
        drop(g);
        self.flush();
    }

    /// 同步删除（用于需要立即生效、且无并发写风险的场景，如损坏文件清理）
    #[allow(dead_code)]
    fn remove_task_file(&self, id: &str) {
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

/// 专用写盘线程：从合并队列取快照，批量、去重地落盘。
/// 同一 id 的多次进度更新在队列里只保留最新一份；删除用墓碑（None）表示，
/// 且墓碑总是覆盖旧快照，避免任务被复活。
fn writer_loop(root: PathBuf, q: Arc<Queue>) {
    let tasks_dir = root.join("tasks");
    let seq = AtomicU64::new(0);
    let (lock, cv) = &*q;
    loop {
        // 取当前一批待写项
        let batch: Vec<(String, Option<Task>)> = {
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            while g.map.is_empty() {
                g = cv
                    .wait_timeout(g, std::time::Duration::from_millis(500))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            g.working = true;
            let batch: Vec<(String, Option<Task>)> =
                std::mem::take(&mut g.map).into_iter().map(|(k, (_, v))| (k, v)).collect();
            batch
        };
        for (id, item) in batch {
            let path = tasks_dir.join(format!("{id}.json"));
            match item {
                Some(task) => {
                    if let Ok(data) = serde_json::to_vec_pretty(&task) {
                        let _ = write_atomic(&path, &data, &seq);
                    }
                }
                None => {
                    // 墓碑：删除任务文件 + 残留 tmp
                    let _ = fs::remove_file(&path);
                    if let Ok(rd) = fs::read_dir(&tasks_dir) {
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
            }
        }
        // 本批处理完，唤醒 flush() 等待者
        {
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            g.working = false;
            cv.notify_all();
        }
    }
}

fn write_atomic(path: &Path, data: &[u8], seq: &AtomicU64) -> io::Result<()> {
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
