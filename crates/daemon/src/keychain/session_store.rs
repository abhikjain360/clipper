use clipper_daemon_client::data_dir::KeychainNames;

use super::{Credentials, KeychainError, KeychainResult, SessionLocation, StoredProfile};

pub(super) trait KeychainStore {
    fn load(&self, names: &KeychainNames) -> KeychainResult<Option<Credentials>>;
    fn store(&self, names: &KeychainNames, credentials: &Credentials) -> KeychainResult<()>;
    fn clear(&self, names: &KeychainNames) -> KeychainResult<()>;
}

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

pub(super) fn store(
    protected: &dyn KeychainStore,
    login: &dyn KeychainStore,
    names: &KeychainNames,
    credentials: &Credentials,
) -> KeychainResult<SessionLocation> {
    match protected.store(names, credentials) {
        Ok(()) => {
            tracing::info!(
                store = "macOS data protection keychain",
                "Saved session credentials"
            );
            if let Err(error) = login.clear(names) {
                tracing::warn!(%error, "Failed to remove superseded login-keychain credentials");
            }
            Ok(SessionLocation::Protected)
        }
        Err(error) if allows_login_keychain(&error) => {
            tracing::warn!(%error, "Data protection keychain unavailable for this signing identity; using the login keychain");
            login.store(names, credentials)?;
            tracing::info!(store = "macOS login keychain", "Saved session credentials");
            if let Err(error) = protected.clear(names) {
                tracing::warn!(%error, "Failed to remove superseded data-protection-keychain credentials");
            }
            Ok(SessionLocation::Login)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn load(
    protected: &dyn KeychainStore,
    login: &dyn KeychainStore,
    names: &KeychainNames,
    profile: &StoredProfile,
) -> KeychainResult<Option<Credentials>> {
    let mut current = None;
    let mut current_error = None;
    for (location, store) in [
        (SessionLocation::Protected, protected),
        (SessionLocation::Login, login),
    ] {
        match store.load(names) {
            Ok(Some(credentials)) if credentials.matches(profile, location) => {
                tracing::info!(store = ?location, "Loaded current session credentials");
                current = Some(credentials);
            }
            Ok(Some(_)) => {
                tracing::warn!(store = ?location, "Ignoring credentials that do not match the current session record");
                if let Err(error) = store.clear(names) {
                    tracing::warn!(%error, store = ?location, "Failed to delete stale session credentials");
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, store = ?location, "Failed to read saved session credentials");
                if profile
                    .resume
                    .as_ref()
                    .is_some_and(|resume| resume.store == location)
                {
                    current_error = Some(error);
                } else if let Err(error) = store.clear(names) {
                    tracing::warn!(%error, store = ?location, "Failed to delete stale session credentials");
                }
            }
        }
    }
    match current_error {
        Some(error) => Err(error),
        None => Ok(current),
    }
}

pub(super) fn clear(
    protected: &dyn KeychainStore,
    login: &dyn KeychainStore,
    names: &KeychainNames,
) -> KeychainResult<()> {
    let protected = protected.clear(names);
    let login = login.clear(names);
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
    fn options(&self, names: &KeychainNames) -> security_framework::passwords::PasswordOptions {
        let mut options = security_framework::passwords::PasswordOptions::new_generic_password(
            &names.service,
            &names.credentials_account,
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
impl KeychainStore for MacKeychain {
    fn load(&self, names: &KeychainNames) -> KeychainResult<Option<Credentials>> {
        match security_framework::passwords::generic_password(self.options(names)) {
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

    fn store(&self, names: &KeychainNames, credentials: &Credentials) -> KeychainResult<()> {
        use security_framework::access_control::{ProtectionMode, SecAccessControl};

        let json = zeroize::Zeroizing::new(
            serde_json::to_string(credentials).map_err(KeychainError::Encode)?,
        );
        let mut options = self.options(names);
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

    fn clear(&self, names: &KeychainNames) -> KeychainResult<()> {
        match security_framework::passwords::delete_generic_password_options(self.options(names)) {
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

    impl KeychainStore for FakeStore {
        fn load(&self, _names: &KeychainNames) -> KeychainResult<Option<Credentials>> {
            self.calls.lock().unwrap().push("load");
            match self.read_error {
                Some(status) => Err(error(status)),
                None => Ok(self.credentials.lock().unwrap().clone()),
            }
        }

        fn store(&self, _names: &KeychainNames, credentials: &Credentials) -> KeychainResult<()> {
            self.calls.lock().unwrap().push("store");
            if let Some(status) = self.write_error {
                return Err(error(status));
            }
            *self.credentials.lock().unwrap() = Some(credentials.clone());
            Ok(())
        }

        fn clear(&self, _names: &KeychainNames) -> KeychainResult<()> {
            self.calls.lock().unwrap().push("clear");
            if let Some(status) = self.delete_error {
                return Err(error(status));
            }
            *self.credentials.lock().unwrap() = None;
            Ok(())
        }
    }

    fn names() -> KeychainNames {
        KeychainNames {
            service: "com.clipper.daemon".into(),
            credentials_account: "credentials".into(),
            ipc_secret_account: "ipc-secret-v1".into(),
        }
    }

    impl FakeStore {
        fn store(&self, credentials: &Credentials) -> KeychainResult<()> {
            KeychainStore::store(self, &names(), credentials)
        }

        fn clear(&self) -> KeychainResult<()> {
            KeychainStore::clear(self, &names())
        }
    }

    fn store(
        protected: &dyn KeychainStore,
        login: &dyn KeychainStore,
        credentials: &Credentials,
    ) -> KeychainResult<SessionLocation> {
        super::store(protected, login, &names(), credentials)
    }

    fn load(
        protected: &dyn KeychainStore,
        login: &dyn KeychainStore,
        profile: &StoredProfile,
    ) -> KeychainResult<Option<Credentials>> {
        super::load(protected, login, &names(), profile)
    }

    fn clear(protected: &dyn KeychainStore, login: &dyn KeychainStore) -> KeychainResult<()> {
        super::clear(protected, login, &names())
    }

    #[derive(Default)]
    struct NamedStore {
        items: Mutex<std::collections::HashMap<(String, String), Credentials>>,
        write_error: Option<i32>,
    }

    impl KeychainStore for NamedStore {
        fn load(&self, names: &KeychainNames) -> KeychainResult<Option<Credentials>> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(names.service.clone(), names.credentials_account.clone()))
                .cloned())
        }

        fn store(&self, names: &KeychainNames, credentials: &Credentials) -> KeychainResult<()> {
            if let Some(status) = self.write_error {
                return Err(error(status));
            }
            self.items.lock().unwrap().insert(
                (names.service.clone(), names.credentials_account.clone()),
                credentials.clone(),
            );
            Ok(())
        }

        fn clear(&self, names: &KeychainNames) -> KeychainResult<()> {
            self.items
                .lock()
                .unwrap()
                .remove(&(names.service.clone(), names.credentials_account.clone()));
            Ok(())
        }
    }

    #[test]
    fn qa_login_fallback_restart_and_logout_do_not_read_or_replace_the_default_sessions() {
        for fallback in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let default = root.path().join("default");
            let qa = root.path().join("qa");
            std::fs::create_dir(&default).unwrap();
            std::fs::create_dir(&qa).unwrap();
            let owner_names = KeychainNames::new(&default, &default).unwrap();
            let qa_names = KeychainNames::new(&qa, &default).unwrap();
            let protected = NamedStore {
                write_error: fallback.then_some(MISSING_ENTITLEMENT),
                ..Default::default()
            };
            let login = NamedStore::default();
            let owner = credentials();
            let key = (
                owner_names.service.clone(),
                owner_names.credentials_account.clone(),
            );
            protected
                .items
                .lock()
                .unwrap()
                .insert(key.clone(), owner.clone());
            login
                .items
                .lock()
                .unwrap()
                .insert(key.clone(), owner.clone());
            let owner_profile = owner.stored_profile(SessionLocation::Protected);
            assert!(
                super::load(&protected, &login, &qa_names, &owner_profile)
                    .unwrap()
                    .is_none()
            );
            let mut guest = credentials();
            guest.username = "qa".into();
            guest.session_id = Some([2; 16]);
            let location = super::store(&protected, &login, &qa_names, &guest).unwrap();
            assert_eq!(
                location,
                if fallback {
                    SessionLocation::Login
                } else {
                    SessionLocation::Protected
                }
            );
            let restarted = KeychainNames::new(&qa, &default).unwrap();
            assert_credentials(
                super::load(
                    &protected,
                    &login,
                    &restarted,
                    &guest.stored_profile(location),
                )
                .unwrap(),
                &guest,
            );
            super::clear(&protected, &login, &qa_names).unwrap();
            assert!(
                KeychainStore::load(&protected, &qa_names)
                    .unwrap()
                    .is_none()
            );
            assert!(KeychainStore::load(&login, &qa_names).unwrap().is_none());
            assert_credentials(protected.items.lock().unwrap().get(&key).cloned(), &owner);
            assert_credentials(login.items.lock().unwrap().get(&key).cloned(), &owner);
        }
    }

    fn credentials() -> Credentials {
        Credentials {
            username: "alice".into(),
            device_name: "Test Mac".into(),
            server_url: "https://test.example".into(),
            session_id: Some([1; 16]),
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
        let location = store(&protected, &login, &credentials).unwrap();
        assert_eq!(location, SessionLocation::Protected);
        assert_credentials(
            load(&protected, &login, &credentials.stored_profile(location)).unwrap(),
            &credentials,
        );
        assert!(login.credentials.lock().unwrap().is_none());
        assert_eq!(*protected.calls.lock().unwrap(), ["store", "load"]);
        assert_eq!(*login.calls.lock().unwrap(), ["store", "clear", "load"]);
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
            let location = store(&protected, &login, &credentials).unwrap();
            assert_eq!(location, SessionLocation::Login);
            assert_credentials(
                load(&protected, &login, &credentials.stored_profile(location)).unwrap(),
                &credentials,
            );
            assert_eq!(
                *protected.calls.lock().unwrap(),
                ["store", "clear", "load", "clear"]
            );
            assert_eq!(*login.calls.lock().unwrap(), ["store", "load"]);
        }
    }

    #[test]
    fn login_credentials_are_read_without_rewriting_them_when_protected_item_is_absent() {
        let credentials = credentials();
        let protected = FakeStore::default();
        let login = FakeStore::default();
        login.store(&credentials).unwrap();
        assert_credentials(
            load(
                &protected,
                &login,
                &credentials.stored_profile(SessionLocation::Login),
            )
            .unwrap(),
            &credentials,
        );
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
            assert!(login.calls.lock().unwrap().is_empty());
            assert!(
                load(
                    &protected,
                    &login,
                    &credentials().stored_profile(SessionLocation::Protected)
                )
                .is_err()
            );
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
        let profile = credentials().stored_profile(SessionLocation::Login);
        assert!(load(&protected, &login, &profile).unwrap().is_none());
        login.read_error = Some(-25293);
        assert!(matches!(
            load(&protected, &login, &profile),
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

    #[test]
    fn restart_never_restores_the_old_account_after_a_failed_delete_and_fallback_login() {
        let directory = tempfile::tempdir().unwrap();
        let mut protected = FakeStore::default();
        let login = FakeStore::default();
        let alice = credentials();
        let location = store(&protected, &login, &alice).unwrap();
        let mut profile = alice.stored_profile(location);
        super::super::store_profile(directory.path(), &profile).unwrap();

        profile.signed_out = true;
        profile.resume = None;
        super::super::store_profile(directory.path(), &profile).unwrap();
        protected.delete_error = Some(-25293);
        assert!(clear(&protected, &login).is_err());
        assert!(load(&protected, &login, &profile).unwrap().is_none());

        let mut bob = credentials();
        bob.username = "bob".into();
        bob.server_url = "https://bob.example".into();
        bob.session_id = Some([2; 16]);
        protected.write_error = Some(MISSING_ENTITLEMENT);
        let location = store(&protected, &login, &bob).unwrap();
        assert_eq!(location, SessionLocation::Login);
        super::super::store_profile(directory.path(), &bob.stored_profile(location)).unwrap();

        let profile = super::super::load_profile(directory.path())
            .unwrap()
            .unwrap();
        assert_credentials(load(&protected, &login, &profile).unwrap(), &bob);
        assert_credentials(protected.credentials.lock().unwrap().clone(), &alice);
        assert!(!profile.signed_out);
        assert_eq!(profile.profile.username, "bob");

        login.clear().unwrap();
        assert!(load(&protected, &login, &profile).unwrap().is_none());
        assert_credentials(protected.credentials.lock().unwrap().clone(), &alice);
    }

    #[test]
    fn a_matching_account_with_a_different_session_id_is_removed_instead_of_resumed() {
        let protected = FakeStore::default();
        let login = FakeStore::default();
        let mut current = credentials();
        protected.store(&current).unwrap();
        current.session_id = Some([2; 16]);
        let profile = current.stored_profile(SessionLocation::Protected);
        assert!(load(&protected, &login, &profile).unwrap().is_none());
        assert!(protected.credentials.lock().unwrap().is_none());
        assert_eq!(*protected.calls.lock().unwrap(), ["store", "load", "clear"]);
    }

    #[test]
    fn a_session_id_copied_to_another_account_or_server_does_not_match() {
        for change_server in [false, true] {
            let protected = FakeStore::default();
            let login = FakeStore::default();
            let current = credentials();
            let mut stale = current.clone();
            if change_server {
                stale.server_url = "https://stale.example".into();
            } else {
                stale.username = "stale".into();
            }
            protected.store(&stale).unwrap();
            assert!(
                load(
                    &protected,
                    &login,
                    &current.stored_profile(SessionLocation::Protected)
                )
                .unwrap()
                .is_none()
            );
            assert!(protected.credentials.lock().unwrap().is_none());
        }
    }

    #[test]
    fn credentials_without_a_current_session_record_cannot_resume() {
        let protected = FakeStore::default();
        let login = FakeStore::default();
        let current = credentials();
        protected.store(&current).unwrap();
        login.store(&current).unwrap();
        let profile = StoredProfile::from(current.profile());
        assert!(load(&protected, &login, &profile).unwrap().is_none());
        assert!(protected.credentials.lock().unwrap().is_none());
        assert!(login.credentials.lock().unwrap().is_none());
    }
}
