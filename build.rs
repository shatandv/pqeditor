//! Build script: stamps the build time into the binary so the window
//! title shows which build is running (stale-exe protection).
//!
//! The stamp is the wall-clock time of THIS compile. The build script
//! only reruns when `.build-stamp` changes, so every shipped build must
//! `touch .build-stamp` first (cross/build-windows.sh and
//! tools/morning-run.sh do). A plain `cargo build` may reuse the
//! previous stamp — acceptable, since shipped flows always touch it.

use std::process::Command;

fn main() {
    // Local time via `date` (build scripts run on a dev box, not the
    // cross target). Fall back to unix seconds without `date`.
    let stamp = Command::new("date")
        .args(["+%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("unix {}", secs)
        });
    println!("cargo:rustc-env=PQEDITOR_BUILD_STAMP={}", stamp);
    println!("cargo:rerun-if-changed=.build-stamp");
}
