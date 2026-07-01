//! 流量统计与配额检查。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sqlx::SqlitePool;
use tokio::sync::{RwLock, mpsc};
use tracing::warn;

use crate::domain::{RuleRecord, TrafficSnapshot, TrafficTotals};
use crate::relay::{RuleControl, TrafficMeter};

/// 内存计数器 + 数据库持久化。
pub struct TrafficService {
    db: SqlitePool,
    meters: RwLock<HashMap<u16, Arc<TrafficMeter>>>,
    controls: RwLock<HashMap<u16, Arc<RuleControl>>>,
    port_to_rule: RwLock<HashMap<u16, i64>>,
    trip_tx: mpsc::UnboundedSender<u16>,
}

impl TrafficService {
    pub fn new(db: SqlitePool) -> (Arc<Self>, mpsc::UnboundedReceiver<u16>) {
        let (trip_tx, trip_rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                db,
                meters: RwLock::new(HashMap::new()),
                controls: RwLock::new(HashMap::new()),
                port_to_rule: RwLock::new(HashMap::new()),
                trip_tx,
            }),
            trip_rx,
        )
    }

    /// 为规则注册或获取计数器与控制面。
    pub async fn meter_for(&self, rule: &RuleRecord) -> Arc<TrafficMeter> {
        if let Some(meter) = self.meters.read().await.get(&rule.local_port) {
            if let Some(control) = self.controls.read().await.get(&rule.local_port) {
                control.set_quota(rule.quota_bytes.unwrap_or(0));
            }
            return meter.clone();
        }

        let mut meters = self.meters.write().await;
        let mut controls = self.controls.write().await;

        if let Some(meter) = meters.get(&rule.local_port) {
            if let Some(control) = controls.get(&rule.local_port) {
                control.set_quota(rule.quota_bytes.unwrap_or(0));
            }
            return meter.clone();
        }

        let control = Arc::new(RuleControl::new(rule.local_port, self.trip_tx.clone()));
        control.set_quota(rule.quota_bytes.unwrap_or(0));
        let meter = Arc::new(TrafficMeter::new(control.clone()));
        controls.insert(rule.local_port, control);
        meters.insert(rule.local_port, meter.clone());
        self.port_to_rule
            .write()
            .await
            .insert(rule.local_port, rule.id);
        meter
    }

    pub async fn control_for(&self, rule: &RuleRecord) -> Arc<RuleControl> {
        self.meter_for(rule).await;
        self.controls
            .read()
            .await
            .get(&rule.local_port)
            .cloned()
            .expect("control exists after meter_for")
    }

    pub async fn all_meters(&self) -> HashMap<u16, Arc<TrafficMeter>> {
        self.meters.read().await.clone()
    }

    pub async fn all_controls(&self) -> HashMap<u16, Arc<RuleControl>> {
        self.controls.read().await.clone()
    }

    pub async fn remove_port(&self, local_port: u16) {
        self.meters.write().await.remove(&local_port);
        self.controls.write().await.remove(&local_port);
        self.port_to_rule.write().await.remove(&local_port);
    }

    /// 重置指定端口的内存计数。
    pub async fn reset_meter(&self, local_port: u16) {
        if let Some(meter) = self.meters.read().await.get(&local_port) {
            meter.reset();
        }
    }

    /// 计算面板统计。
    pub async fn dashboard_stats(
        &self,
        rules: &[crate::domain::RuleRecord],
    ) -> crate::domain::DashboardStats {
        let meters = self.meters.read().await;
        let mut total_traffic_bytes = 0u64;
        let mut quota_blocked_count = 0usize;

        for rule in rules {
            let totals = meters
                .get(&rule.local_port)
                .map(|m| m.snapshot())
                .unwrap_or_default();
            total_traffic_bytes += totals.total_bytes();
            if !rule.enabled
                && rule.quota_bytes.is_some_and(|q| q > 0)
                && totals.total_bytes() as i64 >= rule.quota_bytes.unwrap_or(0)
            {
                quota_blocked_count += 1;
            }
        }

        crate::domain::DashboardStats {
            rule_count: rules.len(),
            active_count: rules.iter().filter(|r| r.enabled).count(),
            total_traffic_bytes,
            quota_blocked_count,
        }
    }

    /// 从数据库加载已有计数到内存。
    pub async fn hydrate_from_db(&self, rules: &[RuleRecord]) -> anyhow::Result<()> {
        for rule in rules {
            let row: Option<(i64, i64, i64, i64)> = sqlx::query_as(
                "SELECT tcp_rx, tcp_tx, udp_rx, udp_tx FROM traffic_counters WHERE rule_id = ?",
            )
            .bind(rule.id)
            .fetch_optional(&self.db)
            .await?;

            let meter = self.meter_for(rule).await;
            if let Some((tcp_rx, tcp_tx, udp_rx, udp_tx)) = row {
                meter.restore(&TrafficTotals {
                    tcp_rx: tcp_rx as u64,
                    tcp_tx: tcp_tx as u64,
                    udp_rx: udp_rx as u64,
                    udp_tx: udp_tx as u64,
                });
            }
        }
        Ok(())
    }

    /// 将所有内存计数批量写入数据库。
    pub async fn flush_to_db(&self) -> anyhow::Result<()> {
        let meters = self.meters.read().await;
        let port_map = self.port_to_rule.read().await;
        let now = Utc::now().to_rfc3339();

        let mut tx = self.db.begin().await?;
        for (port, meter) in meters.iter() {
            let Some(rule_id) = port_map.get(port) else {
                continue;
            };
            let s = meter.snapshot();
            sqlx::query(
                r#"
                INSERT INTO traffic_counters (rule_id, tcp_rx, tcp_tx, udp_rx, udp_tx, updated_at)
                VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(rule_id) DO UPDATE SET
                    tcp_rx = excluded.tcp_rx,
                    tcp_tx = excluded.tcp_tx,
                    udp_rx = excluded.udp_rx,
                    udp_tx = excluded.udp_tx,
                    updated_at = excluded.updated_at
                "#,
            )
            .bind(rule_id)
            .bind(s.tcp_rx as i64)
            .bind(s.tcp_tx as i64)
            .bind(s.udp_rx as i64)
            .bind(s.udp_tx as i64)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 返回所有规则的流量快照（供 API 使用）。
    pub async fn snapshots(&self, rules: &[RuleRecord]) -> Vec<TrafficSnapshot> {
        let meters = self.meters.read().await;
        rules
            .iter()
            .map(|rule| {
                let totals = meters
                    .get(&rule.local_port)
                    .map(|m| m.snapshot())
                    .unwrap_or_default();
                let used = totals.total_bytes() as f64;
                let quota_used_ratio = rule.quota_bytes.map(|q| {
                    if q <= 0 {
                        0.0
                    } else {
                        (used / q as f64).min(1.0)
                    }
                });
                TrafficSnapshot {
                    rule_id: rule.id,
                    local_port: rule.local_port,
                    totals,
                    quota_bytes: rule.quota_bytes,
                    quota_used_ratio,
                }
            })
            .collect()
    }

    pub fn is_quota_exceeded(rule: &RuleRecord, totals: &TrafficTotals) -> bool {
        rule.quota_bytes
            .is_some_and(|q| q > 0 && totals.total_bytes() as i64 >= q)
    }

    /// 后台定时落盘任务。
    pub fn spawn_flush_task(self: Arc<Self>, interval_secs: u64) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                ticker.tick().await;
                if let Err(e) = self.flush_to_db().await {
                    warn!("流量数据落盘失败: {e:#}");
                }
            }
        });
    }

    /// 配额超额即时停服：meter trip → 停 runtime → 写 DB。
    pub fn spawn_trip_handler(
        mut trip_rx: mpsc::UnboundedReceiver<u16>,
        state: crate::state::AppState,
    ) {
        tokio::spawn(async move {
            while let Some(port) = trip_rx.recv().await {
                if let Err(e) = handle_quota_trip(&state, port).await {
                    warn!(port, "配额停服失败: {e:#}");
                }
            }
        });
    }

    /// 配额周期重置 + DB/runtime 对账。
    pub fn spawn_maintenance(state: crate::state::AppState, interval_secs: u64) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                ticker.tick().await;
                if let Err(e) = reset_quota_periods(&state).await {
                    warn!("配额周期重置失败: {e:#}");
                }
                if let Err(e) = reconcile_runtime(&state).await {
                    warn!("转发对账失败: {e:#}");
                }
            }
        });
    }
}

