//! Batched UDP I/O (`recvmmsg`/`sendmmsg` on Linux, single-packet fallback elsewhere).

use std::io::{ErrorKind, Result};
use std::net::SocketAddr;
use std::ops::Range;

use tokio::net::UdpSocket;

pub const MAX_PACKETS: usize = 128;
pub const PACKET_SIZE: usize = 1500;

#[derive(Debug, Clone)]
pub struct Packet {
    pub buf: [u8; PACKET_SIZE],
    pub peer: SocketAddr,
    pub len: u16,
}

impl Packet {
    pub fn payload(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

#[derive(Debug, Default)]
pub struct Batch {
    pkts: Box<[Packet]>,
    groups: Vec<Range<u16>>,
    count: u16,
}

impl Batch {
    pub fn new() -> Self {
        Self {
            pkts: vec![
                Packet {
                    buf: [0; PACKET_SIZE],
                    peer: placeholder_addr(),
                    len: 0,
                };
                MAX_PACKETS
            ]
            .into_boxed_slice(),
            groups: Vec::with_capacity(MAX_PACKETS),
            count: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.count as usize
    }

    pub fn rx_bytes(&self) -> impl Iterator<Item = u64> + '_ {
        self.pkts[..self.count as usize]
            .iter()
            .map(|p| p.len as u64)
    }

    pub async fn recv_on(&mut self, sock: &UdpSocket) -> Result<()> {
        match recv_some(sock, &mut self.pkts).await {
            Ok(n) => self.count = n as u16,
            Err(e) if recoverable(e.kind()) => self.count = 0,
            Err(e) => return Err(e),
        }
        Ok(())
    }

    pub fn group_by_peer(&mut self) {
        let n = self.count as usize;
        self.groups.clear();
        group_by_peer(&mut self.pkts[..n], &mut self.groups, |a, b| {
            a.peer == b.peer
        });
    }

    pub fn peer_groups(&self) -> impl Iterator<Item = &[Packet]> + '_ {
        self.groups
            .iter()
            .map(move |r| &self.pkts[r.start as usize..r.end as usize])
    }

    pub async fn send_to_remote(
        sock: &UdpSocket,
        pkts: &[Packet],
        remote: SocketAddr,
    ) -> Result<()> {
        send_to_remote(sock, pkts, remote).await
    }

    pub async fn send_all_to(&self, sock: &UdpSocket, dest: SocketAddr) -> Result<()> {
        send_to_remote(sock, &self.pkts[..self.count as usize], dest).await
    }
}

fn placeholder_addr() -> SocketAddr {
    use std::net::{IpAddr, Ipv4Addr};
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
}

fn group_by_peer<T, F>(data: &mut [T], groups: &mut Vec<Range<u16>>, eq: F)
where
    F: Fn(&T, &T) -> bool,
{
    if data.is_empty() {
        return;
    }
    let maxn = data.len();
    let (mut beg, mut end) = (0, 1);
    while end < maxn {
        if eq(&data[end], &data[beg]) {
            end += 1;
            continue;
        }
        let mut probe = end + 1;
        while probe < maxn {
            if eq(&data[probe], &data[beg]) {
                data.swap(probe, end);
                end += 1;
            }
            probe += 1;
        }
        groups.push(beg as u16..end as u16);
        (beg, end) = (end, end + 1);
    }
    groups.push(beg as u16..end as u16);
}

#[cfg(target_os = "linux")]
mod io {
    use super::*;
    use std::io::{IoSlice, IoSliceMut};
    use std::mem::MaybeUninit;

    use realm_core::realm_io::mmsg::{
        MmsgHdr, MmsgHdrMut, SockAddrStore, recv_mul_pkts, send_mul_pkts,
    };

