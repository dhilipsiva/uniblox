//! Signaling rate limits and caps (ADR-0047): a token bucket per client IP for connection attempts
//! and token requests, caps on open connections per IP and in total, a token bucket per connection
//! for its frames, a size cap per message (enforced by the WebSocket layer), an idle timeout, a
//! bounded outbound queue per peer, and a cap on peers per session.

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
    /// Open connections one client IP may hold at once.
    pub max_connections_per_ip: usize,
    /// Open connections the server holds at once.
    pub max_connections: usize,
    /// Frames one connection may send at once.
    pub signals_burst: u32,
    /// Frames per second one connection may sustain (0 = no refill).
    pub signals_per_sec: u32,
    /// The largest WebSocket message (and frame) accepted, in bytes. The WebSocket layer enforces
    /// it as fragments arrive and closes a connection that exceeds it.
    pub max_signal_bytes: usize,
    /// Frames dropped over the rate, net of the frames accepted since, after which the connection is
    /// closed.
    pub abuse_close_after: u32,
    /// A connection that sends no complete message for this long is closed (matchbox clients send a
    /// keep-alive every 10 s).
    pub idle_timeout: Duration,
    /// Messages queued for one peer; a peer whose queue is full is not reading and is disconnected.
    pub max_outbound_queue: usize,
    /// Bytes the WebSocket layer may buffer for one peer's writes before the connection fails.
    pub max_write_buffer_bytes: usize,
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
            max_connections_per_ip: 16,
            max_connections: 10_000,
            // A newcomer to a full session answers 63 offers with a few ICE candidates each.
            signals_burst: 1024,
            signals_per_sec: 100,
            max_signal_bytes: 32 * 1024,
            abuse_close_after: 256,
            idle_timeout: Duration::from_secs(60),
            max_outbound_queue: 256,
            max_write_buffer_bytes: 1024 * 1024,
            max_session_peers: 64,
            max_tracked_ips: 100_000,
        }
    }
}

/// Where the client IP for the per-IP limits comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientIp {
    /// The TCP peer address (a server facing clients directly).
    #[default]
    Peer,
    /// From `X-Forwarded-For`, behind this many trusted proxies (at least 1): the entry that many
    /// from the right, the one the outermost trusted proxy appended. The peer address when the header
    /// is absent, too short or unparsable. Only safe behind proxies that set it.
    ForwardedFor(usize),
}

/// One `X-Forwarded-For` entry: a bare address, `a.b.c.d:port`, `[v6]` or `[v6]:port`.
fn forwarded_ip(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|s| s.ip()))
        .or_else(|| {
            entry
                .strip_prefix('[')
                .and_then(|e| e.strip_suffix(']'))
                .and_then(|e| e.parse().ok())
        })
}

impl ClientIp {
    /// The key the per-IP limits count: the address (an IPv4-mapped IPv6 address as its IPv4), with
    /// IPv6 narrowed to its /64 (one host usually holds a whole /64).
    pub(crate) fn key(self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let ip = match self {
            ClientIp::Peer => peer.ip(),
            ClientIp::ForwardedFor(hops) => {
                let entries: Vec<&str> = headers
                    .get_all("x-forwarded-for")
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .flat_map(|v| v.split(','))
                    .collect();
                hops.checked_sub(1)
                    .and_then(|back| entries.len().checked_sub(back + 1))
                    .and_then(|i| forwarded_ip(entries[i]))
                    .unwrap_or(peer.ip())
            }
        };
        match ip.to_canonical() {
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
    buckets: Mutex<Buckets>,
}

#[derive(Debug, Default)]
struct Buckets {
    by_ip: HashMap<IpAddr, TokenBucket>,
    /// When a full table was last pruned; pruning walks it all, so at most once a second.
    last_prune: Option<Instant>,
}

impl IpLimiter {
    /// Whether `ip` may make one more attempt now. A poisoned lock refuses (fail closed).
    pub(crate) fn allow(&self, ip: IpAddr, limits: &SignalingLimits) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else {
            return false;
        };
        let now = Instant::now();
        if !buckets.by_ip.contains_key(&ip) && buckets.by_ip.len() >= limits.max_tracked_ips {
            let due = buckets
                .last_prune
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_secs(1));
            if !due {
                return false;
            }
            buckets.last_prune = Some(now);
            // Forget the clients that are back to a full bucket; they lose nothing.
            buckets
                .by_ip
                .retain(|_, b| !b.is_full(limits.connects_burst, limits.connect_refill, now));
            if buckets.by_ip.len() >= limits.max_tracked_ips {
                return false;
            }
        }
        buckets
            .by_ip
            .entry(ip)
            .or_insert_with(|| TokenBucket::full(limits.connects_burst, now))
            .try_take(limits.connects_burst, limits.connect_refill, now)
    }
}

/// The interval between two frame tokens for a per-second rate (zero = no refill); never rounds a
/// positive rate down to "no refill".
pub(crate) fn per_second(rate: u32) -> Duration {
    if rate == 0 {
        Duration::ZERO
    } else {
        (Duration::from_secs(1) / rate).max(Duration::from_nanos(1))
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
    fn forwarded_for_takes_the_entry_of_the_outermost_trusted_proxy() {
        let peer: SocketAddr = "10.0.0.1:5".parse().unwrap();
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.1.1.1, 203.0.113.7".parse().unwrap());
        assert_eq!(ClientIp::ForwardedFor(1).key(peer, &h), ip("203.0.113.7"));
        assert_eq!(ClientIp::ForwardedFor(2).key(peer, &h), ip("1.1.1.1"));
        assert_eq!(
            ClientIp::ForwardedFor(3).key(peer, &h),
            peer.ip(),
            "too few entries"
        );
        assert_eq!(ClientIp::ForwardedFor(0).key(peer, &h), peer.ip());
        assert_eq!(ClientIp::Peer.key(peer, &h), peer.ip());
        assert_eq!(
            ClientIp::ForwardedFor(1).key(peer, &HeaderMap::new()),
            peer.ip()
        );
        for (entry, want) in [
            ("198.51.100.9:4711", "198.51.100.9"),
            ("[2001:db8::7]", "2001:db8::"),
            ("[2001:db8::7]:443", "2001:db8::"),
        ] {
            let mut h = HeaderMap::new();
            h.insert("x-forwarded-for", entry.parse().unwrap());
            assert_eq!(ClientIp::ForwardedFor(1).key(peer, &h), ip(want), "{entry}");
        }
    }

    /// The review found every IPv4-mapped client keyed as `::` (one bucket for all of IPv4).
    #[test]
    fn ipv4_mapped_clients_are_keyed_by_their_ipv4() {
        let h = HeaderMap::new();
        let a: SocketAddr = "[::ffff:203.0.113.7]:1".parse().unwrap();
        let b: SocketAddr = "[::ffff:198.51.100.9]:1".parse().unwrap();
        assert_eq!(
            ClientIp::Peer.key(a, &h),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
        assert_ne!(ClientIp::Peer.key(a, &h), ClientIp::Peer.key(b, &h));
    }

    #[test]
    fn a_huge_rate_still_refills() {
        assert!(per_second(u32::MAX) > Duration::ZERO);
        assert_eq!(per_second(0), Duration::ZERO);
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
