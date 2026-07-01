//! 登录防爆破：按 IP（IPv6 按 /64）限流，失败封禁递增。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_FAILURES: u32 = 5;
const BASE_BAN_SECS: u64 = 600;
const MAX_BAN_SECS: u64 = 86_400;
const MAX_ENTRIES: usize = 10_000;
const STALE_SECS: u64 = 86_400;

#[derive(Debug)]
struct Entry {
    failures: u32,
    ban_until: Option<Instant>,
    ban_level: u32,
    last_seen: Instant,
}

#[derive(Clone)]
pub struct LoginRateLimiter {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 检查是否被封禁；返回剩余封禁秒数（>0 表示仍被封）。
    pub fn check(&self, ip: IpAddr) -> u64 {
        let key = normalize_ip(ip);
        let mut map = self.inner.lock().expect("rate limiter lock");
        self.cleanup(&mut map);
        let now = Instant::now();

        let Some(entry) = map.get(&key) else {
            return 0;
        };

        if let Some(until) = entry.ban_until
            && until > now
        {
            return (until - now).as_secs().max(1);
        }
        0
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let key = normalize_ip(ip);
        let mut map = self.inner.lock().expect("rate limiter lock");
        self.cleanup(&mut map);
        let now = Instant::now();

        let entry = map.entry(key).or_insert(Entry {
            failures: 0,
            ban_until: None,
            ban_level: 0,
            last_seen: now,
        });

        entry.last_seen = now;
        entry.failures += 1;

        if entry.failures >= MAX_FAILURES {
            entry.ban_level = entry.ban_level.saturating_add(1);
            let multiplier = 1u64 << entry.ban_level.min(6);
            let ban_secs = (BASE_BAN_SECS * multiplier).min(MAX_BAN_SECS);
            entry.ban_until = Some(now + Duration::from_secs(ban_secs));
            entry.failures = 0;
        }
    }

    pub fn record_success(&self, ip: IpAddr) {
        let key = normalize_ip(ip);
        let mut map = self.inner.lock().expect("rate limiter lock");
        map.remove(&key);
    }

    fn cleanup(&self, map: &mut HashMap<String, Entry>) {
        let now = Instant::now();
        map.retain(|_, e| {
            if let Some(until) = e.ban_until
                && until > now
            {
                return true;
            }
            now.duration_since(e.last_seen).as_secs() < STALE_SECS
        });

        if map.len() <= MAX_ENTRIES {
            return;
        }

        let mut keys: Vec<_> = map.keys().cloned().collect();
        keys.sort_by_key(|k| map.get(k).map(|e| e.last_seen).unwrap_or(now));
        let excess = map.len() - MAX_ENTRIES;
        for key in keys.into_iter().take(excess) {
            map.remove(&key);
        }
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// IPv4 精确匹配；IPv6 按 /64 网段聚合。
fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => format!("v4:{v4}"),
        IpAddr::V6(v6) => {
            let o = v6.octets();
            format!(
                "v6:{:x}:{:x}:{:x}:{:x}:/64",
                u16::from_be_bytes([o[0], o[1]]),
                u16::from_be_bytes([o[2], o[3]]),
                u16::from_be_bytes([o[4], o[5]]),
                u16::from_be_bytes([o[6], o[7]]),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    #[test]
    fn ipv6_normalized_to_slash64() {
        let ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0x1234, 0x5678, 0, 0, 0, 1));
        assert_eq!(normalize_ip(ip), "v6:2001:db8:1234:5678:/64");
    }
}