async fn handle_quota_trip(state: &crate::state::AppState, port: u16) -> anyhow::Result<()> {
    let Some(rule) = state.rules.find_by_port(port).await? else {
        return Ok(());
    };
    if !rule.enabled {
        return Ok(());
    }

    let control = state.traffic.control_for(&rule).await;

    {
        let mut engine = state.engine.lock().await;
        engine.stop_rule(port, &control).await?;
    }

    state.rules.set_enabled(port, false).await?;
    warn!(port, "流量已达配额，规则已停服");
    Ok(())
}

async fn reset_quota_periods(state: &crate::state::AppState) -> anyhow::Result<()> {
    use crate::domain::QuotaPeriod;
    use chrono::Utc;

    let rules = state.rules.list().await?;
    let meters = state.traffic.all_meters().await;
    let now = Utc::now();

    for rule in rules {
        if !matches!(rule.quota_period, QuotaPeriod::Daily | QuotaPeriod::Monthly) {
            continue;
        }
        let Some(current_start) = rule.quota_period.current_period_start(now) else {
            continue;
        };
        let stored_start = rule.period_start.unwrap_or(current_start);
        if stored_start >= current_start {
            continue;
        }

        let totals = meters
            .get(&rule.local_port)
            .map(|m| m.snapshot())
            .unwrap_or_default();
        let was_quota_blocked = !rule.enabled && TrafficService::is_quota_exceeded(&rule, &totals);

        state.rules.reset_traffic(rule.local_port).await?;
        state.traffic.reset_meter(rule.local_port).await;
        state
            .rules
            .set_period_start(rule.local_port, current_start)
            .await?;

        if was_quota_blocked {
            let updated = state.rules.set_enabled(rule.local_port, true).await?;
            apply_rule_runtime(state, &updated).await?;
            warn!(port = rule.local_port, "配额周期已重置，规则已重新启用");
        }
    }
    Ok(())
}

