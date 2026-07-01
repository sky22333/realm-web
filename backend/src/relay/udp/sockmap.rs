//! Client ↔ downstream UDP socket associations (read-heavy `RwLock` map).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use tokio::net::UdpSocket;

pub struct SockMap(RwLock<HashMap<SocketAddr, Arc<UdpSocket>>>);

impl SockMap {
    pub fn new() -> Self {
        Self(RwLock::new(HashMap::new()))
    }

    pub fn find_or_insert<E, F>(&self, addr: &SocketAddr, f: F) -> Result<Arc<UdpSocket>, E>
    where
        F: FnOnce() -> Result<Arc<UdpSocket>, E>,
    {
        if let Some(sock) = self.0.read().unwrap_or_else(|e| e.into_inner()).get(addr).cloned() {
            return Ok(sock);
        }
        let sock = f()?;
        self.0
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(*addr, Arc::clone(&sock));
        Ok(sock)
    }

    pub fn remove(&self, addr: &SocketAddr) {
        let _ = self
            .0
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(addr);
    }
}

impl Default for SockMap {
    fn default() -> Self {
        Self::new()
    }
}
