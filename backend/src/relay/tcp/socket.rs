//! TCP socket bind/connect aligned with realm_core v2.9.4.

use std::io::{Error, ErrorKind, Result};
use std::net::SocketAddr;
use std::time::Duration;

use realm_core::dns;
use realm_core::endpoint::{BindOpts, ConnectOpts, RemoteAddr};
use realm_core::realm_syscall::new_tcp_socket;
use realm_core::realm_syscall::socket2::{SockRef, Socket, TcpKeepalive};
use realm_core::time::timeoutfut;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

#[cfg(target_os = "linux")]
use realm_core::realm_syscall::new_mptcp_socket;

fn new_socket(addr: &SocketAddr, mptcp: bool) -> Result<Socket> {
    #[cfg(target_os = "linux")]
    {
        let call = if mptcp {
            new_mptcp_socket
        } else {
            new_tcp_socket
        };
        call(addr)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = mptcp;
        new_tcp_socket(addr)
    }
}

pub fn bind(laddr: &SocketAddr, bind_opts: BindOpts) -> Result<TcpListener> {
    let BindOpts {
        accept_mptcp,
        ipv6_only,
        #[cfg(target_os = "linux")]
        bind_interface,
        ..
    } = bind_opts;

    let socket = new_socket(laddr, accept_mptcp)?;

    if let SocketAddr::V6(_) = laddr {
        socket.set_only_v6(ipv6_only)?;
    }

    #[cfg(target_os = "linux")]
    if let Some(iface) = bind_interface {
        realm_core::realm_syscall::bind_to_device(&socket, &iface)?;
    }

    let _ = socket.set_reuse_address(true);
    socket.bind(&(*laddr).into())?;
    socket.listen(1024)?;

    TcpListener::from_std(socket.into())
}

pub async fn connect(raddr: &RemoteAddr, conn_opts: &ConnectOpts) -> Result<TcpStream> {
    let ConnectOpts {
        send_mptcp,
        connect_timeout,
        bind_address,
        #[cfg(target_os = "linux")]
        bind_interface,
        ..
    } = conn_opts;

    let mut last_err = None;
    let keepalive = build_keepalive(conn_opts);

    for addr in dns::resolve_addr(raddr).await?.iter() {
        let socket = new_socket(&addr, *send_mptcp)?;
        let _ = socket.set_tcp_nodelay(true);
        let _ = socket.set_reuse_address(true);

        if let Some(addr) = *bind_address {
            socket.bind(&addr.into())?;
        }

        #[cfg(target_os = "linux")]
        if let Some(iface) = bind_interface {
            realm_core::realm_syscall::bind_to_device(&socket, iface)?;
        }

        if let Some(kpa) = &keepalive {
            socket.set_tcp_keepalive(kpa)?;
        }

        let socket = TcpSocket::from_std_stream(socket.into());

        match timeoutfut(socket.connect(addr), *connect_timeout).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last_err = Some(e),
            Err(_) => last_err = Some(ErrorKind::TimedOut.into()),
        }
    }

    Err(last_err
        .unwrap_or_else(|| Error::new(ErrorKind::NotConnected, "could not connect to any address")))
}

pub fn apply_accept_keepalive(stream: &TcpStream, conn_opts: &ConnectOpts) {
    let Some(kpa) = build_keepalive(conn_opts) else {
        return;
    };
    let _ = SockRef::from(stream).set_tcp_keepalive(&kpa);
}

fn build_keepalive(conn_opts: &ConnectOpts) -> Option<TcpKeepalive> {
    let ConnectOpts {
        tcp_keepalive,
        tcp_keepalive_probe,
        ..
    } = conn_opts;

    if *tcp_keepalive == 0 {
        return None;
    }

    let secs = Duration::from_secs(*tcp_keepalive as u64);
    let mut kpa = TcpKeepalive::new().with_time(secs);

    #[cfg(not(target_os = "openbsd"))]
    {
        kpa = TcpKeepalive::with_interval(kpa, secs);
        kpa = TcpKeepalive::with_retries(kpa, *tcp_keepalive_probe as u32);
    }

    Some(kpa)
}
