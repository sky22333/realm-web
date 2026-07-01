//! 在本地连接侧统计字节（端口级 rx/tx）。

use std::io::Result;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::TrafficMeter;

/// 包装本地 TCP 连接：read → tcp_rx，write → tcp_tx。
pub struct CountedTcpStream {
    inner: tokio::net::TcpStream,
    meter: Arc<TrafficMeter>,
}

impl CountedTcpStream {
    pub fn new(inner: tokio::net::TcpStream, meter: Arc<TrafficMeter>) -> Self {
        Self { inner, meter }
    }

    /// zero-copy 路径不经 AsyncRead/AsyncWrite，需手动记账。
    #[cfg(target_os = "linux")]
    fn record_transferred(&self, local_to_remote: u64, remote_to_local: u64) {
        if local_to_remote > 0 {
            self.meter.add_tcp_rx(local_to_remote);
        }
        if remote_to_local > 0 {
            self.meter.add_tcp_tx(remote_to_local);
        }
    }
}

impl AsyncRead for CountedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let filled_before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let n = buf.filled().len() - filled_before;
            if n > 0 {
                self.meter.add_tcp_rx(n as u64);
            }
        }
        poll
    }
}

impl AsyncWrite for CountedTcpStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    self.meter.add_tcp_tx(n as u64);
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(target_os = "linux")]
mod linux_raw {
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::task::{Context, Poll};

    use realm_core::realm_io::AsyncRawIO;
    use tokio::io::Interest;

    use super::CountedTcpStream;

    impl AsRawFd for CountedTcpStream {
        fn as_raw_fd(&self) -> RawFd {
            self.inner.as_raw_fd()
        }
    }

    impl AsyncRawIO for CountedTcpStream {
        fn x_poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.inner.poll_read_ready(cx)
        }

        fn x_poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
            self.inner.poll_write_ready(cx)
        }

        fn x_try_io<R>(
            &self,
            interest: Interest,
            f: impl FnOnce() -> Result<R>,
        ) -> Result<R> {
            self.inner.try_io(interest, f)
        }
    }
}

/// realm_io 双向转发（Linux 优先 zero-copy）。
pub async fn bidi_relay(
    local: &mut CountedTcpStream,
    remote: &mut tokio::net::TcpStream,
) -> Result<()> {
    use realm_core::realm_io;

    #[cfg(target_os = "linux")]
    {
        match realm_io::bidi_zero_copy(local, remote).await {
            Ok((local_to_remote, remote_to_local)) => {
                local.record_transferred(local_to_remote, remote_to_local);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                realm_io::bidi_copy(local, remote).await.map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        realm_io::bidi_copy(local, remote).await.map(|_| ())
    }
}
