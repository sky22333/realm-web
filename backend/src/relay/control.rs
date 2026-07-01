//! 单条规则的运行时生命周期：配额检测、停服信号、子连接跟踪。

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::domain::TrafficTotals;

struct RuleSession {
    shutdown: CancellationToken,
    tracker: TaskTracker,
}

impl RuleSession {
    fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
        }
    }
}

/// 与本地端口绑定的转发控制面（meter 记账触发 trip，relay 监听 shutdown）。
pub struct RuleControl {
    port: u16,
    quota_bytes: AtomicI64,
    tripped: AtomicBool,
    session: RwLock<RuleSession>,
    trip_tx: mpsc::UnboundedSender<u16>,
}

impl RuleControl {
    pub fn new(port: u16, trip_tx: mpsc::UnboundedSender<u16>) -> Self {
        Self {
            port,
            quota_bytes: AtomicI64::new(0),
            tripped: AtomicBool::new(false),
            session: RwLock::new(RuleSession::new()),
            trip_tx,
        }
    }

    pub fn set_quota(&self, bytes: i64) {
        self.quota_bytes.store(bytes, Ordering::Relaxed);
    }

    /// 启动新一轮转发会话（手动启用 / 规则启动时调用）。
    pub async fn start_session(&self) {
        let mut session = self.session.write().await;
        *session = RuleSession::new();
        self.tripped.store(false, Ordering::Release);
    }

    pub async fn shutdown_token(&self) -> CancellationToken {
        self.session.read().await.shutdown.clone()
    }

    pub async fn tracker(&self) -> TaskTracker {
        self.session.read().await.tracker.clone()
    }

    /// 停止 accept/recv 并关闭所有子连接。
    pub async fn stop_session(&self) {
        let session = self.session.read().await;
        session.shutdown.cancel();
        session.tracker.close();
    }

    pub async fn wait_session(&self) {
        let tracker = self.session.read().await.tracker.clone();
        tracker.wait().await;
    }

    /// meter 每次入账后检查；超额则立即 cancel 并通知写库停服。
    pub fn check_after_add(&self, totals: TrafficTotals) {
        let quota = self.quota_bytes.load(Ordering::Relaxed);
        if quota <= 0 {
            return;
        }
        if (totals.total_bytes() as i64) < quota {
            return;
        }
        if self.tripped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(session) = self.session.try_read() {
            session.shutdown.cancel();
            session.tracker.close();
        }
        let _ = self.trip_tx.send(self.port);
    }
}
