//! Shared local daemon IPC filesystem paths.

use std::path::{Path, PathBuf};

pub const PRIVATE_SOCKET_DIR_MODE: u32 = 0o700;
pub const SOCKET_FILE_MODE: u32 = 0o600;
pub const SOCKET_PATH_ENV: &str = "CLIPPER_DAEMON_SOCKET_PATH";

const SOCKET_FILE_NAME: &str = "daemon.sock";
#[cfg(target_os = "macos")]
const MACOS_APP_CONTAINER_ID: &str = "com.clipper.desktop";

pub fn socket_path() -> PathBuf {
    if let Some(path) = env_socket_path() {
        return path;
    }

    socket_dir().join(SOCKET_FILE_NAME)
}

pub fn socket_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(path) = xdg_runtime_dir() {
        return path.join("clipper");
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = macos_sandbox_container_socket_dir() {
        return path;
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = macos_default_container_socket_dir() {
        return path;
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

pub fn validate_socket_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a Unix socket", path.display()),
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
                current_uid,
            ),
        ));
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} has insecure mode {:03o}", path.display(), mode),
        ));
    }

    if let Some(parent) = path.parent() {
        validate_socket_dir(parent)?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn xdg_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(target_os = "linux")]
fn fallback_socket_dir() -> PathBuf {
    PathBuf::from("/run/user")
        .join(current_euid().to_string())
        .join("clipper")
}

#[cfg(not(target_os = "linux"))]
fn fallback_socket_dir() -> PathBuf {
    // Last resort, only when the platform container resolvers return None (e.g.
    // home_dir() is unavailable). The Linux /run/user path is not writable here,
    // so fall back to the OS temp dir, which is writable for the current user.
    std::env::temp_dir().join("clipper")
}

fn env_socket_path() -> Option<PathBuf> {
    std::env::var_os(SOCKET_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(target_os = "macos")]
fn macos_sandbox_container_socket_dir() -> Option<PathBuf> {
    let data_dir = dirs::data_dir()?;
    if !data_dir.ends_with(Path::new("Library/Application Support")) {
        return None;
    }

    let container_data_dir = data_dir.parent()?.parent()?;
    if container_data_dir.file_name()?.to_str()? != "Data" {
        return None;
    }

    let container_dir = container_data_dir.parent()?;
    if container_dir.parent()?.file_name()?.to_str()? != "Containers" {
        return None;
    }

    Some(container_data_dir.join("tmp").join("Clipper"))
}

#[cfg(target_os = "macos")]
fn macos_default_container_socket_dir() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join("Library/Containers")
            .join(MACOS_APP_CONTAINER_ID)
            .join("Data/tmp/Clipper"),
    )
}

fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

fn validate_socket_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

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
                current_uid,
            ),
        ));
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} has insecure mode {:03o}", path.display(), mode),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_socket_path_fits_sockaddr_un() {
        use std::os::unix::ffi::OsStrExt;

        assert!(socket_path().ends_with(Path::new("tmp/Clipper/daemon.sock")));
        assert!(socket_path().as_os_str().as_bytes().len() < 104);
    }
}
