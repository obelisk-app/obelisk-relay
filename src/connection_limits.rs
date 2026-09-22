//! Admission control for WebSocket connections, applied *before* NIP-42 AUTH.
//!
//! The settings file has always carried `websocket.max_connections`, but nothing
//! enforced it: `server.rs` called `relay_builder::handle_upgrade`, which hands
//! `ConnectionConfig::default()` -- every field `None` -- to `handle_socket`. The
//! duration and idle timeouts were dead for the same reason. relay_builder does
//! implement a connection semaphore, but only inside its own `ws_handler`, which
//! this relay does not use because it serves NIP-11, the frontend and the socket
//! from one route.
//!
//! So the cap lives here. Two limits, both applied at upgrade time:
//!
//! * a global permit count, the configured `max_connections`;
//! * a per-IP cap, because the global one alone is a self-service outage -- one
//!   host can take every permit and lock everyone else out. Nothing else in the
//!   stack is per-IP: the rate limits are per-connection and per-pubkey, and both
//!   only start applying after AUTH, which an attacker never has to reach.
//!
//! The permit is released on drop, so a connection holds it for exactly as long
//! as its socket lives.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing::warn;

/// Per-IP ceiling. Obelisk clients hold one long-lived socket each, but a shared
/// NAT, a household, or one user with several tabs and a phone are all normal, so
/// this is set well above honest use and only bites on an actual flood.
pub const DEFAULT_MAX_CONNECTIONS_PER_IP: usize = 32;

/// A live connection's claim on the limiter. Releasing happens in `Drop`, so it
/// covers early returns and panics without the call sites having to remember.
pub struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
    ip: IpAddr,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut state = self.limiter.state.lock();
        state.total = state.total.saturating_sub(1);
        if let Some(count) = state.per_ip.get_mut(&self.ip) {
            *count -= 1;
            // Drop the entry at zero. Keeping it would make the map grow with
            // every distinct address that ever connected -- which is exactly the
            // unbounded allocation this module exists to prevent.
            if *count == 0 {
                state.per_ip.remove(&self.ip);
            }
        }
    }
}

#[derive(Default)]
struct LimiterState {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// Why a connection was turned away, so the caller can log and count the two
/// cases separately: a global refusal means the relay is at capacity, a per-IP
/// refusal means one host is misbehaving. They call for different responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    GlobalLimit,
    PerIpLimit,
}

pub struct ConnectionLimiter {
    max_total: Option<usize>,
    max_per_ip: usize,
    state: Mutex<LimiterState>,
}

impl ConnectionLimiter {
    pub fn new(max_total: Option<usize>, max_per_ip: usize) -> Arc<Self> {
        Arc::new(Self {
            max_total,
            max_per_ip,
            state: Mutex::new(LimiterState::default()),
        })
    }

    /// Claim a slot for `ip`, or explain why not. The global limit is checked
    /// first: when the relay is genuinely full, that is the more useful reason to
    /// report, even for an IP that is also over its own cap.
    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Result<ConnectionPermit, RejectReason> {
        let mut state = self.state.lock();

        if let Some(max) = self.max_total {
            if state.total >= max {
                return Err(RejectReason::GlobalLimit);
            }
        }

        let per_ip = state.per_ip.entry(ip).or_insert(0);
        if *per_ip >= self.max_per_ip {
            // Leave no zero entry behind if this insert created one.
            if *per_ip == 0 {
                state.per_ip.remove(&ip);
            }
            return Err(RejectReason::PerIpLimit);
        }

        *per_ip += 1;
        state.total += 1;

        Ok(ConnectionPermit {
            limiter: Arc::clone(self),
            ip,
        })
    }

    /// Current connection count, for `/metrics` and the admin console.
    pub fn active(&self) -> usize {
        self.state.lock().total
    }

    /// The limits this process is actually enforcing. These are fixed at startup,
    /// so they are what the admin console should show as "running" alongside a
    /// saved-but-not-yet-applied value.
    pub fn configured_max_total(&self) -> Option<usize> {
        self.max_total
    }

    pub fn configured_max_per_ip(&self) -> usize {
        self.max_per_ip
    }

    pub fn log_rejection(&self, ip: IpAddr, reason: RejectReason) {
        match reason {
            RejectReason::GlobalLimit => warn!(
                "Refusing connection from {ip}: relay is at its {:?}-connection limit",
                self.max_total
            ),
            RejectReason::PerIpLimit => warn!(
                "Refusing connection from {ip}: that address already holds {} connections",
                self.max_per_ip
            ),
        }
    }
}

/// The client's real address.
///
/// Every public connection arrives through the Cloudflare tunnel, so the socket
/// peer address is the tunnel's, identical for all of them -- a per-IP cap keyed
/// on it would count the whole internet as one host and lock everybody out after
/// `max_per_ip`. `CF-Connecting-IP` carries the true origin.
///
/// This trusts the header, which is only sound because the relay is not reachable
/// except through the tunnel (it binds `127.0.0.1:8081`). If it is ever exposed
/// directly, a client could forge the header and buy itself unlimited connections,
/// so the binding and this function have to stay in agreement.
pub fn client_ip(headers: &axum::http::HeaderMap, peer: IpAddr) -> IpAddr {
    headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .unwrap_or(peer)
}

