//! 下载取消令牌。
//!
//! 下载是通过子进程 `curl` 做的，取消没法「打断函数」——只能由一个共享的
//! 标志位通知正在等的人：`curl` 的轮询循环每次醒来先看一眼这个标志，
//! 被置位就杀掉子进程并返回「已取消」。
//!
//! 用 [`AtomicBool`] 而不是 channel，是因为要多处同时读（每个 worker 各一份
//! 克隆），而写只有一处（界面上那个按钮）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 一次下载任务的取消开关。克隆出来的都是同一个开关。
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// 新建一个没被取消的开关。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消。重复调用没有副作用。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// 是否已经被请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_starts_live_and_cancels_once() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());

        let clone = token.clone();
        token.cancel();
        // 克隆看到的是同一个开关
        assert!(clone.is_cancelled());
        assert!(token.is_cancelled());

        // 再按一次也没事
        clone.cancel();
        assert!(token.is_cancelled());
    }
}
