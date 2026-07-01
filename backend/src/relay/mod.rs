//! realm_core 转发栈：DNS + realm_io（TCP）+ batched UDP + 端口级流量统计。

mod control;
mod counted;
mod endpoint;
mod meter;
mod tcp;
mod udp;

pub use control::RuleControl;
pub use endpoint::endpoint_from_rule;
pub use meter::TrafficMeter;

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

use crate::domain::RuleRecord;

/// 单次转发会话的控制信号（shutdown + 子 task 跟踪）。
pub struct RelaySession {
    pub shutdown: CancellationToken,
    pub tracker: TaskTracker,
}

/// 运行中的 TCP/UDP 转发任务句柄。
pub struct RelayHandle {
    pub tcp: JoinHandle<()>,
    pub udp: JoinHandle<()>,
}

impl RelayHandle {
    pub fn abort(&self) {
        self.tcp.abort();
        self.udp.abort();
    }
}

/// 启动单条规则的 TCP + UDP 转发（realm_core 栈 + 端口级计量）。
pub fn start_rule(
    rule: &RuleRecord,
    meter: Arc<TrafficMeter>,
    session: RelaySession,
) -> anyhow::Result<RelayHandle> {
    let endpoint = endpoint_from_rule(rule)?;

    info!(
        port = rule.local_port,
        target = %format!("{}:{}", rule.target_host, rule.target_port),
        "启动 realm 转发"
    );

    let tcp = tokio::spawn(tcp::run_tcp(
        endpoint.clone(),
        meter.clone(),
        session.shutdown.clone(),
        session.tracker.clone(),
    ));
    let udp = tokio::spawn(udp::run_udp(endpoint, meter, session));

    Ok(RelayHandle { tcp, udp })
}
