use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware::Next,
    response::Response,
};
use clipper_core::{crypto::sha256, models::ApiErrorCode};
use governor::{DefaultDirectRateLimiter, DefaultKeyedRateLimiter, Quota, RateLimiter as Governor};
use ipnet::IpNet;
use uuid::Uuid;

const X_FORWARDED_FOR: &str = "x-forwarded-for";
const X_REAL_IP: &str = "x-real-ip";

/// A /64 is the smallest IPv6 allocation a client realistically controls;
/// keying buckets any finer hands an attacker unlimited fresh keys.
const IPV6_CLIENT_PREFIX_MASK: u128 = !0 << 64;

use crate::{
    auth::AuthInfo,
    config::RateLimitConfig,
    routes::{ApiError, error_response},
    state::AppState,
};

pub struct RateLimiter {
    auth_by_client: DefaultKeyedRateLimiter<IpAddr>,
    auth_by_username: DefaultKeyedRateLimiter<[u8; 16]>,
    auth_global: DefaultDirectRateLimiter,
    api_by_client: DefaultKeyedRateLimiter<IpAddr>,
    api_by_user: DefaultKeyedRateLimiter<Uuid>,
    ws_tickets_by_user: DefaultKeyedRateLimiter<Uuid>,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            auth_by_client: Governor::keyed(per_minute_quota(config.auth_per_client_per_minute)),
            auth_by_username: Governor::keyed(per_minute_quota(
                config.auth_per_username_per_minute,
            )),
            auth_global: Governor::direct(per_minute_quota(config.auth_global_per_minute)),
            api_by_client: Governor::keyed(per_minute_quota(config.api_per_client_per_minute)),
            api_by_user: Governor::keyed(per_minute_quota(config.api_per_user_per_minute)),
            ws_tickets_by_user: Governor::keyed(per_minute_quota(
                config.ws_tickets_per_user_per_minute,
            )),
        }
    }

    /// Returns true if the auth request is allowed, false if rate-limited.
    /// The per-client check runs first so blocked clients cannot drain the
    /// global bucket, which is a capacity ceiling rather than the primary
    /// limit.
    pub fn check_auth(&self, ip: IpAddr) -> bool {
        self.auth_by_client.check_key(&client_key(ip)).is_ok() && self.auth_global.check().is_ok()
    }

    /// Returns true if an OPAQUE challenge for this username is allowed.
    /// Backstops distributed password guessing that rotates client addresses,
    /// which the per-client bucket cannot see.
    pub fn check_auth_username(&self, username: &str) -> bool {
        self.auth_by_username
            .check_key(&username_key(username))
            .is_ok()
    }

    /// Returns true if a request to the authenticated API surface is allowed
    /// for this client address. Runs before token validation, so it bounds
    /// the database cost of invalid-token floods.
    pub fn check_api(&self, ip: IpAddr) -> bool {
        self.api_by_client.check_key(&client_key(ip)).is_ok()
    }

    /// Returns true if an authenticated request is allowed for this user.
    pub fn check_api_user(&self, user_id: Uuid) -> bool {
        self.api_by_user.check_key(&user_id).is_ok()
    }

    /// Returns true if this user may mint another WebSocket ticket.
    pub fn check_ws_ticket_user(&self, user_id: Uuid) -> bool {
        self.ws_tickets_by_user.check_key(&user_id).is_ok()
    }

    /// Prune stale per-key limiter state. Call periodically.
    pub fn prune(&self) {
        self.auth_by_client.retain_recent();
        self.auth_by_client.shrink_to_fit();
        self.auth_by_username.retain_recent();
        self.auth_by_username.shrink_to_fit();
        self.api_by_client.retain_recent();
        self.api_by_client.shrink_to_fit();
        self.api_by_user.retain_recent();
        self.api_by_user.shrink_to_fit();
        self.ws_tickets_by_user.retain_recent();
        self.ws_tickets_by_user.shrink_to_fit();
    }
}

pub(crate) fn rate_limited_error() -> ApiError {
    ApiError::from_code_with_message(ApiErrorCode::RateLimited, "Too many requests")
}

fn client_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & IPV6_CLIENT_PREFIX_MASK)),
    }
}

