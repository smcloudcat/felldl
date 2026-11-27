//! 可重试失败的自动重试排期。
//!
//! 背景：网络抖一下、切 Wi‑Fi、Doze 期间 socket 被掐，任务就会直接进 `Error`，用户必须
//! 手动点「继续」。实测（A03 批次 B）断网 8 秒即失败、网络恢复 25 秒后仍停在错误——
//! 断点续传本身是好的，缺的只是「没人去点那一下」。
//!
//! 这里只做**排期**，不碰状态机：什么时候再试、试了几次、要不要放弃。
//! 真正的重试仍然是 `Engine::resume`。
//!
//! 三条约束，写死了才好解释给用户听：
//! 1. **只重试可重试的错误**（`FellError::retryable()`）：404/403/签名不符这类再试一万次
//!    也没用，重试它们只会把「立刻知道结果」拖成「三分钟后还是失败」。
//! 2. **次数与退避都有上限**：2s → 5s → 15s → 45s → 120s，最多 5 次，之后彻底停下并把
//!    错误留在界面上。无限重试会变成一个用户看不见的后台噪音源。
//! 3. **网络不可用时不烧次数**。断网时按退避一直重试，三分钟就把预算耗光；等用户把
//!    Wi‑Fi 打开时早已放弃。所以「网络不可用」时只是挂着等，恢复的那一刻立刻试一次。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::model::TaskId;

/// 最多自动重试几次（不含用户手动重试）
pub const MAX_ATTEMPTS: u8 = 5;

/// 第 `attempt` 次重试前等多久。1 起算。
pub fn backoff_secs(attempt: u8) -> u64 {
    match attempt {
        1 => 2,
        2 => 5,
        3 => 15,
        4 => 45,
        _ => 120,
    }
}

/// 断网期间第 `probes` 次探测前等多久：5s → 10s → 20s → 40s → 60s 封顶
fn offline_probe_secs(probes: u8) -> u64 {
    let shift = probes.saturating_sub(1).min(4) as u32;
    (OFFLINE_PROBE_START_SECS << shift).min(OFFLINE_PROBE_MAX_SECS)
}

/// 单个任务的重试状态
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    /// 已经安排过几次（含正在等待的这一轮）
    attempts: u8,
    /// 什么时候可以试；`None` 表示「已经发出去了，等结果」
    due: Option<Instant>,
    /// 断网期间探测过几次：只影响探测间隔，不消耗 `attempts`
    probes: u8,
}

/// 断网期间重新探测的间隔：5s 起，逐步放宽到 60s 封顶。
///
/// 为什么断网还要探测：平台上报的「有没有网」是外部状态，可能因为一次查询异常、
/// 权限变更、ROM 差异而长期停在「没有」。只靠它做闸门，一旦它错了，任务就会永远
/// 停在 `Error` 上。这里让引擎自己隔一会儿试一次：网络真没恢复，连接立刻失败（不烧次
/// 数、几乎没有流量）；恢复了就当场续上，最多晚 60 秒。
const OFFLINE_PROBE_START_SECS: u64 = 5;
const OFFLINE_PROBE_MAX_SECS: u64 = 60;

/// 排期被「发出」后如果没能真正把任务拉起来，隔多久重排
pub const REARM_SECS: u64 = 10;

/// 所有待重试任务的排期表。
#[derive(Default)]
pub struct RetryBook {
    plans: HashMap<TaskId, Plan>,
}

impl RetryBook {
    /// 记录一次可重试的失败，返回安排的第几次；`None` = 次数用尽，不再自动重试。
    ///
    /// 注意这是**递增**的：同一任务反复失败会一路走到上限，只有用户手动「继续」
    /// 才会通过 [`clear`] 把预算重置。
    ///
    /// `online == false` 表示平台报告当前没网：此时**不递增预算**，只挂一个稍后的
    /// 探测点（5s→60s 退避）。这样「断网等待」不会把 5 次机会耗光，同时又不把
    /// 恢复的希望完全押在平台状态上。
    pub fn on_failure(&mut self, id: &str, now: Instant, online: bool) -> Option<u8> {
        let prev = self.plans.get(id).cloned().unwrap_or(Plan {
            attempts: 0,
            due: None,
            probes: 0,
        });
        if !online {
            let attempts = prev.attempts.max(1);
            let probes = prev.probes.saturating_add(1);
            let wait = offline_probe_secs(probes);
            self.plans.insert(
                id.to_string(),
                Plan {
                    attempts,
                    due: Some(now + Duration::from_secs(wait)),
                    probes,
                },
            );
            return Some(attempts);
        }
        let attempts = prev.attempts.saturating_add(1);
        if attempts > MAX_ATTEMPTS {
            // 用尽了就把记录也丢掉：留着没有意义，还会让「是否在等重试」的判断含糊
            self.plans.remove(id);
            return None;
        }
        self.plans.insert(
            id.to_string(),
            Plan {
                attempts,
                due: Some(now + Duration::from_secs(backoff_secs(attempts))),
                probes: 0,
            },
        );
        Some(attempts)
    }

