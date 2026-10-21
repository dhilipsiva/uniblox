//! Signaling rate limits and caps (ADR-0047): a token bucket per client IP for connection attempts
//! and token requests, a token bucket per connection for signal messages, a size cap per signal and a
//! cap on peers per session.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;

/// The limits a signaling server enforces. The defaults suit a hosted demo; a deployment behind a
/// reverse proxy sets [`ClientIp::ForwardedFor`] in [`crate::SignalingConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalingLimits {
    /// Connection attempts (and token requests) one client IP may make at once.
    pub connects_burst: u32,
    /// How often one more attempt becomes available per client IP.
    pub connect_refill: Duration,
    /// Signal messages one connection may send at once.
    pub signals_burst: u32,
    /// Signal messages per second one connection may sustain (0 = no refill).
    pub signals_per_sec: u32,
    /// The largest signal message relayed, in bytes; a larger one is dropped.
    pub max_signal_bytes: usize,
    /// Dropped messages (over the rate or the size) after which the connection is closed.
    pub abuse_close_after: u32,
    /// The most peers one session may hold; `?next` may not ask for more.
    pub max_session_peers: usize,
    /// The most client IPs tracked at once; when full, a new IP is refused until buckets refill.
    pub max_tracked_ips: usize,
}

impl Default for SignalingLimits {
    fn default() -> Self {
        Self {
            connects_burst: 30,
            connect_refill: Duration::from_secs(2),
            signals_burst: 200,
            signals_per_sec: 50,
            max_signal_bytes: 32 * 1024,
            abuse_close_after: 100,
            max_session_peers: 64,
            max_tracked_ips: 100_000,
        }
    }
}

/// Where the client IP for the per-IP limit comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientIp {
    /// The TCP peer address (a server facing clients directly).
    #[default]
    Peer,
    /// The right-most `X-Forwarded-For` entry, the one the trusted reverse proxy appended; the peer
    /// address when the header is absent or unparsable. Only safe behind a proxy that sets it.
    ForwardedFor,
}

impl ClientIp {
    /// The key the per-IP limit counts: the address, with IPv6 narrowed to its /64 (one host usually
    /// holds a whole /64).
    pub(crate) fn key(self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let ip = match self {
            ClientIp::Peer => peer.ip(),
            ClientIp::ForwardedFor => headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .next_back()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(peer.ip()),
        };
        match ip {
            IpAddr::V6(v6) => {
                let mut seg = v6.segments();
                seg[4..].fill(0);
                IpAddr::from(seg)
            }
            v4 => v4,
        }
    }
}

/// A token bucket: `capacity` tokens, one more every `refill` (never, if `refill` is zero).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub(crate) fn full(capacity: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(capacity),
            last: now,
        }
    }

    fn refill(&mut self, capacity: u32, refill: Duration, now: Instant) {
        if !refill.is_zero() {
            let gained =
                now.saturating_duration_since(self.last).as_secs_f64() / refill.as_secs_f64();
            self.tokens = (self.tokens + gained).min(f64::from(capacity));
        }
        self.last = now;
    }

    /// Take one token if there is one.
    pub(crate) fn try_take(&mut self, capacity: u32, refill: Duration, now: Instant) -> bool {
        self.refill(capacity, refill, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn is_full(&mut self, capacity: u32, refill: Duration, now: Instant) -> bool {
        self.refill(capacity, refill, now);
        self.tokens >= f64::from(capacity)
    }
}

/// The per-IP limit shared by the gate and the token endpoint.
#[derive(Debug, Default)]
pub(crate) struct IpLimiter {
    buckets: Mutex<HashMap<IpAddr, TokenBucket>>,
}

impl IpLimiter {
    /// Whether `ip` may make one more attempt now. A poisoned lock refuses (fail closed).
    pub(crate) fn allow(&self, ip: IpAddr, limits: &SignalingLimits) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else {
            return false;
        };
        let now = Instant::now();
        if !buckets.contains_key(&ip) && buckets.len() >= limits.max_tracked_ips {
            // Forget the clients that are back to a full bucket; they lose nothing.
            buckets.retain(|_, b| !b.is_full(limits.connects_burst, limits.connect_refill, now));
            if buckets.len() >= limits.max_tracked_ips {
                return false;
            }
        }
        buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::full(limits.connects_burst, now))
            .try_take(limits.connects_burst, limits.connect_refill, now)
    }
}

/// The interval between two signal tokens for a per-second rate (zero = no refill).
pub(crate) fn per_second(rate: u32) -> Duration {
    if rate == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(1) / rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_allows_its_burst_then_refills() {
        let t0 = Instant::now();
        let refill = Duration::from_secs(1);
        let mut b = TokenBucket::full(2, t0);
        assert!(b.try_take(2, refill, t0));
        assert!(b.try_take(2, refill, t0));
        assert!(!b.try_take(2, refill, t0));
        assert!(!b.try_take(2, refill, t0 + Duration::from_millis(500)));
        assert!(b.try_take(2, refill, t0 + Duration::from_millis(1000)));
        // It never holds more than its capacity.
        let later = t0 + Duration::from_secs(100);
        assert!(b.try_take(2, refill, later));
        assert!(b.try_take(2, refill, later));
        assert!(!b.try_take(2, refill, later));
    }

    #[test]
    fn a_zero_refill_never_refills() {
        let t0 = Instant::now();
        let mut b = TokenBucket::full(1, t0);
        assert!(b.try_take(1, Duration::ZERO, t0));
        assert!(!b.try_take(1, Duration::ZERO, t0 + Duration::from_secs(1000)));
    }

    #[test]
    fn ipv6_clients_are_counted_per_64() {
        let a: SocketAddr = "[2001:db8:1:2:aaaa::1]:1".parse().unwrap();
        let b: SocketAddr = "[2001:db8:1:2:bbbb::9]:2".parse().unwrap();
        let c: SocketAddr = "[2001:db8:1:3::1]:3".parse().unwrap();
        let h = HeaderMap::new();
        assert_eq!(ClientIp::Peer.key(a, &h), ClientIp::Peer.key(b, &h));
        assert_ne!(ClientIp::Peer.key(a, &h), ClientIp::Peer.key(c, &h));
    }

    #[test]
    fn forwarded_for_takes_the_proxy_appended_entry() {
        let peer: SocketAddr = "10.0.0.1:5".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.1.1.1, 203.0.113.7".parse().unwrap());
        assert_eq!(
            ClientIp::ForwardedFor.key(peer, &h),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(ClientIp::Peer.key(peer, &h), peer.ip());
        assert_eq!(
            ClientIp::ForwardedFor.key(peer, &HeaderMap::new()),
            peer.ip()
        );
    }

    #[test]
    fn the_ip_table_is_bounded() {
        let limits = SignalingLimits {
            max_tracked_ips: 2,
            connects_burst: 1,
            connect_refill: Duration::from_secs(3600),
            ..SignalingLimits::default()
        };
        let lim = IpLimiter::default();
        let ip = |n: u8| IpAddr::from([10, 0, 0, n]);
        assert!(lim.allow(ip(1), &limits));
        assert!(lim.allow(ip(2), &limits));
        assert!(
            !lim.allow(ip(3), &limits),
            "the table is full of clients still limited"
        );
    }
}