/// Usernames are attacker-controlled input; hashing them bounds key size and
/// keeps submitted usernames out of long-lived limiter state.
fn username_key(username: &str) -> [u8; 16] {
    let digest = sha256(username.as_bytes());
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

pub async fn auth_rate_limit_middleware(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.rate_limiter().check_auth(ip) {
        return Err(rate_limited_error());
    }

    req.extensions_mut().insert(ClientIp(ip));
    Ok(next.run(req).await)
}

pub async fn api_rate_limit_middleware(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.rate_limiter().check_api(ip) {
        return Err(rate_limited_error());
    }

    req.extensions_mut().insert(ClientIp(ip));
    Ok(next.run(req).await)
}

/// Per-user limit for the authenticated API surface. Must be layered inside
/// `auth_middleware` so `AuthInfo` is already in the request extensions.
pub async fn user_rate_limit_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth = req
        .extensions()
        .get::<AuthInfo>()
        .ok_or_else(|| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server error"))?;
    if !state.rate_limiter().check_api_user(auth.user_id) {
        return Err(rate_limited_error());
    }

    Ok(next.run(req).await)
}

#[derive(Clone, Debug, Default)]
pub struct TrustedProxies {
    networks: Arc<Vec<IpNet>>,
}

impl TrustedProxies {
    pub fn new(networks: Vec<IpNet>) -> Self {
        Self {
            networks: Arc::new(networks),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.networks.len()
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(&ip))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(ip) = parts.extensions.get::<ClientIp>() {
            return Ok(*ip);
        }

        let peer_addr = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .ok_or_else(|| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Server error"))?
            .0;
        let trusted_proxies = parts
            .extensions
            .get::<TrustedProxies>()
            .cloned()
            .unwrap_or_default();

        Ok(Self(client_ip_from_headers(
            &parts.headers,
            peer_addr,
            &trusted_proxies,
        )))
    }
}

pub fn client_ip_from_headers(
    headers: &HeaderMap,
    peer_addr: SocketAddr,
    trusted_proxies: &TrustedProxies,
) -> IpAddr {
    let peer_ip = peer_addr.ip();
    if !trusted_proxies.contains(peer_ip) {
        return peer_ip;
    }

    forwarded_chain(headers)
        .and_then(|chain| first_untrusted_forwarded_ip(&chain, trusted_proxies))
        .unwrap_or(peer_ip)
}

fn per_minute_quota(max: u32) -> Quota {
    Quota::per_minute(NonZeroU32::new(max).expect("rate-limit quota must be non-zero"))
}

fn forwarded_chain(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let x_forwarded_for = x_forwarded_for_chain(headers);
    if !x_forwarded_for.is_empty() {
        return Some(x_forwarded_for);
    }

    let forwarded = forwarded_header_chain(headers);
    if !forwarded.is_empty() {
        return Some(forwarded);
    }

    headers
        .get(X_REAL_IP)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_ip_token)
        .map(|ip| vec![ip])
}

fn first_untrusted_forwarded_ip(
    chain: &[IpAddr],
    trusted_proxies: &TrustedProxies,
) -> Option<IpAddr> {
    chain
        .iter()
        .rev()
        .copied()
        .find(|ip| !trusted_proxies.contains(*ip))
        .or_else(|| chain.first().copied())
}

fn x_forwarded_for_chain(headers: &HeaderMap) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for value in headers.get_all(X_FORWARDED_FOR) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        ips.extend(value.split(',').filter_map(parse_ip_token));
    }
    ips
}

fn forwarded_header_chain(headers: &HeaderMap) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for value in headers.get_all(header::FORWARDED) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for entry in value.split(',') {
            ips.extend(entry.split(';').find_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                name.eq_ignore_ascii_case("for")
                    .then(|| parse_ip_token(value))
                    .flatten()
            }));
        }
    }
    ips
}

