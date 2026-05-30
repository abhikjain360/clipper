//! Shared local daemon IPC filesystem paths.

use std::path::{Path, PathBuf};

pub const PRIVATE_SOCKET_DIR_MODE: u32 = 0o700;
pub const SOCKET_FILE_MODE: u32 = 0o600;

const SOCKET_FILE_NAME: &str = "daemon.sock";

pub fn socket_path() -> PathBuf {
    socket_dir().join(SOCKET_FILE_NAME)
}

pub fn socket_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(path) = xdg_runtime_dir() {
        return path.join("clipper");
    }

    fallback_socket_dir()
}

pub fn ensure_private_socket_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    std::fs::create_dir_all(path)?;

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }

    let current_uid = current_euid();
    if metadata.uid() != current_uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is owned by uid {}, expected {}",
                path.display(),
                metadata.uid(),
                current_uid
            ),
        ));
    }

    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(PRIVATE_SOCKET_DIR_MODE),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn xdg_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn fallback_socket_dir() -> PathBuf {
    PathBuf::from("/tmp").join(format!("clipper-{}", current_euid()))
}

fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_socket_path_fits_sockaddr_un() {
        use std::os::unix::ffi::OsStrExt;

        assert_eq!(
            socket_path(),
            PathBuf::from(format!("/tmp/clipper-{}/daemon.sock", current_euid()))
        );
        assert!(socket_path().as_os_str().as_bytes().len() < 104);
    }
}
