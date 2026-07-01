//! 端口占用检测。

use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};

/// 检测本地端口是否可用于监听。
pub fn is_port_available(port: u16, used_ports: &[u16]) -> bool {
    if port == 0 {
        return false;
    }
    if used_ports.contains(&port) {
        return false;
    }

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    StdTcpListener::bind(addr).is_ok()
}

/// 从起始端口起查找下一个可用端口。
pub fn find_next_port(start: u16, used_ports: &[u16]) -> anyhow::Result<u16> {
    let mut port = start;
    loop {
        if is_port_available(port, used_ports) {
            return Ok(port);
        }
        if port == u16::MAX {
            anyhow::bail!("没有可用端口");
        }
        port += 1;
    }
}
