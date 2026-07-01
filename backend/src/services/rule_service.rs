//! 规则持久化与批量操作。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::config::AppConfig;
use crate::domain::{ForwardRule, PortAssignMode, QuotaPeriod, RuleRecord};
use crate::services::port::{find_next_port, is_port_available};

/// 规则数据库访问层。
#[derive(Clone)]
pub struct RuleService {
    db: SqlitePool,
    config: Arc<AppConfig>,
}

impl RuleService {
    pub fn new(db: SqlitePool, config: Arc<AppConfig>) -> Self {
        Self { db, config }
    }

    pub async fn list(&self) -> anyhow::Result<Vec<RuleRecord>> {
        let rows = sqlx::query_as::<_, RuleRow>(
            r#"
            SELECT id, local_port, listen_host, target_host, target_port,
                   enabled, quota_bytes, quota_period, period_start, created_at, updated_at
            FROM rules
            ORDER BY local_port DESC
            "#,
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(RuleRecord::from).collect())
    }

    pub async fn find_by_port(&self, local_port: u16) -> anyhow::Result<Option<RuleRecord>> {
        let row = sqlx::query_as::<_, RuleRow>(
            r#"
            SELECT id, local_port, listen_host, target_host, target_port,
                   enabled, quota_bytes, quota_period, period_start, created_at, updated_at
            FROM rules WHERE local_port = ?
            "#,
        )
        .bind(local_port)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(RuleRecord::from))
    }

    pub async fn used_ports(&self) -> anyhow::Result<Vec<u16>> {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT local_port FROM rules")
            .fetch_all(&self.db)
            .await?;
        Ok(rows.into_iter().map(|(p,)| p as u16).collect())
    }

    /// 批量添加转发规则（单事务）。
    pub async fn add_batch(
        &self,
        targets: Vec<ForwardRule>,
        mode: PortAssignMode,
        quota_bytes: Option<i64>,
        quota_period: QuotaPeriod,
    ) -> anyhow::Result<Vec<RuleRecord>> {
        if targets.is_empty() {
            anyhow::bail!("请至少输入一条转发目标");
        }

        let mut used = self.used_ports().await?;
        let listen_host = self.config.listen_host.to_string();
        let local_ports = self.assign_ports(targets.len(), &mode, &mut used)?;

        let now = Utc::now().to_rfc3339();
        let mut tx = self.db.begin().await?;
        let mut created = Vec::with_capacity(targets.len());

        let quota_period_str = quota_period.as_str();
        let period_start = quota_bytes
            .is_some_and(|q| q > 0)
            .then(|| quota_period.current_period_start(Utc::now()))
            .flatten()
            .map(|t| t.to_rfc3339());

        for (target, local_port) in targets.into_iter().zip(local_ports) {
            let result = sqlx::query(
                r#"
                INSERT INTO rules (
                    local_port, listen_host, target_host, target_port,
                    enabled, quota_bytes, quota_period, period_start, created_at, updated_at
                ) VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(local_port)
            .bind(&listen_host)
            .bind(&target.target_host)
            .bind(target.target_port)
            .bind(quota_bytes)
            .bind(quota_period_str)
            .bind(&period_start)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            let rule_id = result.last_insert_rowid();
            sqlx::query(
                "INSERT INTO traffic_counters (rule_id, tcp_rx, tcp_tx, udp_rx, udp_tx, updated_at) VALUES (?, 0, 0, 0, 0, ?)",
            )
            .bind(rule_id)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            created.push(RuleRecord {
                id: rule_id,
                local_port,
                listen_host: listen_host.clone(),
                target_host: target.target_host,
                target_port: target.target_port,
                enabled: true,
                quota_bytes,
                quota_period,
                period_start: period_start
                    .as_ref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
                created_at: DateTime::parse_from_rfc3339(&now)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                updated_at: DateTime::parse_from_rfc3339(&now)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            });
        }

        tx.commit().await?;
        Ok(created)
    }

    pub async fn delete_by_port(&self, local_port: u16) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM rules WHERE local_port = ?")
            .bind(local_port)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_batch(&self, ports: &[u16]) -> anyhow::Result<(Vec<u16>, Vec<u16>)> {
        let mut deleted = Vec::new();
        let mut failed = Vec::new();
        let mut tx = self.db.begin().await?;
        for port in ports {
            let result = sqlx::query("DELETE FROM rules WHERE local_port = ?")
                .bind(port)
                .execute(&mut *tx)
                .await?;
            if result.rows_affected() > 0 {
                deleted.push(*port);
            } else {
                failed.push(*port);
            }
        }
        tx.commit().await?;
        Ok((deleted, failed))
    }

    /// 更新规则字段。
    pub async fn update(
        &self,
        local_port: u16,
        req: &crate::domain::UpdateRuleRequest,
    ) -> anyhow::Result<RuleRecord> {
        let existing = self
            .find_by_port(local_port)
            .await?
            .ok_or_else(|| anyhow::anyhow!("规则不存在"))?;

        let target_host = req.target_host.as_ref().unwrap_or(&existing.target_host);
        let target_port = req.target_port.unwrap_or(existing.target_port);
        let enabled = req.enabled.unwrap_or(existing.enabled);
        let quota_bytes = req.resolve_quota_bytes(existing.quota_bytes);
        let quota_period = req.parse_quota_period().unwrap_or(existing.quota_period);
        let now = Utc::now().to_rfc3339();

        let period_start = if quota_bytes.is_some_and(|q| q > 0)
            && matches!(quota_period, QuotaPeriod::Daily | QuotaPeriod::Monthly)
        {
            if req.quota_gb.is_some()
                || req.unset_quota == Some(true)
                || req.quota_period.is_some()
                || existing.period_start.is_none()
            {
                quota_period
                    .current_period_start(Utc::now())
                    .map(|t| t.to_rfc3339())
            } else {
                existing.period_start.map(|t| t.to_rfc3339())
            }
        } else {
            None
        };

        sqlx::query(
            r#"
            UPDATE rules SET
                target_host = ?, target_port = ?, enabled = ?,
                quota_bytes = ?, quota_period = ?, period_start = ?, updated_at = ?
            WHERE local_port = ?
            "#,
        )
        .bind(target_host)
        .bind(target_port)
        .bind(enabled)
        .bind(quota_bytes)
        .bind(quota_period.as_str())
        .bind(&period_start)
        .bind(&now)
        .bind(local_port)
        .execute(&self.db)
        .await?;

        self.find_by_port(local_port)
            .await?
            .ok_or_else(|| anyhow::anyhow!("更新后读取规则失败"))
    }

    /// 仅切换启用状态。
    pub async fn set_enabled(&self, local_port: u16, enabled: bool) -> anyhow::Result<RuleRecord> {
        let now = Utc::now().to_rfc3339();
        let result =
            sqlx::query("UPDATE rules SET enabled = ?, updated_at = ? WHERE local_port = ?")
                .bind(enabled)
                .bind(&now)
                .bind(local_port)
                .execute(&self.db)
                .await?;
        if result.rows_affected() == 0 {
            anyhow::bail!("规则不存在");
        }
        self.find_by_port(local_port)
            .await?
            .ok_or_else(|| anyhow::anyhow!("读取规则失败"))
    }

    /// 更新配额周期起始时间。
    pub async fn set_period_start(
        &self,
        local_port: u16,
        period_start: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let ps = period_start.to_rfc3339();
        sqlx::query("UPDATE rules SET period_start = ?, updated_at = ? WHERE local_port = ?")
            .bind(&ps)
            .bind(&now)
            .bind(local_port)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// 重置流量计数。
    pub async fn reset_traffic(&self, local_port: u16) -> anyhow::Result<()> {
        let rule = self
            .find_by_port(local_port)
            .await?
            .ok_or_else(|| anyhow::anyhow!("规则不存在"))?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE traffic_counters SET tcp_rx=0, tcp_tx=0, udp_rx=0, udp_tx=0, updated_at=? WHERE rule_id=?",
        )
        .bind(&now)
        .bind(rule.id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    fn assign_ports(
        &self,
        count: usize,
        mode: &PortAssignMode,
        used: &mut Vec<u16>,
    ) -> anyhow::Result<Vec<u16>> {
        match mode {
            PortAssignMode::Auto => {
                let mut ports = Vec::with_capacity(count);
                let mut cursor = self.config.default_start_port;
                for _ in 0..count {
                    let port = find_next_port(cursor, used)?;
                    ports.push(port);
                    used.push(port);
                    cursor = port.saturating_add(1);
                }
                Ok(ports)
            }
            PortAssignMode::FromStart { start_port } => {
                if *start_port == 0 {
                    anyhow::bail!("起始端口无效");
                }
                let mut ports = Vec::with_capacity(count);
                let mut cursor = *start_port;
                for _ in 0..count {
                    let port = find_next_port(cursor, used)?;
                    ports.push(port);
                    used.push(port);
                    cursor = port.saturating_add(1);
                }
                Ok(ports)
            }
            PortAssignMode::Manual { ports } => {
                if ports.len() != count {
                    anyhow::bail!("手动端口数量与目标数量不一致");
                }
                for port in ports {
                    if *port == 0 {
                        anyhow::bail!("端口 {port} 无效");
                    }
                    if !is_port_available(*port, used) {
                        anyhow::bail!("端口 {port} 不可用");
                    }
                }
                used.extend(ports);
                Ok(ports.clone())
            }
        }
    }
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    id: i64,
    local_port: i64,
    listen_host: String,
    target_host: String,
    target_port: i64,
    enabled: i64,
    quota_bytes: Option<i64>,
    quota_period: String,
    period_start: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<RuleRow> for RuleRecord {
    fn from(row: RuleRow) -> Self {
        Self {
            id: row.id,
            local_port: row.local_port as u16,
            listen_host: row.listen_host,
            target_host: row.target_host,
            target_port: row.target_port as u16,
            enabled: row.enabled != 0,
            quota_bytes: row.quota_bytes,
            quota_period: QuotaPeriod::parse(&row.quota_period),
            period_start: row
                .period_start
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc)),
            created_at: row.created_at.parse().unwrap_or_else(|_| Utc::now()),
            updated_at: row.updated_at.parse().unwrap_or_else(|_| Utc::now()),
        }
    }
}
