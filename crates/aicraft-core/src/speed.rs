//! 下载限速：把一个总速率配额均分给同时进行的下载。
//!
//! 为什么不是「每个下载各限 1 MB/s」：用户设的是**总带宽**，不是单条上限。
//! 十六个文件同时下、每人 1 MB/s 就是 16 MB/s，把用户的网吃干净了。
//! 这里维护一个「当前有几个下载在跑」的计数，每个下载进场时按
//! `总量 / 在场数` 领自己的那份，丢下时把名额还回去。
//!
//! 分配是**进场时一次算定**的：curl 启动后参数就固定了，中途改不了。
//! 所以早期的下载分到的份额偏小，等别的下完了，后进场的会拿到更大的一份；
//! 最后一个文件能独享全部配额。整体平均贴着用户设的值，不会超。
//!
//! 内部用 `Arc` 持有计数，所以 [`Budget`] 可以随手 `clone()` 进各个工作线程，
//! 大家共享同一份在场数。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 单个下载最少给这么多 KB/s。
///
/// 不然「总限 32 KB/s + 16 并发」会算出每份 2 KB/s，
/// 慢到看起来像卡死，还会因为 curl 的 `--max-time` 超时。
/// 宁可略微超一点总配额，也不要一份都下不完。
const MIN_PER_STREAM_KBPS: u32 = 8;

/// 一次下载任务共享的带宽预算。
#[derive(Debug, Clone)]
pub struct Budget {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    total_kbps: Option<u32>,
    active: AtomicUsize,
}

impl Budget {
    /// 不限速。
    pub fn unlimited() -> Budget {
        Budget::new(None)
    }

    /// `kbps` 为总上限；`None` 或 0 表示不限速。
    pub fn new(kbps: Option<u32>) -> Budget {
        Budget {
            inner: Arc::new(Inner {
                total_kbps: kbps.filter(|k| *k > 0),
                active: AtomicUsize::new(0),
            }),
        }
    }

    /// 总上限（KB/s）；None 表示不限速。
    pub fn total_kbps(&self) -> Option<u32> {
        self.inner.total_kbps
    }

    /// 当前在场（正在下载）的条数。
    pub fn active(&self) -> usize {
        self.inner.active.load(Ordering::Relaxed)
    }

    /// 进场：占一个名额，返回的 guard 丢弃时自动归还。
    pub fn enter(&self) -> Share<'_> {
        self.inner.active.fetch_add(1, Ordering::Relaxed);
        Share { parent: self }
    }

    /// 当前这一份该限到多少 KB/s；None 表示不限。
    ///
    /// 调用时应已 `enter()`——分母要算上自己这条流。
    pub fn per_stream_kbps(&self) -> Option<u32> {
        let total = self.inner.total_kbps?;
        let active = self.inner.active.load(Ordering::Relaxed).max(1) as u32;
        Some((total / active).max(MIN_PER_STREAM_KBPS))
    }
}

/// 一次下载占用的名额，Drop 时归还。
#[derive(Debug)]
pub struct Share<'a> {
    parent: &'a Budget,
}

impl Share<'_> {
    /// 这一份该限到多少 KB/s；None 表示不限。
    pub fn kbps(&self) -> Option<u32> {
        self.parent.per_stream_kbps()
    }
}

impl Drop for Share<'_> {
    fn drop(&mut self) {
        // 只减不检查下溢：名额与 guard 一一对应，出问题也该早暴露。
        self.parent.inner.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 给界面/日志用的一句话。
pub fn describe(kbps: Option<u32>) -> String {
    match kbps {
        None => "不限速".to_string(),
        Some(k) if k >= 1024 => format!("{:.1} MB/s", k as f64 / 1024.0),
        Some(k) => format!("{} KB/s", k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_limits() {
        let b = Budget::unlimited();
        let share = b.enter();
        assert_eq!(share.kbps(), None);
        assert_eq!(b.active(), 1);
    }

    #[test]
    fn zero_means_unlimited() {
        let b = Budget::new(Some(0));
        assert_eq!(b.total_kbps(), None);
        assert_eq!(b.enter().kbps(), None);
    }

    #[test]
    fn lone_stream_gets_the_whole_budget() {
        let b = Budget::new(Some(4096));
        assert_eq!(b.enter().kbps(), Some(4096));
    }

    #[test]
    fn budget_is_split_between_streams() {
        let b = Budget::new(Some(4096));
        let a = b.enter();
        let c = b.enter();
        let d = b.enter();
        assert_eq!(b.active(), 3);
        // 已在场的每条都按「总量 / 在场数」算，三条各 1365
        assert_eq!(a.kbps(), Some(1365));
        assert_eq!(c.kbps(), Some(1365));
        assert_eq!(d.kbps(), Some(1365));
    }

    #[test]
    fn share_returns_when_dropped() {
        let b = Budget::new(Some(1024));
        {
            let _a = b.enter();
            let _c = b.enter();
            assert_eq!(b.active(), 2);
        }
        assert_eq!(b.active(), 0);
        // 都走光了，后来者独享
        assert_eq!(b.enter().kbps(), Some(1024));
    }

    #[test]
    fn clone_shares_the_same_counter() {
        // 工作线程拿到的是 clone，必须看到同一份在场数
        let b = Budget::new(Some(1024));
        let other = b.clone();
        let _guard = b.enter();
        assert_eq!(other.active(), 1);
        assert_eq!(other.enter().kbps(), Some(512));
    }

    #[test]
    fn tiny_budget_still_gives_a_usable_floor() {
        let b = Budget::new(Some(32));
        let _guards: Vec<Share<'_>> = (0..16).map(|_| b.enter()).collect();
        // 32/16 = 2，低于下限，抬到 8
        assert_eq!(b.per_stream_kbps(), Some(MIN_PER_STREAM_KBPS));
    }

    #[test]
    fn describe_is_readable() {
        assert_eq!(describe(None), "不限速");
        assert_eq!(describe(Some(512)), "512 KB/s");
        assert_eq!(describe(Some(2048)), "2.0 MB/s");
    }
}
