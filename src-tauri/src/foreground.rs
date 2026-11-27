//! 安卓下载前台服务的启停。
//!
//! 为什么要它：声明 `FOREGROUND_SERVICE` 权限本身没有任何作用，必须真的 `startForeground`。
//! 有了前台状态，进程不会被当成「后台缓存进程」冻结/回收，用户也能看到「正在下载」。
//!
//! 关键约束（真机踩出来的）：**Android 12+ 不允许从后台启动前台服务**。
//! 在熄屏、应用退到后台之后再调用 `startForegroundService`，系统直接给
//! `allowStartForeground=DENIED`（uidState=TPSL）：通知不会出现、服务也不会真的进前台。
//! 所以启动时机必须是「用户刚点完下载、界面还在前台」那一刻——本模块因此被两处调用：
//! 1. 引擎事件里一出现「开始传输」立刻同步一次（此时应用通常仍可见）；
//! 2. 2s 轮询兜底，负责停止、以及应用回到前台后的补启。
//!
//! 真正的 Service 在 Kotlin 侧（`DownloadService`），不持有任何下载状态：
//! 任务与断点都在引擎里，服务被系统杀掉后由下一轮同步重新拉起即可。
//!
//! 非 Android 平台整个模块是空操作：桌面端不需要这层。

#[derive(Default)]
struct State {
    /// 上次成功同步给系统的期望值
    synced: Option<usize>,
    /// 上次尝试过的期望值（用来判断「目标变了就该重新计失败次数」）
    attempted: Option<usize>,
    /// 同一目标的连续失败次数：被系统拒绝后不要每 2 秒撞同一堵墙
    fails: u8,
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State {
    synced: None,
    attempted: None,
    fails: 0,
});

/// 同一个目标最多试几次；之后等目标变化再试
const MAX_FAILS: u8 = 3;

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    match STATE.lock() {
        Ok(mut s) => f(&mut s),
        Err(p) => {
            let mut s = p.into_inner();
            f(&mut s)
        }
    }
}

/// 是否需要动作：目标与已成功的不一致，且没有把同一目标试到上限
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn needs_sync(active: usize) -> bool {
    with_state(|st| {
        if st.synced == Some(active) {
            return false;
        }
        !(st.attempted == Some(active) && st.fails >= MAX_FAILS)
    })
}

/// 按当前「正在传输的任务数」同步前台服务状态
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn sync(app: &tauri::AppHandle, active: usize) {
    if !needs_sync(active) {
        return;
    }
    // 目标变了就重置失败计数：换了个期望状态，之前被拒的原因未必还成立
    with_state(|st| {
        if st.attempted != Some(active) {
            st.attempted = Some(active);
            st.fails = 0;
        }
    });

    #[cfg(target_os = "android")]
    let result = {
        if active == 0 {
            crate::android::stop_download_service(app)
        } else {
            crate::android::start_download_service(app, &format!("正在下载 {active} 个任务"))
        }
    };
    #[cfg(not(target_os = "android"))]
    let result: Result<(), String> = {
        let _ = app;
        Ok(())
    };

    with_state(|st| match result {
        Ok(()) => {
            st.synced = Some(active);
            st.fails = 0;
        }
        Err(e) => {
            st.fails = st.fails.saturating_add(1);
            if st.fails == MAX_FAILS {
                // 只报最后一次，避免每 2 秒一条同样的日志
                eprintln!("[fgs] 前台服务同步失败（已重试 {MAX_FAILS} 次，暂停直到目标变化）: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        with_state(|st| {
            st.synced = None;
            st.attempted = None;
            st.fails = 0;
        });
    }

    #[test]
    fn syncs_only_when_target_changes() {
        reset();
        assert!(needs_sync(0), "进程刚起来不知道系统状态，应先同步一次");
        with_state(|st| st.synced = Some(0));
        assert!(!needs_sync(0), "同样的活跃数不该反复启停服务");
        assert!(needs_sync(1), "有任务时必须启动前台服务");
        with_state(|st| st.synced = Some(1));
        assert!(needs_sync(0), "任务全部结束必须停掉服务，不能一直挂着通知");
        reset();
    }

    #[test]
    fn gives_up_after_repeated_failures() {
        reset();
        with_state(|st| {
            st.attempted = Some(1);
            st.fails = MAX_FAILS;
        });
        assert!(
            !needs_sync(1),
            "连续失败达到上限后应停止重试，等目标变化再说"
        );
        assert!(needs_sync(0), "换了期望状态就该重新试");
        reset();
    }

    #[test]
    fn failure_budget_resets_when_target_changes() {
        reset();
        with_state(|st| {
            st.attempted = Some(2);
            st.fails = MAX_FAILS;
        });
        assert!(!needs_sync(2));
        assert!(needs_sync(3));
        reset();
    }
}