/// Wraps the real handler so the permit lives exactly as long as the connection.
///
/// `handle_upgrade_with_config` takes ownership of the handler and drops it when
/// the socket closes, so parking the permit alongside it releases the slot at the
/// right moment without a cleanup path that could be missed on an error return.
pub struct PermitHolder<H> {
    pub handler: H,
    pub permit: ConnectionPermit,
}

impl<H: relay_builder::websocket::WebSocketHandler> relay_builder::websocket::WebSocketHandler
    for PermitHolder<H>
{
    fn on_connect(
        &mut self,
        remote_addr: std::net::SocketAddr,
        sink: futures::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        self.handler.on_connect(remote_addr, sink)
    }

    fn on_message(
        &mut self,
        text: relay_builder::websocket::Utf8Bytes,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send {
        self.handler.on_message(text)
    }

    fn on_disconnect(
        &mut self,
        reason: relay_builder::websocket::DisconnectReason,
    ) -> impl std::future::Future<Output = ()> + Send {
        self.handler.on_disconnect(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    #[test]
    fn the_global_limit_is_enforced() {
        let limiter = ConnectionLimiter::new(Some(2), 100);
        let _a = limiter.try_acquire(ip(1)).expect("first fits");
        let _b = limiter.try_acquire(ip(2)).expect("second fits");
        assert_eq!(
            limiter.try_acquire(ip(3)).err(),
            Some(RejectReason::GlobalLimit),
            "a third connection must be refused once the relay is full"
        );
    }

    #[test]
    fn a_permit_frees_its_slot_when_dropped() {
        let limiter = ConnectionLimiter::new(Some(1), 100);
        {
            let _held = limiter.try_acquire(ip(1)).expect("first fits");
            assert!(limiter.try_acquire(ip(2)).is_err());
        }
        assert_eq!(limiter.active(), 0);
        limiter.try_acquire(ip(2)).expect("the slot came back");
    }

    #[test]
    fn active_counts_every_live_connection() {
        // `active()` is what the admin console reports as "active connections"
        // and what the Prometheus gauge publishes. The console used to read a
        // different counter that nothing incremented, so it showed nought on a
        // busy relay; this pins the surviving source to the permits actually
        // held, across several addresses and after some have gone away.
        let limiter = ConnectionLimiter::new(Some(100), 100);
        assert_eq!(limiter.active(), 0, "a fresh relay has no connections");

        let a = limiter.try_acquire(ip(1)).unwrap();
        let b = limiter.try_acquire(ip(1)).unwrap();
        let c = limiter.try_acquire(ip(2)).unwrap();
        assert_eq!(
            limiter.active(),
            3,
            "two sockets from one address and one from another are three connections"
        );

        drop(b);
        assert_eq!(limiter.active(), 2, "a closed socket stops counting");

        drop(a);
        drop(c);
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn one_address_cannot_exhaust_the_relay() {
        // The regression this module exists for: without a per-IP cap, a single
        // host takes every slot and every other client is refused.
        let limiter = ConnectionLimiter::new(Some(100), 2);
        let _a = limiter.try_acquire(ip(1)).unwrap();
        let _b = limiter.try_acquire(ip(1)).unwrap();

        assert_eq!(
            limiter.try_acquire(ip(1)).err(),
            Some(RejectReason::PerIpLimit)
        );
        limiter
            .try_acquire(ip(2))
            .expect("a different address is unaffected");
    }

    #[test]
    fn the_per_ip_map_does_not_grow_without_bound() {
        let limiter = ConnectionLimiter::new(None, 4);
        for last in 0..50 {
            let _permit = limiter.try_acquire(ip(last)).unwrap();
        }
        assert_eq!(limiter.active(), 0);
        assert!(
            limiter.state.lock().per_ip.is_empty(),
            "entries must be removed at zero, or the map is itself a leak"
        );
    }

    #[test]
    fn a_refused_address_leaves_no_entry_behind() {
        let limiter = ConnectionLimiter::new(None, 1);
        let held = limiter.try_acquire(ip(1)).unwrap();
        assert_eq!(
            limiter.try_acquire(ip(1)).err(),
            Some(RejectReason::PerIpLimit)
        );
        drop(held);
        assert!(limiter.state.lock().per_ip.is_empty());
    }

    #[test]
    fn no_configured_total_means_only_the_per_ip_cap_applies() {
        let limiter = ConnectionLimiter::new(None, 2);
        let _a = limiter.try_acquire(ip(1)).unwrap();
        let _b = limiter.try_acquire(ip(1)).unwrap();
        assert_eq!(
            limiter.try_acquire(ip(1)).err(),
            Some(RejectReason::PerIpLimit)
        );
        for last in 2..40 {
            limiter
                .try_acquire(ip(last))
                .expect("other hosts still fit");
        }
    }

    #[test]
    fn the_forwarded_header_wins_over_the_tunnel_address() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        assert_eq!(
            client_ip(&headers, ip(1)),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn a_missing_or_junk_header_falls_back_to_the_peer() {
        let empty = axum::http::HeaderMap::new();
        assert_eq!(client_ip(&empty, ip(1)), ip(1));

        let mut junk = axum::http::HeaderMap::new();
        junk.insert("cf-connecting-ip", "not-an-address".parse().unwrap());
        assert_eq!(client_ip(&junk, ip(1)), ip(1));
    }
}
