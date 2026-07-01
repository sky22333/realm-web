//! 转发规则、流量与统计 API。

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use crate::domain::{
    DashboardStats, ForwardRule, PortAssignMode, QuotaPeriod, RuleRecord, TrafficSnapshot,
    UpdateRuleRequest,
};
use crate::services::{apply_rule_runtime, stop_rule_runtime};
use crate::state::AppState;

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

#[derive(Serialize)]
pub struct RuleWithTraffic {
    pub rule: RuleRecord,
    pub traffic: TrafficSnapshot,
}

#[derive(Deserialize)]
pub struct AddRulesRequest {
    pub mode: String,
    pub targets: String,
    #[serde(default)]
    pub start_port: Option<u16>,
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
    #[serde(default)]
    pub quota_gb: Option<f64>,
    #[serde(default)]
    pub quota_period: Option<String>,
}

#[derive(Deserialize)]
pub struct BatchDeleteRequest {
    pub ports: Vec<u16>,
}

#[derive(Serialize)]
pub struct BatchDeleteResponse {
    pub deleted: Vec<u16>,
    pub failed: Vec<u16>,
}

fn gb_to_bytes(gb: f64) -> i64 {
    (gb * 1024.0 * 1024.0 * 1024.0) as i64
}

pub async fn dashboard_stats(State(state): State<AppState>) -> Json<DashboardStats> {
    let rules = state.rules.list().await.unwrap_or_default();
    Json(state.traffic.dashboard_stats(&rules).await)
}

pub async fn list_rules(State(state): State<AppState>) -> Json<Vec<RuleWithTraffic>> {
    let rules = state.rules.list().await.unwrap_or_default();
    let traffic = state.traffic.snapshots(&rules).await;
    let merged = rules
        .into_iter()
        .zip(traffic)
        .map(|(rule, traffic)| RuleWithTraffic { rule, traffic })
        .collect();
    Json(merged)
}

pub async fn list_traffic(State(state): State<AppState>) -> Json<Vec<TrafficSnapshot>> {
    let rules = state.rules.list().await.unwrap_or_default();
    Json(state.traffic.snapshots(&rules).await)
}

pub async fn used_ports(State(state): State<AppState>) -> Json<Vec<u16>> {
    Json(state.rules.used_ports().await.unwrap_or_default())
}

pub async fn add_rules(
    State(state): State<AppState>,
    Json(body): Json<AddRulesRequest>,
) -> (StatusCode, Json<ApiResponse<Vec<RuleRecord>>>) {
    let mut lines = Vec::new();
    for line in body.targets.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match ForwardRule::parse(line) {
            Ok(rule) => lines.push(rule),
            Err(e) => {
                return bad_request(e.to_string());
            }
        }
    }

    if lines.is_empty() {
        return bad_request("请至少输入一条转发目标".into());
    }

    let mode = match parse_port_mode(&body) {
        Ok(m) => m,
        Err(msg) => return bad_request(msg),
    };

    let quota_bytes = body.quota_gb.map(gb_to_bytes);
    let quota_period = body
        .quota_period
        .as_deref()
        .map(QuotaPeriod::parse)
        .unwrap_or(QuotaPeriod::Total);

    let created = match state
        .rules
        .add_batch(lines, mode, quota_bytes, quota_period)
        .await
    {
        Ok(v) => v,
        Err(e) => return bad_request(e.to_string()),
    };

    for rule in &created {
        let meter = state.traffic.meter_for(rule).await;
        let control = state.traffic.control_for(rule).await;
        if let Err(e) = {
            let mut engine = state.engine.lock().await;
            engine.start_rule(rule, meter, control).await
        } {
            rollback_created(&state, &created).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    message: Some(format!("启动转发失败，已回滚: {e}")),
                    data: None,
                }),
            );
        }
    }

    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: Some(format!("成功添加 {} 条规则", created.len())),
            data: Some(created),
        }),
    )
}

pub async fn update_rule(
    State(state): State<AppState>,
    Path(port): Path<u16>,
    Json(body): Json<UpdateRuleRequest>,
) -> (StatusCode, Json<ApiResponse<RuleRecord>>) {
    let updated = match state.rules.update(port, &body).await {
        Ok(r) => r,
        Err(e) => return bad_request(e.to_string()),
    };

    if let Err(e) = apply_rule_runtime(&state, &updated).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                message: Some(format!("更新转发失败: {e}")),
                data: None,
            }),
        );
    }

    ok(updated)
}

