use std::{
    net::{IpAddr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware::Next,
    response::Response,
};
use clipper_core::models::ApiErrorCode;
use governor::{DefaultDirectRateLimiter, DefaultKeyedRateLimiter, Quota, RateLimiter as Governor};
use ipnet::IpNet;

const X_FORWARDED_FOR: &str = "x-forwarded-for";
const X_REAL_IP: &str = "x-real-ip";

use crate::{
    config::RateLimitConfig,
    routes::{ApiError, error_response},
};

pub struct RateLimiter {
    auth_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    auth_global: DefaultDirectRateLimiter,
}

impl RateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            auth_by_ip: Governor::keyed(per_minute_quota(config.auth_per_client_per_minute)),
            auth_global: Governor::direct(per_minute_quota(config.auth_global_per_minute)),
        }
    }

    /// Returns true if the auth request is allowed, false if rate-limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        self.auth_by_ip.check_key(&ip).is_ok() && self.auth_global.check().is_ok()
    }

    /// Prune stale per-client limiter state. Call periodically.
    pub fn prune(&self) {
        self.auth_by_ip.retain_recent();
        self.auth_by_ip.shrink_to_fit();
    }
}

pub async fn auth_rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    ClientIp(ip): ClientIp,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !limiter.check(ip) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ApiErrorCode::RateLimited,
            "Too many requests",
        ));
    }

    req.extensions_mut().insert(ClientIp(ip));
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
            assert!(limiter.check(ip));
        }

        assert!(!limiter.check(ip));
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
