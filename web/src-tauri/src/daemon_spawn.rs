use std::path::PathBuf;

use tracing::{info, warn};

pub fn spawn_daemon(server_url: &str) {
    let Some(path) = find_daemon_binary() else {
        warn!("clipper-daemon binary not found; clipboard sync won't persist when window closes");
        return;
    };
    info!("Spawning daemon from {}", path.display());
    match std::process::Command::new(&path)
        .arg("--server-url")
        .arg(server_url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            // Drop the Child handle — on unix the process is reparented to
            // PID 1 when the Tauri app exits, so it keeps running.
        }
        Err(e) => warn!("Failed to spawn daemon: {}", e),
    }
}

fn find_daemon_binary() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    // Tauri sidecar naming in a production bundle: clipper-daemon-{triple}
    let sidecar = exe_dir.join(format!("clipper-daemon-{}", env!("APP_TARGET")));
    if sidecar.exists() {
        return Some(sidecar);
    }

    // Plain name for dev builds (cargo build puts it next to clipper-desktop)
    let plain = exe_dir.join("clipper-daemon");
    if plain.exists() {
        return Some(plain);
    }

    None
}
