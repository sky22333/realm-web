//! realm_core 转发栈：DNS + realm_io（TCP）+ batched UDP + 端口级流量统计。

mod counted;
mod endpoint;
mod meter;
mod tcp;
mod udp;

pub use endpoint::endpoint_from_rule;
pub use meter::TrafficMeter;

use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::info;

use crate::domain::RuleRecord;

/// 运行中的 TCP/UDP 转发任务句柄。
pub struct RelayHandle {
    pub tcp: Option<JoinHandle<()>>,
    pub udp: Option<JoinHandle<()>>,
}

impl RelayHandle {
    pub fn abort(self) {
        if let Some(h) = self.tcp {
            h.abort();
        }
        if let Some(h) = self.udp {
            h.abort();
        }
    }
}

/// 启动单条规则的 TCP + UDP 转发（realm_core 栈 + 端口级计量）。
pub fn start_rule(rule: &RuleRecord, meter: Arc<TrafficMeter>) -> anyhow::Result<RelayHandle> {
    let endpoint = endpoint_from_rule(rule)?;

    info!(
        port = rule.local_port,
        target = %format!("{}:{}", rule.target_host, rule.target_port),
        "启动 realm 转发"
    );

    let tcp = tokio::spawn(tcp::run_tcp(endpoint.clone(), meter.clone()));
    let udp = tokio::spawn(udp::run_udp(endpoint, meter));

    Ok(RelayHandle {
        tcp: Some(tcp),
        udp: Some(udp),
    })
}
