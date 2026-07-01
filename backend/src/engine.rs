//! 转发引擎：按端口管理 TCP/UDP 中继任务。

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{info, warn};

use crate::domain::RuleRecord;
use crate::relay::{RelayHandle, RelaySession, RuleControl, TrafficMeter, start_rule};

/// 管理各本地端口上的中继任务。
pub struct ForwardEngine {
    tasks: HashMap<u16, RelayHandle>,
}

impl ForwardEngine {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    pub fn is_running(&self, local_port: u16) -> bool {
        self.tasks.contains_key(&local_port)
    }

    pub async fn start_rule(
        &mut self,
        rule: &RuleRecord,
        meter: Arc<TrafficMeter>,
        control: Arc<RuleControl>,
    ) -> anyhow::Result<()> {
        if !rule.enabled {
            return Ok(());
        }
        if self.tasks.contains_key(&rule.local_port) {
            anyhow::bail!("端口 {} 已在转发中", rule.local_port);
        }

        control.set_quota(rule.quota_bytes.unwrap_or(0));
        control.start_session().await;
        let session = RelaySession {
            shutdown: control.shutdown_token().await,
            tracker: control.tracker().await,
        };

        let handle = start_rule(rule, meter, session)?;
        self.tasks.insert(rule.local_port, handle);

        info!(
            port = rule.local_port,
            target = %format!("{}:{}", rule.target_host, rule.target_port),
            "已启动 TCP+UDP 转发"
        );
        Ok(())
    }

    pub async fn stop_rule(
        &mut self,
        local_port: u16,
        control: &RuleControl,
    ) -> anyhow::Result<()> {
        let Some(handle) = self.tasks.remove(&local_port) else {
            return Ok(());
        };

        control.stop_session().await;
        control.wait_session().await;
        handle.abort();

        info!(port = local_port, "已停止转发");
        Ok(())
    }

    pub async fn sync_rules(
        &mut self,
        rules: &[RuleRecord],
        meters: &HashMap<u16, Arc<TrafficMeter>>,
        controls: &HashMap<u16, Arc<RuleControl>>,
    ) -> anyhow::Result<()> {
        let desired: HashMap<u16, &RuleRecord> = rules
            .iter()
            .filter(|r| r.enabled)
            .map(|r| (r.local_port, r))
            .collect();

        let to_stop: Vec<u16> = self
            .tasks
            .keys()
            .copied()
            .filter(|p| !desired.contains_key(p))
            .collect();

        for port in to_stop {
            if let Some(control) = controls.get(&port) {
                self.stop_rule(port, control).await?;
            } else {
                if let Some(handle) = self.tasks.remove(&port) {
                    handle.abort();
                }
            }
        }

        for rule in desired.values() {
            if self.tasks.contains_key(&rule.local_port) {
                continue;
            }
            let Some(meter) = meters.get(&rule.local_port) else {
                warn!(port = rule.local_port, "缺少流量计数器，跳过启动");
                continue;
            };
            let Some(control) = controls.get(&rule.local_port) else {
                warn!(port = rule.local_port, "缺少转发控制，跳过启动");
                continue;
            };
            if let Err(e) = self.start_rule(rule, meter.clone(), control.clone()).await {
                warn!(port = rule.local_port, "启动转发失败: {e:#}");
            }
        }
        Ok(())
    }
}

impl Default for ForwardEngine {
    fn default() -> Self {
        Self::new()
    }
}
