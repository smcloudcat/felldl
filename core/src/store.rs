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
    /// 已删除任务的永久墓碑集合。
    ///
    /// 队列里的墓碑只能盖住**同一批**里的旧快照；取消下载时 worker 并没有被 join，
    /// 它可能在墓碑落盘之后才收尾并调用 `save_task`，于是任务文件被重新写出来
    /// （进程重启后用户会看到自己删掉的任务又回来了）。任务 id 是 UUID 且永不复用，
    /// 所以「已删除」是永久事实，直接在这里记住即可。
    dead: Arc<Mutex<std::collections::HashSet<String>>>,
    /// writer 线程句柄：`shutdown` 时用它 join，保证退出前数据已落盘
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

type Queue = (Mutex<QueueState>, Condvar);

#[derive(Default)]
struct QueueState {
    map: HashMap<String, (u64, Option<Task>)>,
    /// writer 线程是否正在处理一批数据
    working: bool,
    /// 累计写盘失败次数。`save_task` 在入队前取一次、落盘后再取一次，
    /// 用增量判断「我这次提交的写入有没有失败」，而不是把上一次的失败算到这次头上。
    failures: u64,
    /// 最近一次写盘失败的描述（含任务 id 与系统错误）
    last_error: Option<String>,
    /// writer 线程是否还在。false 时 flush 立即报错，
    /// 绝不无限等待一个已经死掉的线程（否则磁盘满 / 线程崩溃会变成 UI 卡死）。
    writer_alive: bool,
    /// 关闭标志：writer 处理完手上这批后退出
    stop: bool,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tasks"))?;
        let queue: Arc<Queue> = Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let dead: Arc<Mutex<std::collections::HashSet<String>>> =
            Arc::new(Mutex::new(std::collections::HashSet::new()));
        // 专用 writer 线程：唯一真正落盘者，批量、去重、串行
        queue
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .writer_alive = true;
        let wq = queue.clone();
        let wroot = root.clone();
        let wdead = dead.clone();
        // 线程创建失败必须上抛：否则所有写入都会静默丢失，而 UI 一切正常
        let handle = std::thread::Builder::new()
            .name("felldl-store-writer".into())
            .spawn(move || writer_loop(wroot, wq, wdead))
            .map_err(|e| io::Error::new(e.kind(), format!("无法创建持久化线程: {e}")))?;
        Ok(Self {
            root,
            write_lock: Mutex::new(()),
            tmp_seq: AtomicU64::new(0),
            queue,
            dead,
            writer: Mutex::new(Some(handle)),
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
        // 已删除任务的迟到快照直接丢弃：否则 worker 收尾会把墓碑覆盖掉
        if self.is_dead(&task.id) {
            return;
        }
        let rev = self.next_rev();
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.map.insert(task.id.clone(), (rev, Some(task)));
        cv.notify_one();
    }

    /// 任务是否已被删除（墓碑永久有效，id 为 UUID 不会复用）
    pub fn is_dead(&self, id: &str) -> bool {
        self.dead
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }

    /// 阻塞直到队列已排空（测试/退出时使用）。
    ///
    /// writer 线程已退出时立即返回错误：宁可报告「没能保存」，
    /// 也不能让调用方（或 UI 线程）无限等待一个不会再来处理队列的线程。
    pub fn flush(&self) -> io::Result<()> {
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !g.writer_alive {
                return Err(writer_gone());
            }
            if g.map.is_empty() && !g.working {
                return Ok(());
            }
            g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// 当前累计失败次数（内部用）
    fn failure_count(&self) -> u64 {
        let (lock, _) = &*self.queue;
        lock.lock().unwrap_or_else(|e| e.into_inner()).failures
    }

    /// 自 `since` 以来是否有写入失败；有则返回带原因的错误
    fn error_since(&self, since: u64) -> io::Result<()> {
        let (lock, _) = &*self.queue;
        let g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if g.failures > since {
            let msg = g
                .last_error
                .clone()
                .unwrap_or_else(|| "任务状态写入磁盘失败".to_string());
            return Err(io::Error::other(msg));
        }
        Ok(())
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

    /// 同步保存（状态变更/终态等需要立即持久化的场景）：入队、等待落盘，并**确认真的写成功**。
    ///
    /// 以前这里永远返回 `Ok(())`：磁盘满、目录只读、writer 线程死掉时，用户看到「已暂停」，
    /// 重启后任务却回到上一个状态。现在失败会带着系统错误返回。
    pub fn save_task(&self, task: &Task) -> io::Result<()> {
        let before = self.failure_count();
        self.enqueue_save(task);
        self.flush()?;
        self.error_since(before)
    }

    /// 同步删除任务记录，同样会确认墓碑真的落盘
    pub fn delete_task(&self, id: &str) -> io::Result<()> {
        let before = self.failure_count();
        // 先立墓碑再入队：保证「删除之后才到达的快照」一律被拒绝，
        // 而不是依赖队列先后顺序去赌 worker 的收尾时机。
        self.dead
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string());
        // 通过队列写入"墓碑"：writer 线程串行处理，天然与在途进度保存排序，
        // 不会出现「先删除、随后旧快照又写回来」的复活问题。
        let rev = self.next_rev();
        let (lock, cv) = &*self.queue;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.map.insert(id.to_string(), (rev, None));
        cv.notify_all();
        drop(g);
        self.flush()?;
        self.error_since(before)
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

    /// 显式关闭：把队列里剩余的写入落盘，然后让 writer 退出并 join。
    ///
    /// 进程退出前调用它可以保证「最后一次状态变更」不会丢在半路；
    /// 重复调用是安全的（第二次没有线程可 join，直接返回）。
    pub fn shutdown(&self) {
        {
            let (lock, cv) = &*self.queue;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            g.stop = true;
            cv.notify_all();
        }
        // 等 writer 处理完手上这批（writer 只在队列为空时才退出）
        {
            let (lock, cv) = &*self.queue;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while g.writer_alive && std::time::Instant::now() < deadline {
                g = cv
                    .wait_timeout(g, std::time::Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        let handle = self.writer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = handle {
            // writer 已经收到 stop 且队列已空，这里不会长期阻塞
            let _ = h.join();
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // 保证离开作用域时（含测试、进程正常退出）后台线程不会悬挂，
        // 也不会有「已入队但没落盘」的写入被悄悄丢掉
        self.shutdown();
    }
}

/// writer 已经不在时的统一错误：调用方据此知道「这次保存没有成功」
fn writer_gone() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "持久化线程已退出，任务状态未能保存",
    )
}

/// 专用写盘线程：从合并队列取快照，批量、去重地落盘。
/// 同一 id 的多次进度更新在队列里只保留最新一份；删除用墓碑（None）表示，
/// 且墓碑总是覆盖旧快照，避免任务被复活。
fn writer_loop(root: PathBuf, q: Arc<Queue>, dead: Arc<Mutex<std::collections::HashSet<String>>>) {
    // 不管正常退出还是 panic，都要把「writer 已不在」告诉等待者：
    // 否则 flush() 会永远等下去，UI 表现为「保存时卡死」。
    struct Alive(Arc<Queue>);
    impl Drop for Alive {
        fn drop(&mut self) {
            let (lock, cv) = &*self.0;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            g.writer_alive = false;
            cv.notify_all();
        }
    }
    let _alive = Alive(q.clone());

    let tasks_dir = root.join("tasks");
    let seq = AtomicU64::new(0);
    let (lock, cv) = &*q;
    loop {
        // 取当前一批待写项
        let batch: Vec<(String, Option<Task>)> = {
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if (g.stop && g.map.is_empty()) || !g.writer_alive {
                    return;
                }
                if !g.map.is_empty() {
                    break;
                }
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

        let mut errors: Vec<String> = Vec::new();
        for (id, item) in batch {
            let path = tasks_dir.join(format!("{id}.json"));
            match item {
                Some(task) => {
                    // 快照可能在墓碑立起之前就已经在队列里，落盘前再确认一次
                    if dead.lock().unwrap_or_else(|e| e.into_inner()).contains(&id) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    match serde_json::to_vec_pretty(&task) {
                        Ok(data) => {
                            if let Err(e) = write_atomic(&path, &data, &seq) {
                                errors.push(format!("任务 {id} 状态写盘失败: {e}"));
                            }
                        }
                        Err(e) => errors.push(format!("任务 {id} 序列化失败: {e}")),
                    }
                }
                None => {
                    // 墓碑：删除任务文件 + 残留 tmp
                    if let Err(e) = fs::remove_file(&path) {
                        if e.kind() != io::ErrorKind::NotFound {
                            errors.push(format!("任务 {id} 记录删除失败: {e}"));
                        }
                    }
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

        // 本批处理完：记账失败、唤醒 flush() 等待者
        {
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(first) = errors.first() {
                eprintln!("[store] {first}");
                g.failures += errors.len() as u64;
                g.last_error = Some(if errors.len() > 1 {
                    format!("{}（共 {} 项失败）", first, errors.len())
                } else {
                    first.clone()
                });
            }
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

    fn sample_task(id: &str) -> Task {
        Task {
            id: id.into(),
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
            error_code: None,
            retryable: None,
            rev: 0,
            connections: 8,
            segments: vec![],
            supports_range: true,
            etag: None,
            last_modified: None,
            content_type: None,
            referer: None,
            cookie: None,
            auth_origin: String::new(),
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
            resume_anchor: None,
        }
    }

    #[test]
    fn roundtrip_task() {
        let dir = tmp_dir("rt");
        let store = Store::new(&dir).unwrap();
        let mut t = sample_task("abc");
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

    /// 删除后 worker 收尾写回的旧快照不能把任务复活。
    #[test]
    fn deleted_task_is_not_resurrected_by_late_snapshot() {
        let dir = tmp_dir("tomb");
        let store = Store::new(&dir).unwrap();
        let mut t = sample_task("gone");
        store.save_task(&t).unwrap();
        assert!(store.task_path("gone").exists());

        store.delete_task("gone").unwrap();
        assert!(!store.task_path("gone").exists(), "删除应立即落盘");

        // 取消下载时 worker 并没有被 join：它会带着旧 Arc 再存一次
        t.downloaded = 999;
        t.state = TaskState::Paused;
        store.save_task(&t).unwrap();
        assert!(
            !store.task_path("gone").exists(),
            "删除后迟到的快照不得把任务文件写回来"
        );
        assert!(store.load_tasks().is_empty(), "重启后不应再看到已删除任务");
        assert!(store.is_dead("gone"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 反向场景：墓碑只对同一个 id 生效，别的任务照常保存。
    #[test]
    fn tombstone_does_not_affect_other_tasks() {
        let dir = tmp_dir("tomb2");
        let store = Store::new(&dir).unwrap();
        store.save_task(&sample_task("a")).unwrap();
        store.delete_task("a").unwrap();
        store.save_task(&sample_task("b")).unwrap();
        let ids: Vec<String> = store.load_tasks().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["b".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 写盘失败必须返回错误：以前 `save_task` 永远 Ok，磁盘满/目录只读时
    /// 用户看到「已暂停」，重启后任务却回到下载中。
    #[test]
    fn save_task_reports_write_failure() {
        let dir = tmp_dir("writefail");
        let store = Store::new(&dir).unwrap();
        // 用一个同名目录占住任务文件名：tmp 写完 rename 到它上面必然失败
        fs::create_dir_all(dir.join("tasks").join("bad.json")).unwrap();

        let err = store
            .save_task(&sample_task("bad"))
            .expect_err("写盘失败必须被报告");
        assert!(
            err.to_string().contains("bad"),
            "错误信息应指明是哪个任务：{err}"
        );

        // 别的任务不受影响，仍然可以正常保存
        store.save_task(&sample_task("ok")).unwrap();
        // 失败过一次之后，后续成功保存不应再被旧错误污染
        store.save_task(&sample_task("ok")).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    /// 删除记录失败同样要能返回（例如被同名目录占位、目录只读）
    #[test]
    fn delete_task_reports_failure() {
        let dir = tmp_dir("delfail");
        let store = Store::new(&dir).unwrap();
        // 让 remove_file 失败：把目标做成非空目录
        let victim = dir.join("tasks").join("stuck.json");
        fs::create_dir_all(victim.join("inside")).unwrap();

        let err = store.delete_task("stuck").expect_err("删除失败必须被报告");
        assert!(err.to_string().contains("stuck"), "错误应带任务 id：{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// writer 已退出后，保存必须立刻报错而不是无限等待（UI 不能卡死）
    #[test]
    fn save_fails_fast_after_shutdown() {
        let dir = tmp_dir("shutdown");
        let store = Store::new(&dir).unwrap();
        store.save_task(&sample_task("a")).unwrap();
        store.shutdown();

        let started = std::time::Instant::now();
        let err = store
            .save_task(&sample_task("b"))
            .expect_err("writer 已退出，保存不能报告成功");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "不能等死掉的 writer，实际耗时 {:?}",
            started.elapsed()
        );
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
        // 重复关闭安全
        store.shutdown();
        let _ = fs::remove_dir_all(&dir);
    }

    /// 关闭时要把已入队但还没落盘的写入处理完，不能丢
    #[test]
    fn shutdown_flushes_pending_snapshot() {
        let dir = tmp_dir("shutdown2");
        let store = Store::new(&dir).unwrap();
        let mut t = sample_task("pending");
        t.downloaded = 4242;
        // 只入队不等落盘，随后立即关闭
        store.enqueue_save_owned(t);
        store.shutdown();
        assert!(
            dir.join("tasks").join("pending.json").exists(),
            "关闭前入队的快照必须在退出时落盘"
        );
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
