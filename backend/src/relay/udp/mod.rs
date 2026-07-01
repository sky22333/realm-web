//! Realm-style batched UDP relay with per-port traffic metering.

mod batch;
mod sockmap;

use std::io::{ErrorKind, Result};
use std::net::SocketAddr;
use std::sync::Arc;

use realm_core::dns;
use realm_core::endpoint::{BindOpts, ConnectOpts, Endpoint, RemoteAddr};
use realm_core::realm_syscall::new_udp_socket;
use realm_core::time::timeoutfut;
use tokio::net::UdpSocket;
use tracing::{debug, warn};

use super::TrafficMeter;
use batch::{recoverable, Batch};
use sockmap::SockMap;

pub async fn run_udp(endpoint: Endpoint, meter: Arc<TrafficMeter>) {
    let Endpoint {
        laddr,
        raddr,
        bind_opts,
        conn_opts,
        ..
    } = endpoint;

    let listen = match bind(&laddr, bind_opts) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(%laddr, "UDP 绑定失败: {e}");
            return;
        }
    };

    let sockmap = Arc::new(SockMap::new());
    let mut batch = Batch::new();

    loop {
        if let Err(e) = relay_batch(
            &listen,
            &raddr,
            &conn_opts,
            &sockmap,
            &mut batch,
            &meter,
        )
        .await
        {
            if recoverable(e.kind()) {
                debug!("UDP 可恢复错误: {e}");
                continue;
            }
            warn!("UDP 转发错误: {e}");
        }
    }
}

async fn relay_batch(
    listen: &Arc<UdpSocket>,
    rname: &RemoteAddr,
    conn_opts: &ConnectOpts,
    sockmap: &Arc<SockMap>,
    batch: &mut Batch,
    meter: &Arc<TrafficMeter>,
) -> Result<()> {
    batch.recv_on(listen).await?;
    if batch.count() == 0 {
        return Ok(());
    }

    for bytes in batch.rx_bytes() {
        meter.add_udp_rx(bytes);
    }

    let remote = resolve_first(rname).await?;
    batch.group_by_peer();

    for pkts in batch.peer_groups() {
        let client = pkts[0].peer;
        let downstream = sockmap.find_or_insert(&client, || -> Result<Arc<UdpSocket>> {
            let sock = Arc::new(associate(&remote, conn_opts)?);
            tokio::spawn(send_back(
                Arc::clone(listen),
                client,
                Arc::clone(&sock),
                conn_opts.associate_timeout,
                Arc::clone(sockmap),
                Arc::clone(meter),
            ));
            Ok(sock)
        })?;

        Batch::send_to_remote(&downstream, pkts, remote).await?;
    }

    Ok(())
}

async fn send_back(
    listen: Arc<UdpSocket>,
    client: SocketAddr,
    downstream: Arc<UdpSocket>,
    associate_timeout: usize,
    sockmap: Arc<SockMap>,
    meter: Arc<TrafficMeter>,
) {
    let mut batch = Batch::new();
    loop {
        let recv = batch.recv_on(&downstream);
        match timeoutfut(recv, associate_timeout).await {
            Ok(Ok(())) if batch.count() > 0 => {
                for bytes in batch.rx_bytes() {
                    meter.add_udp_tx(bytes);
                }
                if batch.send_all_to(&listen, client).await.is_err() {
                    break;
                }
            }
            Ok(Ok(())) => {}
            Ok(Err(e)) if recoverable(e.kind()) => {}
            _ => break,
        }
    }
    sockmap.remove(&client);
}

async fn resolve_first(raddr: &RemoteAddr) -> Result<SocketAddr> {
    dns::resolve_addr(raddr)
        .await?
        .iter()
        .next()
        .ok_or(ErrorKind::NotFound.into())
}

fn bind(laddr: &SocketAddr, opts: BindOpts) -> Result<UdpSocket> {
    let BindOpts {
        ipv6_only,
        #[cfg(target_os = "linux")]
        bind_interface,
        ..
    } = opts;
    let socket = new_udp_socket(laddr)?;

    if let SocketAddr::V6(_) = laddr {
        socket.set_only_v6(ipv6_only)?;
    }

    #[cfg(target_os = "linux")]
    if let Some(iface) = bind_interface {
        realm_core::realm_syscall::bind_to_device(&socket, &iface)?;
    }

    let _ = socket.set_reuse_address(true);
    harden_udp(&socket)?;
    socket.bind(&(*laddr).into())?;
    UdpSocket::from_std(socket.into())
}

fn associate(raddr: &SocketAddr, opts: &ConnectOpts) -> Result<UdpSocket> {
    let ConnectOpts {
        bind_address,
        #[cfg(target_os = "linux")]
        bind_interface,
        ..
    } = opts;

    let socket = new_udp_socket(raddr)?;
    let _ = socket.set_reuse_address(true);

    if let Some(addr) = *bind_address {
        socket.bind(&addr.into())?;
    }

    #[cfg(target_os = "linux")]
    if let Some(iface) = bind_interface {
        realm_core::realm_syscall::bind_to_device(&socket, iface)?;
    }

    harden_udp(&socket)?;
    UdpSocket::from_std(socket.into())
}

/// Windows: ignore ICMP port-unreachable as a fatal recv error (WSASendMsg path).
#[cfg(windows)]
fn harden_udp(socket: &socket2::Socket) -> Result<()> {
    use std::os::windows::io::AsRawSocket;
    use std::mem::size_of;
    use windows_sys::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET, SOCKET_ERROR, WSAIoctl};

    let disabled: u32 = 0;
    let mut out_bytes = 0u32;
    let status = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as SOCKET,
            SIO_UDP_CONNRESET,
            &disabled as *const _ as *mut _,
            size_of::<u32>() as u32,
            std::ptr::null_mut(),
            0,
            &mut out_bytes,
            std::ptr::null_mut(),
            None,
        )
    };
    if status == SOCKET_ERROR {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn harden_udp(_socket: &socket2::Socket) -> Result<()> {
    Ok(())
}
