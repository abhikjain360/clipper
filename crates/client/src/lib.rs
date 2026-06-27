pub mod api_client;
mod clipboard_privacy;
pub mod engine;
mod local_store;

#[cfg(target_os = "macos")]
#[path = "clipboard_watcher_macos.rs"]
pub mod clipboard_watcher;

#[cfg(target_os = "linux")]
#[path = "clipboard_watcher_linux.rs"]
pub mod clipboard_watcher;

/// Install the `ring` [`rustls::crypto::CryptoProvider`] as the process
/// default for every native TLS consumer in this crate (reqwest via
/// `use_preconfigured_tls`, and tokio-tungstenite's internal `ClientConfig`).
///
/// rustls's default provider is aws-lc-rs, which aborts inside aws-lc's CPU
/// jitter-entropy self-test when cross-compiled for Android, taking the app
/// down on the first TLS connection (login). ring relies on the OS CSPRNG and
/// has no such path. Idempotent: safe to call from every connection-setup
/// path, and a competing install (already set) is ignored.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn ensure_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            tracing::debug!("rustls crypto provider already installed");
        }
    });
}
