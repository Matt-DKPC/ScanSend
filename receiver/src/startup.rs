//! "Start ScanSend when I log in": a Run registry entry on Windows, a Launch Agent
//! on macOS, an autostart .desktop file on Linux. Always points at the running
//! program, so moving the program and ticking the box again fixes it.

use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};

fn launcher() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name("ScanSend")
        .set_app_path(exe.to_str()?)
        .set_macos_launch_mode(MacOSLaunchMode::LaunchAgent)
        .build()
        .ok()
}

pub fn is_enabled() -> bool {
    launcher().and_then(|l| l.is_enabled().ok()).unwrap_or(false)
}

pub fn set(on: bool) -> Result<(), String> {
    let l = launcher().ok_or("Couldn't find where ScanSend is installed.")?;
    if on { l.enable() } else { l.disable() }.map_err(|e| e.to_string())
}