pub async fn toggle_rule(
    State(state): State<AppState>,
    Path(port): Path<u16>,
) -> (StatusCode, Json<ApiResponse<RuleRecord>>) {
    let current = match state.rules.find_by_port(port).await {
        Ok(Some(r)) => r,
        Ok(None) => return bad_request("规则不存在".into()),
        Err(e) => return bad_request(e.to_string()),
    };

    let new_enabled = !current.enabled;

    if !new_enabled {
        if let Err(e) = stop_rule_runtime(&state, port).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    success: false,
                    message: Some(format!("停止转发失败: {e}")),
                    data: None,
                }),
            );
        }
        let updated = match state.rules.set_enabled(port, false).await {
            Ok(r) => r,
            Err(e) => return bad_request(e.to_string()),
        };
        return ok(updated);
    }

    let updated = match state.rules.set_enabled(port, true).await {
        Ok(r) => r,
        Err(e) => return bad_request(e.to_string()),
    };

    if let Err(e) = apply_rule_runtime(&state, &updated).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                message: Some(format!("启动转发失败: {e}")),
                data: None,
            }),
        );
    }

    ok(updated)
}

pub async fn reset_traffic(
    State(state): State<AppState>,
    Path(port): Path<u16>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    if let Err(e) = state.rules.reset_traffic(port).await {
        return bad_request(e.to_string());
    }
    state.traffic.reset_meter(port).await;
    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: Some("流量已重置".into()),
            data: None,
        }),
    )
}

pub async fn delete_rule(
    State(state): State<AppState>,
    Path(port): Path<u16>,
) -> Json<ApiResponse<()>> {
    let _ = stop_rule_runtime(&state, port).await;
    state.traffic.remove_port(port).await;

    match state.rules.delete_by_port(port).await {
        Ok(true) => Json(ApiResponse {
            success: true,
            message: Some("删除成功".into()),
            data: None,
        }),
        Ok(false) => Json(ApiResponse {
            success: false,
            message: Some("规则不存在".into()),
            data: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            message: Some(e.to_string()),
            data: None,
        }),
    }
}

pub async fn delete_batch(
    State(state): State<AppState>,
    Json(body): Json<BatchDeleteRequest>,
) -> Json<ApiResponse<BatchDeleteResponse>> {
    for port in &body.ports {
        let _ = stop_rule_runtime(&state, *port).await;
        state.traffic.remove_port(*port).await;
    }

    match state.rules.delete_batch(&body.ports).await {
        Ok((deleted, failed)) => Json(ApiResponse {
            success: failed.is_empty(),
            message: None,
            data: Some(BatchDeleteResponse { deleted, failed }),
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            message: Some(e.to_string()),
            data: None,
        }),
    }
}

async fn rollback_created(state: &AppState, created: &[RuleRecord]) {
    for r in created {
        let _ = stop_rule_runtime(state, r.local_port).await;
        let _ = state.rules.delete_by_port(r.local_port).await;
        state.traffic.remove_port(r.local_port).await;
    }
}

fn parse_port_mode(body: &AddRulesRequest) -> Result<PortAssignMode, String> {
    match body.mode.as_str() {
        "auto" => Ok(PortAssignMode::Auto),
        "specific" => {
            let start = body
                .start_port
                .ok_or_else(|| "请指定起始端口".to_string())?;
            Ok(PortAssignMode::FromStart { start_port: start })
        }
        "manual" => {
            let ports = body
                .ports
                .clone()
                .ok_or_else(|| "请指定端口列表".to_string())?;
            Ok(PortAssignMode::Manual { ports })
        }
        _ => Err("无效的端口分配模式".into()),
    }
}

fn bad_request<T>(message: String) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse {
            success: false,
            message: Some(message),
            data: None,
        }),
    )
}

fn ok<T>(data: T) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::OK,
        Json(ApiResponse {
            success: true,
            message: Some("操作成功".into()),
            data: Some(data),
        }),
    )
}
