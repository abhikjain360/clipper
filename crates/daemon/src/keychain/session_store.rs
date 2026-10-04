use super::{CredentialStore, Credentials, KeychainError, KeychainResult};

const MISSING_ENTITLEMENT: i32 = -34018;
const UNSIGNED_CODE: i32 = -67062;
const INVALID_SIGNATURE: i32 = -67061;
const FAILED_SIGNING_REQUIREMENT: i32 = -67050;
const CHANGED_CODE: i32 = -67034;

fn allows_login_keychain(error: &KeychainError) -> bool {
    matches!(
        error,
        KeychainError::Platform {
            status: MISSING_ENTITLEMENT
                | UNSIGNED_CODE
                | INVALID_SIGNATURE
                | FAILED_SIGNING_REQUIREMENT
                | CHANGED_CODE,
            ..
        }
    )
}

pub fn store(
    protected: &dyn CredentialStore,
    login: &dyn CredentialStore,
    credentials: &Credentials,
) -> KeychainResult<()> {
    match protected.store(credentials) {
        Ok(()) => {
            tracing::info!(
                store = "macOS data protection keychain",
                "Saved session credentials"
            );
            if let Err(error) = login.clear() {
                tracing::warn!(%error, "Failed to remove superseded login-keychain credentials");
            }
            Ok(())
        }
        Err(error) if allows_login_keychain(&error) => {
            tracing::warn!(%error, "Data protection keychain unavailable for this signing identity; using the login keychain");
            login.store(credentials)?;
            tracing::info!(store = "macOS login keychain", "Saved session credentials");
            if let Err(error) = protected.clear() {
                tracing::warn!(%error, "Failed to remove superseded data-protection-keychain credentials");
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn load(
    protected: &dyn CredentialStore,
    login: &dyn CredentialStore,
) -> KeychainResult<Option<Credentials>> {
    let protected_error = match protected.load() {
        Ok(Some(credentials)) => {
            tracing::info!(
                store = "macOS data protection keychain",
                "Loaded session credentials"
            );
            return Ok(Some(credentials));
        }
        Ok(None) => None,
        Err(error) if allows_login_keychain(&error) => {
            tracing::warn!(%error, "Data protection keychain unavailable for this signing identity; checking the login keychain");
            Some(error)
        }
        Err(error) => return Err(error),
    };
    match login.load()? {
        Some(credentials) => {
            tracing::info!(store = "macOS login keychain", "Loaded session credentials");
            Ok(Some(credentials))
        }
        None => match protected_error {
            Some(error) => Err(error),
            None => Ok(None),
        },
    }
}

pub fn clear(protected: &dyn CredentialStore, login: &dyn CredentialStore) -> KeychainResult<()> {
    let protected = protected.clear();
    let login = login.clear();
    match &protected {
        Ok(()) => tracing::info!(
            store = "macOS data protection keychain",
            "Deleted saved session credentials"
        ),
        Err(error) => {
            tracing::warn!(%error, store = "macOS data protection keychain", "Failed to delete saved session credentials")
        }
    }
    match &login {
        Ok(()) => tracing::info!(
            store = "macOS login keychain",
            "Deleted saved session credentials"
        ),
        Err(error) => {
            tracing::warn!(%error, store = "macOS login keychain", "Failed to delete saved session credentials")
        }
    }
    protected.and(login)
}

#[cfg(all(target_os = "macos", not(test)))]
pub enum MacKeychain {
    Protected,
    Login,
}

#[cfg(all(target_os = "macos", not(test)))]
impl MacKeychain {
    fn options(&self) -> security_framework::passwords::PasswordOptions {
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            super::SERVICE,
            super::ACCOUNT,
        );
        if matches!(self, Self::Protected) {
            options.use_protected_keychain();
            options.set_access_synchronized(Some(false));
        }
        options
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn platform_error(error: security_framework::base::Error) -> KeychainError {
    KeychainError::Platform {
        status: error.code(),
        message: error.to_string(),
    }
}

#[cfg(all(target_os = "macos", not(test)))]
impl CredentialStore for MacKeychain {
    fn supports_resume(&self) -> bool {
        true
    }

    fn load(&self) -> KeychainResult<Option<Credentials>> {
        match security_framework::passwords::generic_password(self.options()) {
            Ok(data) => {
                let data = zeroize::Zeroizing::new(data);
                serde_json::from_slice(&data)
                    .map(Some)
                    .map_err(KeychainError::Decode)
            }
            Err(error) if error.code() == super::ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(platform_error(error)),
        }
    }

    fn store(&self, credentials: &Credentials) -> KeychainResult<()> {
        use security_framework::access_control::{ProtectionMode, SecAccessControl};

        let json = zeroize::Zeroizing::new(
            serde_json::to_string(credentials).map_err(KeychainError::Encode)?,
        );
        let mut options = self.options();
        if matches!(self, Self::Protected) {
            let access = SecAccessControl::create_with_protection(
                Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
                0,
            )
            .map_err(platform_error)?;
            options.set_access_control(access);
        }
        security_framework::passwords::set_generic_password_options(json.as_bytes(), options)
            .map_err(platform_error)
    }

    fn clear(&self) -> KeychainResult<()> {
        match security_framework::passwords::delete_generic_password_options(self.options()) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == super::ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(platform_error(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        credentials: Mutex<Option<Credentials>>,
        read_error: Option<i32>,
        write_error: Option<i32>,
        delete_error: Option<i32>,
        calls: Mutex<Vec<&'static str>>,
    }

    fn error(status: i32) -> KeychainError {
        KeychainError::Platform {
            status,
            message: "fake keychain error".into(),
        }
    }

    impl CredentialStore for FakeStore {
        fn supports_resume(&self) -> bool {
            true
        }

        fn load(&self) -> KeychainResult<Option<Credentials>> {
            self.calls.lock().unwrap().push("load");
            match self.read_error {
                Some(status) => Err(error(status)),
                None => Ok(self.credentials.lock().unwrap().clone()),
            }
        }

        fn store(&self, credentials: &Credentials) -> KeychainResult<()> {
            self.calls.lock().unwrap().push("store");
            if let Some(status) = self.write_error {
                return Err(error(status));
            }
            *self.credentials.lock().unwrap() = Some(credentials.clone());
            Ok(())
        }

        fn clear(&self) -> KeychainResult<()> {
            self.calls.lock().unwrap().push("clear");
            if let Some(status) = self.delete_error {
                return Err(error(status));
            }
            *self.credentials.lock().unwrap() = None;
            Ok(())
        }
    }

    fn credentials() -> Credentials {
        Credentials {
            username: "alice".into(),
            device_name: "Test Mac".into(),
            server_url: "https://test.example".into(),
            session: Some(super::super::StoredSession {
                token: zeroize::Zeroizing::new("saved-token".into()),
                data_key: zeroize::Zeroizing::new([7; 32]),
                device_identity_wrapping_key: zeroize::Zeroizing::new([8; 32]),
                last_confirmed_at: 1_000,
            }),
        }
    }

    fn assert_credentials(actual: Option<Credentials>, expected: &Credentials) {
        assert!(actual.as_ref() == Some(expected));
    }

    #[test]
    fn entitled_build_uses_the_protected_store_and_removes_the_login_copy() {
        let credentials = credentials();
        let protected = FakeStore::default();
        let login = FakeStore::default();
        login.store(&credentials).unwrap();
        store(&protected, &login, &credentials).unwrap();
        assert_credentials(load(&protected, &login).unwrap(), &credentials);
        assert!(login.credentials.lock().unwrap().is_none());
        assert_eq!(*protected.calls.lock().unwrap(), ["store", "load"]);
        assert_eq!(*login.calls.lock().unwrap(), ["store", "clear"]);
    }

    #[test]
    fn ad_hoc_build_saves_and_loads_from_login_after_signing_or_entitlement_errors() {
        for status in [
            MISSING_ENTITLEMENT,
            UNSIGNED_CODE,
            INVALID_SIGNATURE,
            FAILED_SIGNING_REQUIREMENT,
            CHANGED_CODE,
        ] {
            let credentials = credentials();
            let protected = FakeStore {
                read_error: Some(status),
                write_error: Some(status),
                delete_error: Some(status),
                ..Default::default()
            };
            let login = FakeStore::default();
            store(&protected, &login, &credentials).unwrap();
            assert_credentials(load(&protected, &login).unwrap(), &credentials);
            assert_eq!(*protected.calls.lock().unwrap(), ["store", "clear", "load"]);
            assert_eq!(*login.calls.lock().unwrap(), ["store", "load"]);
        }
    }

    #[test]
    fn login_credentials_are_read_without_rewriting_them_when_protected_item_is_absent() {
        let credentials = credentials();
        let protected = FakeStore::default();
        let login = FakeStore::default();
        login.store(&credentials).unwrap();
        assert_credentials(load(&protected, &login).unwrap(), &credentials);
        assert_eq!(*login.calls.lock().unwrap(), ["store", "load"]);
    }

    #[test]
    fn locked_denied_or_broken_protected_store_does_not_downgrade() {
        for status in [-25308, -25293, -25299, -50] {
            let protected = FakeStore {
                read_error: Some(status),
                write_error: Some(status),
                ..Default::default()
            };
            let login = FakeStore::default();
            assert!(store(&protected, &login, &credentials()).is_err());
            assert!(load(&protected, &login).is_err());
            assert!(login.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn failed_replacement_does_not_delete_the_existing_credentials() {
        let credentials = credentials();
        let protected = FakeStore {
            write_error: Some(MISSING_ENTITLEMENT),
            ..Default::default()
        };
        *protected.credentials.lock().unwrap() = Some(credentials.clone());
        let login = FakeStore {
            write_error: Some(-25293),
            ..Default::default()
        };
        assert!(store(&protected, &login, &credentials).is_err());
        assert_credentials(protected.credentials.lock().unwrap().clone(), &credentials);
        assert_eq!(*protected.calls.lock().unwrap(), ["store"]);
        assert_eq!(*login.calls.lock().unwrap(), ["store"]);
    }

    #[test]
    fn missing_or_denied_login_item_preserves_the_read_error() {
        let protected = FakeStore {
            read_error: Some(MISSING_ENTITLEMENT),
            ..Default::default()
        };
        let mut login = FakeStore::default();
        assert!(matches!(
            load(&protected, &login),
            Err(KeychainError::Platform {
                status: MISSING_ENTITLEMENT,
                ..
            })
        ));
        login.read_error = Some(-25293);
        assert!(matches!(
            load(&protected, &login),
            Err(KeychainError::Platform { status: -25293, .. })
        ));
    }

    #[test]
    fn logout_deletes_credentials_from_both_stores() {
        let protected = FakeStore::default();
        let login = FakeStore::default();
        protected.store(&credentials()).unwrap();
        login.store(&credentials()).unwrap();
        clear(&protected, &login).unwrap();
        assert!(protected.credentials.lock().unwrap().is_none());
        assert!(login.credentials.lock().unwrap().is_none());
    }

    #[test]
    fn logout_attempts_both_deletions_even_when_either_store_fails() {
        for (protected_error, login_error) in [
            (Some(MISSING_ENTITLEMENT), None),
            (None, Some(-25293)),
            (Some(MISSING_ENTITLEMENT), Some(-25293)),
        ] {
            let protected = FakeStore {
                delete_error: protected_error,
                ..Default::default()
            };
            let login = FakeStore {
                delete_error: login_error,
                ..Default::default()
            };
            assert!(clear(&protected, &login).is_err());
            assert_eq!(*protected.calls.lock().unwrap(), ["clear"]);
            assert_eq!(*login.calls.lock().unwrap(), ["clear"]);
        }
    }
}
