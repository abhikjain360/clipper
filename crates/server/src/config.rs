use std::{net::IpAddr, path::PathBuf};

use clap::Args;
use clipper_core::crypto::Argon2Params;
use garde::Validate;
use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use serde::Deserialize;

const DEFAULT_CONFIG: ConfigDefaults = ConfigDefaults {
    server: ServerDefaults {
        data_dir: ".clipper-server",
        addr: "127.0.0.1:8787",
        trusted_proxies: &[],
    },
    rate_limit: RateLimitConfig {
        auth_per_client_per_minute: 10,
        auth_global_per_minute: 600,
        prune_interval_secs: 60,
    },
    auth: AuthConfig {
        challenge_ttl_secs: 5 * 60,
        max_pending_challenges: 4096,
    },
    limits: LimitsConfig {
        max_file_blob_bytes: 512 * 1024 * 1024,
        max_file_meta_ciphertext_bytes: 64 * 1024,
        max_object_meta_ciphertext_bytes: 64 * 1024,
    },
    clipboard: ClipboardConfig {
        ttl_days: 7,
        max_items: 100,
    },
    list: ListConfig {
        default_limit: 100,
        max_limit: 500,
    },
    cleanup: CleanupConfig {
        interval_secs: 60 * 60,
        event_log_retention_days: 3,
        orphan_upload_ttl_secs: 60 * 60,
    },
    crypto: CryptoConfig {
        access_key_hash_params: Argon2Params {
            m_cost: 19 * 1024,
            t_cost: 2,
            p_cost: 1,
        },
        encryption_params: Argon2Params {
            m_cost: 65536, // 64 MiB
            t_cost: 3,
            p_cost: 1,
        },
        access_key_hash_salt_bytes: 16,
        encryption_salt_bytes: 16,
        session_token_bytes: 32,
    },
};

struct ConfigDefaults {
    server: ServerDefaults,
    rate_limit: RateLimitConfig,
    auth: AuthConfig,
    limits: LimitsConfig,
    clipboard: ClipboardConfig,
    list: ListConfig,
    cleanup: CleanupConfig,
    crypto: CryptoConfig,
}

struct ServerDefaults {
    data_dir: &'static str,
    addr: &'static str,
    trusted_proxies: &'static [&'static str],
}

macro_rules! apply_nested_overrides {
    ($target:expr, $overrides:expr, [$($field:ident),+ $(,)?]) => {
        $(
            $target.$field.apply_overrides($overrides.$field);
        )+
    };
}

macro_rules! apply_option_overrides {
    ($target:expr, $overrides:expr, [$($field:ident),+ $(,)?]) => {
        $(
            if let Some(value) = $overrides.$field {
                $target.$field = value;
            }
        )+
    };
}

#[derive(Debug, Clone, Validate)]
pub struct ServerConfig {
    #[garde(dive)]
    pub server: ServerSection,
    #[garde(dive)]
    pub rate_limit: RateLimitConfig,
    #[garde(dive)]
    pub auth: AuthConfig,
    #[garde(dive)]
    pub limits: LimitsConfig,
    #[garde(dive)]
    pub clipboard: ClipboardConfig,
    #[garde(dive)]
    pub list: ListConfig,
    #[garde(dive)]
    pub cleanup: CleanupConfig,
    #[garde(dive)]
    pub crypto: CryptoConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server: ServerSection {
                data_dir: DEFAULT_CONFIG.server.data_dir.into(),
                addr: DEFAULT_CONFIG.server.addr.into(),
                trusted_proxies: DEFAULT_CONFIG
                    .server
                    .trusted_proxies
                    .iter()
                    .map(|proxy| parse_trusted_proxy(proxy).expect("valid default trusted proxy"))
                    .collect(),
            },
            rate_limit: DEFAULT_CONFIG.rate_limit.clone(),
            auth: DEFAULT_CONFIG.auth.clone(),
            limits: DEFAULT_CONFIG.limits.clone(),
            clipboard: DEFAULT_CONFIG.clipboard.clone(),
            list: DEFAULT_CONFIG.list.clone(),
            cleanup: DEFAULT_CONFIG.cleanup.clone(),
            crypto: DEFAULT_CONFIG.crypto.clone(),
        }
    }
}

