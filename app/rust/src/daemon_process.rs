//! Per-user daemon management.

use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const LAUNCHAGENT_LABEL: &str = "com.clipper.daemon";
#[cfg(target_os = "linux")]
const SYSTEMD_UNIT_NAME: &str = "clipper-daemon.service";

pub(crate) type DaemonProcessResult<T> = Result<T, DaemonProcessError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonProcessError {
    #[error("clipper-daemon not found")]
    DaemonBinaryNotFound,
    #[error("daemon process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[cfg(target_os = "linux")]
    #[error("{program} failed: {stderr}")]
    CommandFailed { program: String, stderr: String },
}

/// Find the daemon binary.
///
/// Packaged apps can place it next to the Flutter executable. Development
/// shells can set CLIPPER_DAEMON_PATH or keep clipper-daemon on PATH.
fn daemon_binary_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLIPPER_DAEMON_PATH").map(PathBuf::from)
        && path.exists()
    {
        return Some(normalize_existing_path(path));
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let bundled = exe_dir.join("clipper-daemon");
        if bundled.exists() {
            return Some(normalize_existing_path(bundled));
        }
    }

    #[cfg(target_os = "macos")]
    {
        None
    }

    #[cfg(target_os = "linux")]
    {
        find_on_path("clipper-daemon")
    }
}

/// Ensure the daemon is running.
#[cfg(target_os = "macos")]
pub(crate) fn install_and_start_daemon() -> DaemonProcessResult<()> {
    let daemon_path = daemon_binary_path().ok_or(DaemonProcessError::DaemonBinaryNotFound)?;

    let plist_path = launchagent_plist_path();
    let new_plist = generate_plist(&daemon_path);

    let needs_install = if plist_path.exists() {
        let existing = std::fs::read_to_string(&plist_path).unwrap_or_default();
        existing != new_plist
    } else {
        true
    };

    if needs_install {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();

        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plist_path, &new_plist)?;
        tracing::info!("Installed LaunchAgent plist at {}", plist_path.display());

        let output = std::process::Command::new("launchctl")
            .args(["load", &plist_path.to_string_lossy()])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("launchctl load warning: {}", stderr);
        }
    } else {
        match crate::transport::socket_path() {
            Ok(sock) if !sock.exists() => {
                let output = std::process::Command::new("launchctl")
                    .args(["start", LAUNCHAGENT_LABEL])
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!("launchctl start warning: {}", stderr);
                }
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("cannot locate daemon socket: {}", error),
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn launchagent_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents/com.clipper.daemon.plist")
}

#[cfg(target_os = "macos")]
fn generate_plist(daemon_path: &Path) -> String {
    let environment_variables = crate::transport::socket_path()
        .ok()
        .map(|socket_path| {
            format!(
                r#"    <key>EnvironmentVariables</key>
    <dict>
        <key>{socket_path_env}</key>
        <string>{socket_path}</string>
    </dict>
"#,
                socket_path_env = clipper_daemon_types::ipc_path::SOCKET_PATH_ENV,
                socket_path = xml_escape(&socket_path.display().to_string()),
            )
        })
        .unwrap_or_default();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{daemon}</string>
    </array>
{environment_variables}    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/clipper-daemon.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/clipper-daemon.stderr.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>"#,
        label = LAUNCHAGENT_LABEL,
        daemon = xml_escape(&daemon_path.display().to_string()),
        environment_variables = environment_variables,
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Ensure the Linux user service is installed and running.
#[cfg(target_os = "linux")]
pub(crate) fn install_and_start_daemon() -> DaemonProcessResult<()> {
    let daemon_path = daemon_binary_path().ok_or(DaemonProcessError::DaemonBinaryNotFound)?;

    if let Err(error) = install_and_start_systemd_user_service(&daemon_path) {
        tracing::warn!(
            %error,
            "systemd user service start failed; falling back to direct daemon spawn"
        );
        spawn_detached(&daemon_path)?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn install_and_start_systemd_user_service(daemon_path: &Path) -> DaemonProcessResult<()> {
    let unit_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd/user");
    std::fs::create_dir_all(&unit_dir)?;

    let unit_path = unit_dir.join(SYSTEMD_UNIT_NAME);
    let unit = generate_systemd_unit(daemon_path);
    let needs_install = std::fs::read_to_string(&unit_path)
        .map(|existing| existing != unit)
        .unwrap_or(true);
    if needs_install {
        std::fs::write(&unit_path, unit)?;
    }

    run_systemctl(&[
        "--user",
        "import-environment",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
    ])?;
    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", SYSTEMD_UNIT_NAME])?;
    if needs_install {
        run_systemctl(&["--user", "restart", SYSTEMD_UNIT_NAME])?;
    } else {
        run_systemctl(&["--user", "start", SYSTEMD_UNIT_NAME])?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn generate_systemd_unit(daemon_path: &Path) -> String {
    format!(
        r#"[Unit]
Description=Clipper background sync daemon
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart={}
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
"#,
        systemd_quote(daemon_path)
    )
}

#[cfg(target_os = "linux")]
fn systemd_quote(path: &Path) -> String {
    let value = path.display().to_string();
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> DaemonProcessResult<()> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(DaemonProcessError::CommandFailed {
            program: format!("systemctl {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(target_os = "linux")]
fn spawn_detached(daemon_path: &Path) -> std::io::Result<()> {
    std::process::Command::new(daemon_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(target_os = "linux")]
fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|path| path.exists())
        .map(normalize_existing_path)
}

fn normalize_existing_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}
