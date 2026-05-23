//! LaunchAgent management for the clipper-daemon on macOS.

use std::path::PathBuf;

const LAUNCHAGENT_LABEL: &str = "com.clipper.daemon";

/// Find the daemon binary inside the app bundle (<bundle>/Contents/MacOS/clipper-daemon).
fn daemon_binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    let daemon = macos_dir.join("clipper-daemon");
    if daemon.exists() { Some(daemon) } else { None }
}

fn launchagent_plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents/com.clipper.daemon.plist")
}

fn generate_plist(daemon_path: &std::path::Path) -> String {
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
    <key>RunAtLoad</key>
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
        daemon = daemon_path.display(),
    )
}

/// Ensure the daemon is running. Installs/updates the LaunchAgent if needed.
pub(crate) fn install_and_start_daemon() -> anyhow::Result<()> {
    let daemon_path = daemon_binary_path()
        .ok_or_else(|| anyhow::anyhow!("clipper-daemon not found in app bundle"))?;

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
        let sock = crate::transport::socket_path();
        if !sock.exists() {
            let output = std::process::Command::new("launchctl")
                .args(["start", LAUNCHAGENT_LABEL])
                .output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!("launchctl start warning: {}", stderr);
            }
        }
    }

    Ok(())
}