    /// 把「发出去了但没结果」的排期重新挂上。
    ///
    /// 场景：tick 里 `fired()` 之后 `resume_impl` 早退（任务已被并发泵拉起、
    /// 文件被占用等），既没开始传输也没走到 `fail_task`。若不管，这条排期的
    /// `due` 会一直是 `None`，`waiting()` 捞不到、`due()` 也捞不到，任务就永久
    /// 停在错误态——界面上写「稍后重试」，实际上永远不会试。
    pub fn rearm(&mut self, id: &str, now: Instant) -> bool {
        let Some(p) = self.plans.get_mut(id) else {
            return false;
        };
        if p.due.is_some() {
            return false;
        }
        p.due = Some(now + Duration::from_secs(REARM_SECS));
        true
    }

    /// 已经到点的任务（不改变状态）
    pub fn due(&self, now: Instant) -> Vec<TaskId> {
        let mut out: Vec<TaskId> = self
            .plans
            .iter()
            .filter(|(_, p)| p.due.map(|d| d <= now).unwrap_or(false))
            .map(|(id, _)| id.clone())
            .collect();
        // 同一次 tick 里的顺序稳定，测试与日志都好读
        out.sort();
        out
    }

    /// 标记「这一轮已经发出去了」，避免同一任务在一轮里被连发两次
    pub fn fired(&mut self, id: &str) {
        if let Some(p) = self.plans.get_mut(id) {
            p.due = None;
        }
    }