    pub async fn recv_some(sock: &UdpSocket, pkts: &mut [Packet]) -> Result<usize> {
        let pkt_amt = pkts.len().min(MAX_PACKETS);
        let mut iovs: MaybeUninit<[IoSliceMut; MAX_PACKETS]> = MaybeUninit::uninit();
        let mut addrs: MaybeUninit<[SockAddrStore; MAX_PACKETS]> = MaybeUninit::uninit();
        let mut msgs: MaybeUninit<[MmsgHdrMut; MAX_PACKETS]> = MaybeUninit::uninit();
        let iovs = unsafe { iovs.assume_init_mut() };
        let addrs = unsafe { addrs.assume_init_mut() };
        let msgs = unsafe { msgs.assume_init_mut() };

        for ((pkt, iov), (addr, msg)) in pkts
            .iter_mut()
            .zip(iovs.iter_mut())
            .zip(addrs.iter_mut().zip(msgs.iter_mut()))
        {
            *addr = SockAddrStore::new();
            *iov = IoSliceMut::new(&mut pkt.buf);
            *msg = MmsgHdrMut::new()
                .with_addr(addr)
                .with_iovec(std::slice::from_mut(iov));
        }

        let n = recv_mul_pkts(sock, &mut msgs[..pkt_amt]).await?;
        for (pkt, msg) in pkts.iter_mut().zip(msgs.iter()).take(n) {
            pkt.len = msg.get_ref().nbytes() as u16;
            pkt.peer = SocketAddr::from(msg.get_ref().addr().clone());
        }
        Ok(n)
    }

    pub async fn send_to_remote(
        sock: &UdpSocket,
        pkts: &[Packet],
        remote: SocketAddr,
    ) -> Result<()> {
        send_pkts(sock, pkts.iter().map(|p| (p.payload(), remote))).await
    }

    async fn send_pkts<'a, I>(sock: &UdpSocket, pkts: I) -> Result<()>
    where
        I: ExactSizeIterator<Item = (&'a [u8], SocketAddr)>,
    {
        let pkt_amt = pkts.len();
        if pkt_amt == 0 {
            return Ok(());
        }

        let collected: Vec<_> = pkts.collect();
        let mut addrs: Vec<SockAddrStore> = collected.iter().map(|(_, a)| (*a).into()).collect();
        let mut iovs: Vec<IoSlice<'_>> =
            collected.iter().map(|(buf, _)| IoSlice::new(buf)).collect();
        let mut msgs: Vec<MmsgHdr<'_, '_, '_, '_>> = Vec::with_capacity(pkt_amt);

        for (idx, ((_, _), addr)) in collected.iter().zip(addrs.iter_mut()).enumerate() {
            msgs.push(
                MmsgHdr::new()
                    .with_addr(addr)
                    .with_iovec(std::slice::from_ref(&iovs[idx])),
            );
        }

        let mut cursor = 0;
        while cursor < pkt_amt {
            cursor += send_mul_pkts(sock, &mut msgs[cursor..pkt_amt]).await?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod io {
    use super::*;

    pub async fn recv_some(sock: &UdpSocket, pkts: &mut [Packet]) -> Result<usize> {
        let pkt = &mut pkts[0];
        match sock.recv_from(&mut pkt.buf).await {
            Ok((bytes, peer)) => {
                pkt.len = bytes as u16;
                pkt.peer = peer;
                Ok(1)
            }
            Err(e) if recoverable(e.kind()) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub async fn send_to_remote(
        sock: &UdpSocket,
        pkts: &[Packet],
        remote: SocketAddr,
    ) -> Result<()> {
        for pkt in pkts {
            sock.send_to(pkt.payload(), remote).await?;
        }
        Ok(())
    }
}

pub use io::{recv_some, send_to_remote};

pub fn recoverable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(octet: u8, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, octet)), port)
    }

    #[test]
    fn group_by_peer_clusters_same_client() {
        let mut pkts = vec![
            Packet {
                buf: [0; PACKET_SIZE],
                peer: addr(1, 1000),
                len: 1,
            },
            Packet {
                buf: [0; PACKET_SIZE],
                peer: addr(2, 1000),
                len: 1,
            },
            Packet {
                buf: [0; PACKET_SIZE],
                peer: addr(1, 1000),
                len: 1,
            },
        ];
        let mut groups = Vec::new();
        group_by_peer(&mut pkts, &mut groups, |a, b| a.peer == b.peer);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], 0..2);
        assert_eq!(groups[1], 2..3);
    }
}
