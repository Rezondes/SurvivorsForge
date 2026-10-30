//! Per-IP request limit with a fixed one-minute window, shared by the API and the website.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ponytail: in-process fixed window, one instance only; a shared store when a service scales out.
pub struct RateLimiter {
    per_minute: u32,
    trusted_proxies: Vec<IpAddr>,
    window: Mutex<(Instant, HashMap<IpAddr, u32>)>,
}

/// The client address behind `trusted_proxies`; see `RateLimiter::client_ip`.
pub fn client_ip(trusted_proxies: &[IpAddr], peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
    if !trusted_proxies.contains(&peer) {
        return peer;
    }
    forwarded_for
        .into_iter()
        .flat_map(|h| h.rsplit(','))
        .filter_map(|ip| ip.trim().parse::<IpAddr>().ok())
        .find(|ip| !trusted_proxies.contains(ip))
        .unwrap_or(peer)
}

impl RateLimiter {
    pub fn new(per_minute: u32, trusted_proxies: Vec<IpAddr>) -> RateLimiter {
        RateLimiter { per_minute, trusted_proxies, window: Mutex::new((Instant::now(), HashMap::new())) }
    }

    /// The client address. `X-Forwarded-For` only counts when the direct peer is a trusted proxy;
    /// then the rightmost address that is not a trusted proxy is the client.
    pub fn client_ip(&self, peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
        client_ip(&self.trusted_proxies, peer, forwarded_for)
    }

    /// Counts one request; false when the limit for this minute is used up.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut window = self.window.lock().unwrap();
        if now.duration_since(window.0) >= Duration::from_secs(60) {
            *window = (now, HashMap::new());
        }
        let count = window.1.entry(ip).or_insert(0);
        *count += 1;
        *count <= self.per_minute
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_for_only_from_trusted_proxy() {
        let proxy: IpAddr = "10.0.0.2".parse().unwrap();
        let limiter = RateLimiter::new(1, vec![proxy]);
        let other: IpAddr = "203.0.113.9".parse().unwrap();
        let client: IpAddr = "1.2.3.4".parse().unwrap();
        assert_eq!(limiter.client_ip(other, Some("1.2.3.4")), other);
        assert_eq!(limiter.client_ip(proxy, Some("6.6.6.6, 1.2.3.4")), client);
        assert_eq!(limiter.client_ip(proxy, Some("1.2.3.4, 10.0.0.2")), client);
        assert_eq!(limiter.client_ip(proxy, None), proxy);
    }

    #[test]
    fn window_resets_after_a_minute() {
        let limiter = RateLimiter::new(1, vec![]);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let start = Instant::now();
        assert!(limiter.allow(ip, start));
        assert!(!limiter.allow(ip, start));
        assert!(limiter.allow(ip, start + Duration::from_secs(61)));
    }
}