    /// 所有待重试的（不论是否到点）任务，用于「网络恢复」时立刻全部触发
    pub fn waiting(&self) -> Vec<TaskId> {
        let mut out: Vec<TaskId> = self
            .plans
            .iter()
            .filter(|(_, p)| p.due.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        out.sort();
        out
    }

    /// 把某个任务的排期提前到「现在就试」（网络刚恢复）
    pub fn hurry(&mut self, id: &str, now: Instant) {
        if let Some(p) = self.plans.get_mut(id) {
            if p.due.is_some() {
                p.due = Some(now);
            }
        }
    }

    /// 清掉排期。用户手动继续 / 暂停 / 取消、任务完成或删除时都要调用——
    /// 否则下一次 tick 会把用户刚做的决定覆盖掉。
    pub fn clear(&mut self, id: &str) {
        self.plans.remove(id);
    }

    pub fn attempts(&self, id: &str) -> u8 {
        self.plans.get(id).map(|p| p.attempts).unwrap_or(0)
    }

    pub fn is_waiting(&self, id: &str) -> bool {
        self.plans.get(id).map(|p| p.due.is_some()).unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.plans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let seq: Vec<u64> = (1..=6).map(backoff_secs).collect();
        assert_eq!(seq, vec![2, 5, 15, 45, 120, 120], "退避应递增并封顶");
        assert!(
            seq.windows(2).all(|w| w[0] <= w[1]),
            "退避不允许出现变短（会让重试越来越密）"
        );
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let now = Instant::now();
        let mut book = RetryBook::default();
        for i in 1..=MAX_ATTEMPTS {
            assert_eq!(book.on_failure("t", now, true), Some(i), "第 {i} 次应安排");
            book.fired("t");
        }
        assert_eq!(
            book.on_failure("t", now, true),
            None,
            "超过上限后必须停下，不能靠退避无限重试"
        );
        assert!(book.is_empty(), "放弃后不留残记录");
    }

    #[test]
    fn due_only_after_backoff_and_only_once_per_round() {
        let t0 = Instant::now();
        let mut book = RetryBook::default();
        assert_eq!(book.on_failure("a", t0, true), Some(1));
        assert!(book.due(t0).is_empty(), "退避期内不该触发");
        let later = t0 + Duration::from_secs(backoff_secs(1));
        assert_eq!(book.due(later), vec!["a".to_string()]);
        book.fired("a");
        assert!(
            book.due(later).is_empty(),
            "已发出的不能在同一时刻被重复触发"
        );
        assert!(!book.is_waiting("a"), "发出后进入等结果状态");
    }

    #[test]
    fn hurry_moves_pending_retry_to_now() {
        let t0 = Instant::now();
        let mut book = RetryBook::default();
        book.on_failure("a", t0, true);
        assert!(book.due(t0).is_empty());
        book.hurry("a", t0); // 网络恢复
        assert_eq!(book.due(t0), vec!["a".to_string()], "网络恢复应立刻重试");
    }

    #[test]
    fn clear_resets_budget_for_user_retry() {
        let now = Instant::now();
        let mut book = RetryBook::default();
        for _ in 0..MAX_ATTEMPTS {
            book.on_failure("a", now, true);
            book.fired("a");
        }
        assert_eq!(book.on_failure("a", now, true), None);
        book.clear("a");
        assert_eq!(
            book.on_failure("a", now, true),
            Some(1),
            "用户手动继续后预算应重置"
        );
    }

    #[test]
    fn tasks_are_independent() {
        let t0 = Instant::now();
        let mut book = RetryBook::default();
        book.on_failure("a", t0, true);
        book.on_failure("a", t0, true);
        book.on_failure("b", t0, true);
        assert_eq!(book.attempts("a"), 2);
        assert_eq!(book.attempts("b"), 1, "一个任务失败不该消耗别人的预算");
        book.clear("a");
        assert_eq!(book.attempts("b"), 1);
    }

    #[test]
    fn offline_probe_backs_off_to_a_cap() {
        let seq: Vec<u64> = (1..=7).map(offline_probe_secs).collect();
        assert_eq!(seq, vec![5, 10, 20, 40, 60, 60, 60], "探测间隔应放宽并封顶");
        assert!(seq.len() > 2 && seq[0] < seq[2] && seq[6] == OFFLINE_PROBE_MAX_SECS);
    }

    /// 断网期间反复失败：预算必须停在 1/5，但每一轮都要有明确的探测点。
    ///
    /// 这是「平台说没网」和「引擎自己试一次」的折中：前者决定文案与是否计数，
    /// 后者保证网络回来时不被一个错误的外部状态永久挡住。
    #[test]
    fn offline_failures_keep_budget_and_always_reschedule() {
        let t0 = Instant::now();
        let mut book = RetryBook::default();
        let mut now = t0;
        for round in 1..=30u8 {
            let n = book
                .on_failure("a", now, false)
                .expect("断网不应放弃排期，只是不计数");
            assert_eq!(n, 1, "第 {round} 轮不该把预算推到 2/5");
            assert!(book.is_waiting("a"), "第 {round} 轮必须留下探测点");
            let later = now + Duration::from_secs(OFFLINE_PROBE_MAX_SECS);
            assert_eq!(
                book.due(later),
                vec!["a".to_string()],
                "第 {round} 轮最迟 60s 内要再试一次"
            );
            book.fired("a");
            now = later;
        }
        assert_eq!(book.attempts("a"), 1, "30 轮断网失败也不能烧掉预算");
    }

    /// `fired()` 之后如果 `resume_impl` 没能真正拉起任务，排期会停在 `due=None`：
    /// 既不在 `waiting()` 也不在 `due()`，任务就永久卡在错误态。
    #[test]
    fn rearm_recovers_plan_orphaned_after_fired() {
        let t0 = Instant::now();
        let mut book = RetryBook::default();
        book.on_failure("a", t0, true);
        book.fired("a");
        assert!(!book.is_waiting("a"), "发出后进入等结果状态");
        assert!(book.due(t0 + Duration::from_secs(600)).is_empty());

        assert!(book.rearm("a", t0), "孤儿排期应被重新挂上");
        assert!(book.is_waiting("a"));
        assert_eq!(
            book.due(t0 + Duration::from_secs(REARM_SECS)),
            vec!["a".to_string()],
            "重排后应在 REARM_SECS 内再试一次"
        );
        assert!(
            !book.rearm("a", t0),
            "已经挂上的排期不该被重复推迟（否则会一直往后拖）"
        );
    }
}
