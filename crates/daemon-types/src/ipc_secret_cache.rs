//! Process-lifetime cache for the local IPC secret.
//!
//! Both sides of the daemon socket authenticate with a shared secret held in the
//! platform credential store. On macOS that store is the keychain, and every
//! read is a Keychain Services call that can raise an authorization prompt — so
//! reading it per connection (the daemon authenticates each client; the app
//! retries the handshake on a backoff) asks the user to unlock the keychain
//! several times on a single launch.
//!
//! The secret does not change under a running process, so each process reads it
//! once and holds it. That trades a longer in-memory lifetime for the secret —
//! it was already resident for the duration of every handshake — against a
//! prompt per connection attempt.
//!
//! A secret rotated behind a running process's back (deleting the keychain item
//! by hand) is not picked up until restart. That is the right way round:
//! rotation is manual surgery, prompts were every launch.

use std::sync::Mutex;

use zeroize::Zeroizing;

/// Where a process keeps its copy of the secret. Declare one per process as a
/// `static` and pass it to [`cached_secret`].
pub type IpcSecretCache = Mutex<Option<Zeroizing<Vec<u8>>>>;

/// An empty cache, for initialising a `static IpcSecretCache`.
pub const fn empty_cache() -> IpcSecretCache {
    Mutex::new(None)
}

/// Return the cached secret, calling `load` exactly once to populate it.
///
/// The lock is held across `load` on purpose: that is what makes the read happen
/// once rather than once per racing caller. Two connections arriving together
/// would otherwise both find the cache empty and both hit the store — which on
/// macOS means two prompts. A failed `load` leaves the cache empty so a later
/// caller can retry.
pub fn cached_secret<E>(
    cache: &IpcSecretCache,
    load: impl FnOnce() -> Result<Zeroizing<Vec<u8>>, E>,
) -> Result<Zeroizing<Vec<u8>>, E> {
    let mut cached = cache.lock().expect("IPC secret cache lock poisoned");
    if let Some(secret) = cached.as_deref() {
        return Ok(Zeroizing::new(secret.to_vec()));
    }
    let secret = load()?;
    // Hand back a copy rather than the cached value: `Zeroizing` is not `Clone`,
    // and the caller's copy is zeroized when it drops.
    let copy = Zeroizing::new(secret.to_vec());
    *cached = Some(secret);
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    #[test]
    fn the_store_is_read_once_even_when_callers_race() {
        static CACHE: IpcSecretCache = empty_cache();
        static READS: AtomicUsize = AtomicUsize::new(0);

        let load = || -> Result<Zeroizing<Vec<u8>>, Infallible> {
            READS.fetch_add(1, Ordering::SeqCst);
            // Long enough that the other threads are all waiting on the lock
            // before the first read finishes — the window in which an
            // unsynchronised cache reads the store more than once.
            std::thread::sleep(Duration::from_millis(50));
            Ok(Zeroizing::new(vec![7u8; 32]))
        };

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let secret = cached_secret(&CACHE, load).expect("infallible");
                    assert_eq!(&secret[..], &[7u8; 32]);
                });
            }
        });

        assert_eq!(
            READS.load(Ordering::SeqCst),
            1,
            "eight racing callers must produce exactly one store read",
        );
    }

    #[test]
    fn a_failed_load_leaves_the_cache_empty_so_it_can_be_retried() {
        static CACHE: IpcSecretCache = empty_cache();
        static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

        let load = || -> Result<Zeroizing<Vec<u8>>, &'static str> {
            if ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err("store unavailable");
            }
            Ok(Zeroizing::new(vec![1u8; 32]))
        };

        assert_eq!(cached_secret(&CACHE, load), Err("store unavailable"));
        let secret = cached_secret(&CACHE, load).expect("second attempt succeeds");
        assert_eq!(&secret[..], &[1u8; 32]);
        assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 2);
    }
}