/// DB 已停用但 engine 仍在跑 → 强制 stop（修复停服失败后的漂移）。
async fn reconcile_runtime(state: &crate::state::AppState) -> anyhow::Result<()> {
    let rules = state.rules.list().await?;
    let controls = state.traffic.all_controls().await;
    let mut engine = state.engine.lock().await;

    for rule in &rules {
        if rule.enabled || !engine.is_running(rule.local_port) {
            continue;
        }
        if let Some(control) = controls.get(&rule.local_port) {
            warn!(port = rule.local_port, "DB/runtime 不一致，强制停止转发");
            engine.stop_rule(rule.local_port, control).await?;
        }
    }
    Ok(())
}

/// 根据规则状态同步转发引擎：先停 runtime，再按需启动。
pub async fn apply_rule_runtime(
    state: &crate::state::AppState,
    rule: &RuleRecord,
) -> anyhow::Result<()> {
    let meter = state.traffic.meter_for(rule).await;
    let control = state.traffic.control_for(rule).await;
    control.set_quota(rule.quota_bytes.unwrap_or(0));

    let mut engine = state.engine.lock().await;
    engine.stop_rule(rule.local_port, &control).await?;
    if rule.enabled {
        engine.start_rule(rule, meter, control).await?;
    }
    Ok(())
}

/// 停止转发（手动停用 / 删除 / 配额停服共用）。
pub async fn stop_rule_runtime(state: &crate::state::AppState, port: u16) -> anyhow::Result<()> {
    let control = if let Some(rule) = state.rules.find_by_port(port).await? {
        state.traffic.control_for(&rule).await
    } else if let Some(control) = state.traffic.all_controls().await.get(&port).cloned() {
        control
    } else {
        return Ok(());
    };

    let mut engine = state.engine.lock().await;
    engine.stop_rule(port, &control).await
}
