//! 基于 realm_core DNS + realm_io 的 TCP 转发（端口级流量统计）。

mod socket;

use std::sync::Arc;

use realm_core::endpoint::Endpoint;
use tracing::warn;

use super::TrafficMeter;
use super::counted::{CountedTcpStream, bidi_relay};

pub async fn run_tcp(endpoint: Endpoint, meter: Arc<TrafficMeter>) {
    let Endpoint {
        laddr,
        raddr,
        bind_opts,
        conn_opts,
        ..
    } = endpoint;

    let listener = match socket::bind(&laddr, bind_opts) {
        Ok(l) => l,
        Err(e) => {
            warn!(%laddr, "TCP 绑定失败: {e}");
            return;
        }
    };

    loop {
        let Ok((inbound, peer)) = listener.accept().await else {
            continue;
        };
        let _ = inbound.set_nodelay(true);
        socket::apply_accept_keepalive(&inbound, &conn_opts);

        let raddr = raddr.clone();
        let conn_opts = conn_opts.clone();
        let meter = meter.clone();

        tokio::spawn(async move {
            let remote = match socket::connect(&raddr, &conn_opts).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%peer, %raddr, "TCP 连接目标失败: {e}");
                    return;
                }
            };

            let mut local = CountedTcpStream::new(inbound, meter);
            let mut remote = remote;
            if let Err(e) = bidi_relay(&mut local, &mut remote).await {
                tracing::debug!(%peer, "TCP 转发结束: {e}");
            }
        });
    }
}
