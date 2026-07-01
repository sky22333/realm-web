//! In-memory traffic counters with atomic updates.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::TrafficTotals;

use super::RuleControl;

pub struct TrafficMeter {
    tcp_rx: AtomicU64,
    tcp_tx: AtomicU64,
    udp_rx: AtomicU64,
    udp_tx: AtomicU64,
    control: Arc<RuleControl>,
}

impl TrafficMeter {
    pub fn new(control: Arc<RuleControl>) -> Self {
        Self {
            tcp_rx: AtomicU64::new(0),
            tcp_tx: AtomicU64::new(0),
            udp_rx: AtomicU64::new(0),
            udp_tx: AtomicU64::new(0),
            control,
        }
    }

    fn record<F: FnOnce()>(&self, update: F) {
        update();
        self.control.check_after_add(self.snapshot());
    }

    pub fn add_tcp_rx(&self, n: u64) {
        self.record(|| {
            self.tcp_rx.fetch_add(n, Ordering::Relaxed);
        });
    }

    pub fn add_tcp_tx(&self, n: u64) {
        self.record(|| {
            self.tcp_tx.fetch_add(n, Ordering::Relaxed);
        });
    }

    pub fn add_udp_rx(&self, n: u64) {
        self.record(|| {
            self.udp_rx.fetch_add(n, Ordering::Relaxed);
        });
    }

    pub fn add_udp_tx(&self, n: u64) {
        self.record(|| {
            self.udp_tx.fetch_add(n, Ordering::Relaxed);
        });
    }

    pub fn snapshot(&self) -> TrafficTotals {
        TrafficTotals {
            tcp_rx: self.tcp_rx.load(Ordering::Relaxed),
            tcp_tx: self.tcp_tx.load(Ordering::Relaxed),
            udp_rx: self.udp_rx.load(Ordering::Relaxed),
            udp_tx: self.udp_tx.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&self) {
        self.tcp_rx.store(0, Ordering::Relaxed);
        self.tcp_tx.store(0, Ordering::Relaxed);
        self.udp_rx.store(0, Ordering::Relaxed);
        self.udp_tx.store(0, Ordering::Relaxed);
    }

    pub fn restore(&self, totals: &TrafficTotals) {
        self.tcp_rx.store(totals.tcp_rx, Ordering::Relaxed);
        self.tcp_tx.store(totals.tcp_tx, Ordering::Relaxed);
        self.udp_rx.store(totals.udp_rx, Ordering::Relaxed);
        self.udp_tx.store(totals.udp_tx, Ordering::Relaxed);
    }
}