impl ServerConfig {
    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        apply_nested_overrides!(
            self,
            overrides,
            [
                server, rate_limit, auth, limits, clipboard, list, cleanup, crypto
            ]
        );
    }

    pub fn validate_config(&self) -> Result<(), String> {
        self.validate().map_err(|report| report.to_string())
    }
}

#[derive(Debug, Clone, Validate)]
pub struct ServerSection {
    #[garde(skip)]
    pub data_dir: PathBuf,
    #[garde(length(min = 1))]
    pub addr: String,
    #[garde(skip)]
    pub trusted_proxies: Vec<IpNet>,
}

impl ServerSection {
    fn apply_overrides(&mut self, overrides: ServerSectionOverrides) {
        apply_option_overrides!(self, overrides, [data_dir, addr]);
        if !overrides.trusted_proxies.is_empty() {
            self.trusted_proxies = overrides.trusted_proxies;
        }
    }
}

#[derive(Debug, Clone, Validate)]
pub struct RateLimitConfig {
    #[garde(range(min = 1))]
    pub auth_per_client_per_minute: u32,
    #[garde(range(min = 1))]
    pub auth_global_per_minute: u32,
    #[garde(range(min = 1))]
    pub prune_interval_secs: u64,
}

impl RateLimitConfig {
    fn apply_overrides(&mut self, overrides: RateLimitConfigOverrides) {
        apply_option_overrides!(
            self,
            overrides,
            [
                auth_per_client_per_minute,
                auth_global_per_minute,
                prune_interval_secs
            ]
        );
    }
}

#[derive(Debug, Clone, Validate)]
pub struct AuthConfig {
    #[garde(range(min = 1))]
    pub challenge_ttl_secs: u64,
    #[garde(range(min = 1))]
    pub max_pending_challenges: usize,
}

impl AuthConfig {
    fn apply_overrides(&mut self, overrides: AuthConfigOverrides) {
        apply_option_overrides!(
            self,
            overrides,
            [challenge_ttl_secs, max_pending_challenges]
        );
    }
}

#[derive(Debug, Clone, Validate)]
pub struct LimitsConfig {
    #[garde(custom(validate_max_file_blob_bytes))]
    pub max_file_blob_bytes: u64,
    #[garde(range(min = 1))]
    pub max_file_meta_ciphertext_bytes: usize,
    #[garde(range(min = 1))]
    pub max_object_meta_ciphertext_bytes: usize,
}

impl LimitsConfig {
    fn apply_overrides(&mut self, overrides: LimitsConfigOverrides) {
        apply_option_overrides!(
            self,
            overrides,
            [
                max_file_blob_bytes,
                max_file_meta_ciphertext_bytes,
                max_object_meta_ciphertext_bytes
            ]
        );
    }
}

#[derive(Debug, Clone, Validate)]
pub struct ClipboardConfig {
    #[garde(range(min = 1))]
    pub ttl_days: i64,
    #[garde(range(min = 1))]
    pub max_items: u64,
}

impl ClipboardConfig {
    fn apply_overrides(&mut self, overrides: ClipboardConfigOverrides) {
        apply_option_overrides!(self, overrides, [ttl_days, max_items]);
    }
}

#[derive(Debug, Clone, Validate)]
pub struct ListConfig {
    #[garde(range(min = 1))]
    pub default_limit: u64,
    #[garde(range(min = 1), custom(validate_list_max_limit(self.default_limit)))]
    pub max_limit: u64,
}

impl ListConfig {
    fn apply_overrides(&mut self, overrides: ListConfigOverrides) {
        apply_option_overrides!(self, overrides, [default_limit, max_limit]);
    }
}

#[derive(Debug, Clone, Validate)]
pub struct CleanupConfig {
    #[garde(range(min = 1))]
    pub interval_secs: u64,
    #[garde(range(min = 1))]
    pub event_log_retention_days: i64,
    #[garde(range(min = 1), custom(validate_chrono_seconds))]
    pub orphan_upload_ttl_secs: u64,
}

impl CleanupConfig {
    fn apply_overrides(&mut self, overrides: CleanupConfigOverrides) {
        apply_option_overrides!(
            self,
            overrides,
            [
                interval_secs,
                event_log_retention_days,
                orphan_upload_ttl_secs
            ]
        );
    }
}