fn parse_ip_token(value: &str) -> Option<IpAddr> {
    let value = value.trim().trim_matches('"').trim();
    if value.eq_ignore_ascii_case("unknown") || value.starts_with('_') {
        return None;
    }

    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip);
    }

    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Some(addr.ip());
    }

    if let Some(rest) = value.strip_prefix('[') {
        let (host, _) = rest.split_once(']')?;
        return host.parse::<IpAddr>().ok();
    }

    let (host, _) = value.rsplit_once(':')?;
    (!host.contains(':'))
        .then(|| host.parse::<IpAddr>().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;
    use crate::config::{ServerConfig, parse_trusted_proxy};

    fn header_map(name: &'static str, value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, value.parse().expect("header value"));
        headers
    }

    fn peer(ip: IpAddr) -> SocketAddr {
        SocketAddr::new(ip, 12345)
    }

    fn trusted(values: &[&str]) -> TrustedProxies {
        TrustedProxies::new(
            values
                .iter()
                .map(|value| parse_trusted_proxy(value).expect("trusted proxy"))
                .collect(),
        )
    }

    #[test]
    fn rate_limiter_blocks_after_ten_auth_attempts_per_ip() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        for _ in 0..config.rate_limit.auth_per_client_per_minute {
            assert!(limiter.check_auth(ip));
        }

        assert!(!limiter.check_auth(ip));
    }

    #[test]
    fn ipv6_clients_share_one_auth_bucket_per_64_prefix() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);
        let first: IpAddr = "2001:db8:1:1::1".parse().expect("ipv6");
        let same_prefix: IpAddr = "2001:db8:1:1:ffff::2".parse().expect("ipv6");
        let other_prefix: IpAddr = "2001:db8:1:2::1".parse().expect("ipv6");

        for _ in 0..config.rate_limit.auth_per_client_per_minute {
            assert!(limiter.check_auth(first));
        }

        assert!(!limiter.check_auth(same_prefix));
        assert!(limiter.check_auth(other_prefix));
    }

    #[test]
    fn ipv4_mapped_ipv6_shares_the_ipv4_bucket() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().expect("mapped ipv6");

        for _ in 0..config.rate_limit.auth_per_client_per_minute {
            assert!(limiter.check_auth(v4));
        }

        assert!(!limiter.check_auth(mapped));
    }

    #[test]
    fn username_limiter_is_keyed_by_username() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);

        for _ in 0..config.rate_limit.auth_per_username_per_minute {
            assert!(limiter.check_auth_username("alice"));
        }

        assert!(!limiter.check_auth_username("alice"));
        assert!(limiter.check_auth_username("bob"));
    }

    #[test]
    fn api_user_limiter_is_keyed_by_user_id() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);
        let user = uuid::Uuid::now_v7();
        let other_user = uuid::Uuid::now_v7();

        for _ in 0..config.rate_limit.api_per_user_per_minute {
            assert!(limiter.check_api_user(user));
        }

        assert!(!limiter.check_api_user(user));
        assert!(limiter.check_api_user(other_user));
    }

    #[test]
    fn ws_ticket_limiter_blocks_after_per_user_quota() {
        let config = ServerConfig::default();
        let limiter = RateLimiter::new(&config.rate_limit);
        let user = uuid::Uuid::now_v7();

        for _ in 0..config.rate_limit.ws_tickets_per_user_per_minute {
            assert!(limiter.check_ws_ticket_user(user));
        }

        assert!(!limiter.check_ws_ticket_user(user));
    }

    #[test]
    fn client_ip_ignores_forwarded_headers_from_untrusted_peer() {
        let headers = header_map(X_FORWARDED_FOR, "198.51.100.7");
        let peer_ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

        assert_eq!(
            client_ip_from_headers(&headers, peer(peer_ip), &TrustedProxies::default()),
            peer_ip
        );
    }

    #[test]
    fn client_ip_uses_forwarded_headers_from_trusted_proxy() {
        let headers = header_map(X_FORWARDED_FOR, "198.51.100.7");
        let proxy_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        assert_eq!(
            client_ip_from_headers(&headers, peer(proxy_ip), &trusted(&["10.0.0.1"])),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))
        );
    }

    #[test]
    fn client_ip_uses_rightmost_untrusted_forwarded_for_entry() {
        let headers = header_map(X_FORWARDED_FOR, "192.0.2.1, 198.51.100.7, 10.0.0.2");
        let proxy_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        assert_eq!(
            client_ip_from_headers(&headers, peer(proxy_ip), &trusted(&["10.0.0.0/24"])),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))
        );
    }

    #[test]
    fn client_ip_can_parse_forwarded_header() {
        let headers = header_map(header::FORWARDED.as_str(), "for=\"[2001:db8::1]:1234\"");
        let proxy_ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        assert_eq!(
            client_ip_from_headers(&headers, peer(proxy_ip), &trusted(&["10.0.0.1"])),
            IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().expect("ipv6"))
        );
    }
}
