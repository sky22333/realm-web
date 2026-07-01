//! 流量统计与配额检查。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::warn;

use crate::domain::{RuleRecord, TrafficSnapshot, TrafficTotals};
use crate::relay::TrafficMeter;

/// 内存计数器 + 数据库持久化。
pub struct TrafficService {
    db: SqlitePool,
    /// 按本地端口索引的实时计数器。
    meters: RwLock<HashMap<u16, Arc<TrafficMeter>>>,
    /// 端口 → 规则 ID。
    port_to_rule: RwLock<HashMap<u16, i64>>,
}

impl TrafficService {
    pub fn new(db: SqlitePool) -> Self {
        Self {
            db,
            meters: RwLock::new(HashMap::new()),
            port_to_rule: RwLock::new(HashMap::new()),
        }
    }

    /// 为规则注册或获取计数器。
    pub async fn meter_for(&self, rule: &RuleRecord) -> Arc<TrafficMeter> {
        let mut map = self.meters.write().await;
        let meter = map
            .entry(rule.local_port)
            .or_insert_with(|| Arc::new(TrafficMeter::default()))
            .clone();

        self.port_to_rule
            .write()
            .await
            .insert(rule.local_port, rule.id);
        meter
    }

    pub async fn all_meters(&self) -> HashMap<u16, Arc<TrafficMeter>> {
        self.meters.read().await.clone()
    }

    pub async fn remove_port(&self, local_port: u16) {
        self.meters.write().await.remove(&local_port);
        self.port_to_rule.write().await.remove(&local_port);
    }

    /// 重置指定端口的内存计数。
    pub async fn reset_meter(&self, local_port: u16) {
        if let Some(meter) = self.meters.read().await.get(&local_port) {
            meter.tcp_rx.store(0, std::sync::atomic::Ordering::Relaxed);
            meter.tcp_tx.store(0, std::sync::atomic::Ordering::Relaxed);
            meter.udp_rx.store(0, std::sync::atomic::Ordering::Relaxed);
            meter.udp_tx.store(0, std::sync::atomic::Ordering::Relaxed);
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
                meter
                    .tcp_rx
                    .store(tcp_rx as u64, std::sync::atomic::Ordering::Relaxed);
                meter
                    .tcp_tx
                    .store(tcp_tx as u64, std::sync::atomic::Ordering::Relaxed);
                meter
                    .udp_rx
                    .store(udp_rx as u64, std::sync::atomic::Ordering::Relaxed);
                meter
                    .udp_tx
                    .store(udp_tx as u64, std::sync::atomic::Ordering::Relaxed);
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

    /// 检查是否超出配额。
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

    /// 检查流量配额，超额则自动停用规则。
    pub fn spawn_quota_enforcement(state: crate::state::AppState, interval_secs: u64) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                ticker.tick().await;
                if let Err(e) = reset_quota_periods(&state).await {
                    warn!("配额周期重置失败: {e:#}");
                }
                if let Err(e) = enforce_quotas(&state).await {
                    warn!("配额检查失败: {e:#}");
                }
            }
        });
    }
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

async fn enforce_quotas(state: &crate::state::AppState) -> anyhow::Result<()> {
    let rules = state.rules.list().await?;
    let meters = state.traffic.all_meters().await;

    for rule in &rules {
        if !rule.enabled {
            continue;
        }
        let Some(quota) = rule.quota_bytes else {
            continue;
        };
        if quota <= 0 {
            continue;
        }
        let totals = meters
            .get(&rule.local_port)
            .map(|m| m.snapshot())
            .unwrap_or_default();
        if !TrafficService::is_quota_exceeded(rule, &totals) {
            continue;
        }

        warn!(port = rule.local_port, "流量已达配额，自动停用");
        let updated = state.rules.set_enabled(rule.local_port, false).await?;
        apply_rule_runtime(state, &updated).await?;
    }
    Ok(())
}

/// 根据规则状态同步转发引擎。
pub async fn apply_rule_runtime(
    state: &crate::state::AppState,
    rule: &RuleRecord,
) -> anyhow::Result<()> {
    let meter = state.traffic.meter_for(rule).await;
    let mut engine = state.engine.lock().await;
    engine.stop_rule(rule.local_port).await?;
    if rule.enabled {
        engine.start_rule(rule, meter).await?;
    }
    Ok(())
}
