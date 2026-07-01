//! realm-web 入口：启动 HTTP 服务、数据库、转发引擎。

mod auth;
mod config;
mod db;
mod domain;
mod embed;
mod engine;
mod relay;
mod routes;
mod services;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::net::TcpListener;
use tracing::info;

use crate::config::AppConfig;
use crate::engine::ForwardEngine;
use crate::services::TrafficService;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "realm_web=info,tower_http=info,realm_core=warn".into()),
        )
        .init();
    tracing_log::LogTracer::init().ok();

    realm_core::dns::build(None, None);
    realm_core::dns::force_init();

    let config = Arc::new(AppConfig::from_env()?);
    let db = connect_db(&config).await?;
    db::init_schema(&db).await?;

    info!(
        port = config.panel_port,
        data_dir = %config.data_dir.display(),
        "正在启动 realm-web"
    );

    let (traffic, trip_rx) = TrafficService::new(db.clone());
    let engine = ForwardEngine::new();
    let state = AppState::new(config.clone(), db, traffic.clone(), engine);

    bootstrap_relays(&state).await?;
    traffic.clone().spawn_flush_task(60);
    TrafficService::spawn_trip_handler(trip_rx, state.clone());
    TrafficService::spawn_maintenance(state.clone(), 30);

    let listener = TcpListener::bind(("0.0.0.0", config.panel_port)).await?;
    info!(port = config.panel_port, "管理面板已就绪");

    axum::serve(
        listener,
        routes::router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

async fn connect_db(config: &AppConfig) -> anyhow::Result<sqlx::SqlitePool> {
    let db_path = config.data_dir.join("realm-web.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA busy_timeout = 5000")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await?;
    Ok(pool)
}

async fn bootstrap_relays(state: &AppState) -> anyhow::Result<()> {
    let mut rules = state.rules.list().await?;
    traffic::hydrate_meters(state, &rules).await?;

    let meters = state.traffic.all_meters().await;
    for rule in &rules {
        if !rule.enabled {
            continue;
        }
        let totals = meters
            .get(&rule.local_port)
            .map(|m| m.snapshot())
            .unwrap_or_default();
        if TrafficService::is_quota_exceeded(rule, &totals) {
            state.rules.set_enabled(rule.local_port, false).await?;
        }
    }

    rules = state.rules.list().await?;
    let meters = state.traffic.all_meters().await;
    let controls = state.traffic.all_controls().await;
    let mut engine = state.engine.lock().await;
    engine.sync_rules(&rules, &meters, &controls).await?;
    info!(count = rules.len(), "已恢复转发规则");
    Ok(())
}

mod traffic {
    use super::*;

    pub async fn hydrate_meters(
        state: &AppState,
        rules: &[domain::RuleRecord],
    ) -> anyhow::Result<()> {
        for rule in rules {
            state.traffic.meter_for(rule).await;
        }
        state.traffic.hydrate_from_db(rules).await
    }
}
