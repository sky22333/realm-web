//! Forwarding rule domain model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Traffic quota reset period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaPeriod {
    None,
    Daily,
    Monthly,
    Total,
}

impl QuotaPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Monthly => "monthly",
            Self::Total => "total",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "daily" => Self::Daily,
            "monthly" => Self::Monthly,
            "total" => Self::Total,
            _ => Self::None,
        }
    }

    /// 当前配额周期的起始时间（UTC）。
    pub fn current_period_start(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        use chrono::{Datelike, TimeZone};
        match self {
            Self::Daily => {
                let naive = now.date_naive().and_hms_opt(0, 0, 0)?;
                Some(Utc.from_utc_datetime(&naive))
            }
            Self::Monthly => {
                let naive = chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), 1)?
                    .and_hms_opt(0, 0, 0)?;
                Some(Utc.from_utc_datetime(&naive))
            }
            _ => None,
        }
    }
}

/// Port assignment strategy for batch creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PortAssignMode {
    Auto,
    FromStart { start_port: u16 },
    Manual { ports: Vec<u16> },
}

/// Rule as stored in SQLite and exposed via API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleRecord {
    pub id: i64,
    pub local_port: u16,
    pub listen_host: String,
    pub target_host: String,
    pub target_port: u16,
    pub enabled: bool,
    pub quota_bytes: Option<i64>,
    pub quota_period: QuotaPeriod,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_start: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Target endpoint parsed from `host:port` lines.
#[derive(Debug, Clone)]
pub struct ForwardRule {
    pub target_host: String,
    pub target_port: u16,
}

impl ForwardRule {
    pub fn parse(line: &str) -> anyhow::Result<Self> {
        let line = line.trim();
        let (host, port) = line
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("无效格式，需要 主机:端口"))?;
        let target_port: u16 = port
            .parse()
            .map_err(|_| anyhow::anyhow!("无效端口: {port}"))?;
        if target_port == 0 {
            anyhow::bail!("端口不能为 0");
        }
        Ok(Self {
            target_host: host.to_string(),
            target_port,
        })
    }
}
