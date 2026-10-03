use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const DATA_DIR_ENV: &str = "CLIPPER_DATA_DIR";

pub fn default_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|base| base.join("Clipper"))
}

pub fn data_dir(explicit: Option<PathBuf>) -> Option<PathBuf> {
    explicit
        .or_else(|| std::env::var_os(DATA_DIR_ENV).map(PathBuf::from))
        .or_else(default_data_dir)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeychainNames {
    pub service: String,
    pub credentials_account: String,
    pub ipc_secret_account: String,
}

impl KeychainNames {
    pub fn for_data_dir(data_dir: &Path) -> std::io::Result<Self> {
        let default = default_data_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "default data directory unavailable",
            )
        })?;
        Self::new(data_dir, &default)
    }

    pub fn new(data_dir: &Path, default: &Path) -> std::io::Result<Self> {
        let mut names = Self {
            service: "com.clipper.daemon".into(),
            credentials_account: "credentials".into(),
            ipc_secret_account: "ipc-secret-v1".into(),
        };
        if data_dir == default {
            return Ok(names);
        }
        let canonical = std::fs::canonicalize(data_dir)?;
        match std::fs::canonicalize(default) {
            Ok(path) if canonical == path => return Ok(names),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let hash = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
        names.service.push('.');
        for byte in hash {
            use std::fmt::Write;
            write!(names.service, "{byte:02x}").expect("writing to a String succeeds");
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn configured_directories_are_shared_and_an_explicit_directory_takes_precedence() {
        if let Some(expected) = std::env::var_os("CLIPPER_DIRECTORY_TEST_CHILD") {
            assert_eq!(data_dir(None), Some(PathBuf::from(&expected)));
            let explicit = PathBuf::from(expected).join("explicit");
            assert_eq!(data_dir(Some(explicit.clone())), Some(explicit));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["data_dir::tests::configured_directories_are_shared_and_an_explicit_directory_takes_precedence", "--exact"])
            .env("CLIPPER_DIRECTORY_TEST_CHILD", root.path())
            .env(DATA_DIR_ENV, root.path())
            .stdout(std::process::Stdio::null())
            .status().unwrap();
        assert!(status.success());
    }

    #[test]
    fn the_default_directory_keeps_existing_items_and_other_directories_are_isolated() {
        let root = tempfile::tempdir().unwrap();
        let default = root.path().join("default");
        let first = root.path().join("first");
        let second = root.path().join("second");
        for path in [&default, &first, &second] {
            std::fs::create_dir(path).unwrap();
        }
        let owner = KeychainNames::new(&default, &default).unwrap();
        assert_eq!(owner.service, "com.clipper.daemon");
        assert_eq!(owner.credentials_account, "credentials");
        assert_eq!(owner.ipc_secret_account, "ipc-secret-v1");
        let qa = KeychainNames::new(&first, &default).unwrap();
        let other = KeychainNames::new(&second, &default).unwrap();
        assert_ne!(qa, owner);
        assert_ne!(qa, other);
        assert_eq!(qa, KeychainNames::new(&first, &default).unwrap());
        let mut store = HashMap::new();
        for names in [&owner, &qa, &other] {
            for account in [&names.credentials_account, &names.ipc_secret_account] {
                store.insert(
                    (names.service.clone(), account.clone()),
                    names.service.clone(),
                );
            }
        }
        for account in [&qa.credentials_account, &qa.ipc_secret_account] {
            store.remove(&(qa.service.clone(), account.clone()));
        }
        assert_eq!(store.len(), 4);
        for names in [&owner, &other] {
            for account in [&names.credentials_account, &names.ipc_secret_account] {
                assert_eq!(
                    store.get(&(names.service.clone(), account.clone())),
                    Some(&names.service)
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn aliases_use_the_same_items_as_the_canonical_directory() {
        let root = tempfile::tempdir().unwrap();
        let default = root.path().join("default");
        let qa = root.path().join("qa");
        std::fs::create_dir(&default).unwrap();
        std::fs::create_dir(&qa).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&qa, &alias).unwrap();
        assert_eq!(
            KeychainNames::new(&qa, &default).unwrap(),
            KeychainNames::new(&alias, &default).unwrap()
        );
        let owner_alias = root.path().join("owner-alias");
        std::os::unix::fs::symlink(&default, &owner_alias).unwrap();
        assert_eq!(
            KeychainNames::new(&default, &default).unwrap(),
            KeychainNames::new(&owner_alias, &default).unwrap()
        );
        let relative = qa.join(".").join("..").join("qa");
        assert_eq!(
            KeychainNames::new(&qa, &default).unwrap(),
            KeychainNames::new(&relative, &default).unwrap()
        );
    }

    #[test]
    fn an_unavailable_extra_directory_cannot_select_the_default_items() {
        let root = tempfile::tempdir().unwrap();
        let default = root.path().join("default");
        assert_eq!(
            KeychainNames::new(&default, &default).unwrap().service,
            "com.clipper.daemon"
        );
        assert!(KeychainNames::new(&root.path().join("missing"), &default).is_err());
    }
}