#[derive(Debug, Clone, Validate)]
pub struct CryptoConfig {
    #[garde(dive)]
    pub access_key_hash_params: Argon2Params,
    #[garde(dive)]
    pub encryption_params: Argon2Params,
    #[garde(range(min = 1))]
    pub access_key_hash_salt_bytes: usize,
    #[garde(range(min = 1))]
    pub encryption_salt_bytes: usize,
    #[garde(range(min = 1))]
    pub session_token_bytes: usize,
}

impl CryptoConfig {
    fn apply_overrides(&mut self, overrides: CryptoConfigOverrides) {
        if let Some(value) = overrides.access_key_hash_m_cost {
            self.access_key_hash_params.m_cost = value;
        }
        if let Some(value) = overrides.access_key_hash_t_cost {
            self.access_key_hash_params.t_cost = value;
        }
        if let Some(value) = overrides.access_key_hash_p_cost {
            self.access_key_hash_params.p_cost = value;
        }

        if let Some(value) = overrides.encryption_m_cost {
            self.encryption_params.m_cost = value;
        }
        if let Some(value) = overrides.encryption_t_cost {
            self.encryption_params.t_cost = value;
        }
        if let Some(value) = overrides.encryption_p_cost {
            self.encryption_params.p_cost = value;
        }

        if let Some(value) = overrides.access_key_hash_salt_bytes {
            self.access_key_hash_salt_bytes = value;
        }
        if let Some(value) = overrides.encryption_salt_bytes {
            self.encryption_salt_bytes = value;
        }
        if let Some(value) = overrides.session_token_bytes {
            self.session_token_bytes = value;
        }
    }
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigOverrides {
    #[command(flatten)]
    pub server: ServerSectionOverrides,
    #[command(flatten)]
    pub rate_limit: RateLimitConfigOverrides,
    #[command(flatten)]
    pub auth: AuthConfigOverrides,
    #[command(flatten)]
    pub limits: LimitsConfigOverrides,
    #[command(flatten)]
    pub clipboard: ClipboardConfigOverrides,
    #[command(flatten)]
    pub list: ListConfigOverrides,
    #[command(flatten)]
    pub cleanup: CleanupConfigOverrides,
    #[command(flatten)]
    pub crypto: CryptoConfigOverrides,
}

impl ConfigOverrides {
    pub fn from_env() -> Result<Self, String> {
        let mut overrides = Self::default();
        if let Ok(value) = std::env::var("CLIPPER_TRUSTED_PROXIES") {
            for proxy in value
                .split(',')
                .map(str::trim)
                .filter(|proxy| !proxy.is_empty())
            {
                overrides
                    .server
                    .trusted_proxies
                    .push(parse_trusted_proxy(proxy)?);
            }
        }
        Ok(overrides)
    }
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSectionOverrides {
    /// Directory containing the SQLite database and blob storage.
    #[arg(long, short = 'd', value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Address the HTTP server binds to.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,
    /// Trust Forwarded/X-Forwarded-For/X-Real-IP only from these proxy IPs or CIDR ranges.
    #[arg(
        long = "trusted-proxy",
        value_name = "IP_OR_CIDR",
        value_delimiter = ',',
        value_parser = parse_trusted_proxy
    )]
    #[serde(deserialize_with = "deserialize_trusted_proxies")]
    pub trusted_proxies: Vec<IpNet>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfigOverrides {
    /// Auth requests allowed per resolved client IP per minute.
    #[arg(long = "auth-rate-limit-per-client-per-minute")]
    pub auth_per_client_per_minute: Option<u32>,
    /// Auth requests allowed globally per minute.
    #[arg(long = "auth-rate-limit-global-per-minute")]
    pub auth_global_per_minute: Option<u32>,
    /// How often stale per-client auth limiter state is pruned.
    #[arg(long = "rate-limit-prune-interval-secs")]
    pub prune_interval_secs: Option<u64>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfigOverrides {
    /// OPAQUE challenge and pending registration lifetime.
    #[arg(long = "auth-challenge-ttl-secs")]
    pub challenge_ttl_secs: Option<u64>,
    /// Maximum in-memory OPAQUE challenges and pending registrations.
    #[arg(long = "auth-max-pending-challenges")]
    pub max_pending_challenges: Option<usize>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfigOverrides {
    /// Maximum encrypted file blob size.
    #[arg(long = "max-file-blob-bytes")]
    pub max_file_blob_bytes: Option<u64>,
    /// Maximum encrypted file metadata size after base64 decoding.
    #[arg(long = "max-file-meta-ciphertext-bytes")]
    pub max_file_meta_ciphertext_bytes: Option<usize>,
    /// Maximum encrypted generic object metadata size.
    #[arg(long = "max-object-meta-ciphertext-bytes")]
    pub max_object_meta_ciphertext_bytes: Option<usize>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfigOverrides {
    /// Server-side encrypted clipboard item retention.
    #[arg(long = "clipboard-ttl-days")]
    pub ttl_days: Option<i64>,
    /// Maximum clipboard items retained per user; oldest beyond this are trimmed.
    #[arg(long = "clipboard-max-items")]
    pub max_items: Option<u64>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListConfigOverrides {
    /// Default item count for list endpoints when no limit is requested.
    #[arg(long = "list-default-limit")]
    pub default_limit: Option<u64>,
    /// Maximum item count accepted by list endpoints.
    #[arg(long = "list-max-limit")]
    pub max_limit: Option<u64>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CleanupConfigOverrides {
    /// How often background cleanup runs.
    #[arg(long = "cleanup-interval-secs")]
    pub interval_secs: Option<u64>,
    /// Event log retention window.
    #[arg(long = "event-log-retention-days")]
    pub event_log_retention_days: Option<i64>,
    /// How long incomplete file uploads may remain before cleanup.
    #[arg(long = "orphan-upload-ttl-secs")]
    pub orphan_upload_ttl_secs: Option<u64>,
}

#[derive(Args, Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CryptoConfigOverrides {
    /// Argon2 m-cost for access-key hashing.
    #[arg(long = "access-key-hash-m-cost")]
    pub access_key_hash_m_cost: Option<u32>,
    /// Argon2 t-cost for access-key hashing.
    #[arg(long = "access-key-hash-t-cost")]
    pub access_key_hash_t_cost: Option<u32>,
    /// Argon2 p-cost for access-key hashing.
    #[arg(long = "access-key-hash-p-cost")]
    pub access_key_hash_p_cost: Option<u32>,
    /// Argon2 m-cost for client-side encryption key derivation.
    #[arg(long = "encryption-m-cost")]
    pub encryption_m_cost: Option<u32>,
    /// Argon2 t-cost for client-side encryption key derivation.
    #[arg(long = "encryption-t-cost")]
    pub encryption_t_cost: Option<u32>,
    /// Argon2 p-cost for client-side encryption key derivation.
    #[arg(long = "encryption-p-cost")]
    pub encryption_p_cost: Option<u32>,
    /// Access-key hash salt size in bytes.
    #[arg(long = "access-key-hash-salt-bytes")]
    pub access_key_hash_salt_bytes: Option<usize>,
    /// Encryption salt size in bytes.
    #[arg(long = "encryption-salt-bytes")]
    pub encryption_salt_bytes: Option<usize>,
    /// Session token size in bytes.
    #[arg(long = "session-token-bytes")]
    pub session_token_bytes: Option<usize>,
}

pub fn parse_trusted_proxy(value: &str) -> Result<IpNet, String> {
    if let Ok(network) = value.parse::<IpNet>() {
        return Ok(network);
    }

    let ip = value
        .parse::<IpAddr>()
        .map_err(|_| format!("expected IP address or CIDR network, got `{value}`"))?;

    match ip {
        IpAddr::V4(ip) => Ipv4Net::new(ip, 32).map(IpNet::V4),
        IpAddr::V6(ip) => Ipv6Net::new(ip, 128).map(IpNet::V6),
    }
    .map_err(|error| error.to_string())
}

fn deserialize_trusted_proxies<'de, D>(deserializer: D) -> Result<Vec<IpNet>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|proxy| parse_trusted_proxy(&proxy).map_err(serde::de::Error::custom))
        .collect()
}

fn validate_max_file_blob_bytes(value: &u64, _: &()) -> garde::Result {
    if *value == 0 {
        return Err(garde::Error::new("must be greater than zero"));
    }
    if *value > i64::MAX as u64 {
        return Err(garde::Error::new(
            "must fit in a signed 64-bit integer for database storage",
        ));
    }
    Ok(())
}

fn validate_chrono_seconds(value: &u64, _: &()) -> garde::Result {
    if *value > i64::MAX as u64 {
        return Err(garde::Error::new(
            "must fit in a signed 64-bit integer for chrono duration conversion",
        ));
    }
    Ok(())
}

fn validate_list_max_limit(default_limit: u64) -> impl FnOnce(&u64, &()) -> garde::Result {
    move |max_limit, _| {
        if default_limit > *max_limit {
            return Err(garde::Error::new(
                "must be greater than or equal to list.default_limit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_overrides_defaults() {
        let overrides: ConfigOverrides = toml::from_str(
            r#"
                [server]
                data_dir = "/tmp/clipper"
                addr = "0.0.0.0:8787"
                trusted_proxies = ["127.0.0.1", "10.0.0.0/24"]

                [rate_limit]
                auth_per_client_per_minute = 20
                auth_global_per_minute = 900
                prune_interval_secs = 30

                [auth]
                challenge_ttl_secs = 120
                max_pending_challenges = 128

                [limits]
                max_file_blob_bytes = 1024
                max_file_meta_ciphertext_bytes = 2048
                max_object_meta_ciphertext_bytes = 4096

                [clipboard]
                ttl_days = 2
                max_items = 25

                [list]
                default_limit = 10
                max_limit = 50

                [cleanup]
                interval_secs = 15
                event_log_retention_days = 4
                orphan_upload_ttl_secs = 20

                [crypto]
                access_key_hash_m_cost = 4096
                access_key_hash_t_cost = 1
                access_key_hash_p_cost = 2
                encryption_m_cost = 32768
                encryption_t_cost = 1
                encryption_p_cost = 1
                access_key_hash_salt_bytes = 32
                encryption_salt_bytes = 24
                session_token_bytes = 48
            "#,
        )
        .expect("config TOML");
        let mut config = ServerConfig::default();
        config.apply_overrides(overrides);
        config.validate_config().expect("valid config");

        assert_eq!(config.server.data_dir, PathBuf::from("/tmp/clipper"));
        assert_eq!(config.server.addr, "0.0.0.0:8787");
        assert_eq!(config.server.trusted_proxies.len(), 2);
        assert_eq!(config.rate_limit.auth_per_client_per_minute, 20);
        assert_eq!(config.rate_limit.auth_global_per_minute, 900);
        assert_eq!(config.rate_limit.prune_interval_secs, 30);
        assert_eq!(config.auth.challenge_ttl_secs, 120);
        assert_eq!(config.auth.max_pending_challenges, 128);
        assert_eq!(config.limits.max_file_blob_bytes, 1024);
        assert_eq!(config.limits.max_file_meta_ciphertext_bytes, 2048);
        assert_eq!(config.limits.max_object_meta_ciphertext_bytes, 4096);
        assert_eq!(config.clipboard.ttl_days, 2);
        assert_eq!(config.clipboard.max_items, 25);
        assert_eq!(config.list.default_limit, 10);
        assert_eq!(config.list.max_limit, 50);
        assert_eq!(config.cleanup.interval_secs, 15);
        assert_eq!(config.cleanup.event_log_retention_days, 4);
        assert_eq!(config.cleanup.orphan_upload_ttl_secs, 20);
        assert_eq!(config.crypto.access_key_hash_params.m_cost, 4096);
        assert_eq!(config.crypto.access_key_hash_params.t_cost, 1);
        assert_eq!(config.crypto.access_key_hash_params.p_cost, 2);
        assert_eq!(config.crypto.encryption_params.m_cost, 32768);
        assert_eq!(config.crypto.encryption_params.t_cost, 1);
        assert_eq!(config.crypto.encryption_params.p_cost, 1);
        assert_eq!(config.crypto.access_key_hash_salt_bytes, 32);
        assert_eq!(config.crypto.encryption_salt_bytes, 24);
        assert_eq!(config.crypto.session_token_bytes, 48);
    }

    #[test]
    fn validation_rejects_zero_rate_limit() {
        let mut config = ServerConfig::default();
        config.rate_limit.auth_per_client_per_minute = 0;

        assert!(config.validate_config().is_err());
    }

    #[test]
    fn toml_allows_partial_config() {
        let overrides: ConfigOverrides = toml::from_str(
            r#"
                [server]
                data_dir = "/tmp/clipper"
            "#,
        )
        .expect("partial config TOML");
        let mut config = ServerConfig::default();
        config.apply_overrides(overrides);
        config.validate_config().expect("valid config");

        assert_eq!(config.server.data_dir, PathBuf::from("/tmp/clipper"));
        assert_eq!(config.server.addr, DEFAULT_CONFIG.server.addr);
        assert!(config.server.trusted_proxies.is_empty());
    }
}
