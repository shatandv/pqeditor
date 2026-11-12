//! Build script: stamps the build time into the binary so the window
//! title shows which build is running (stale-exe protection).

use std::process::Command;

fn main() {
    // Prefer the git commit date/time; fall back to the current time.
    let stamp = Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // No git (e.g. source snapshot): use this build's wall time.
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("unix {}", secs)
        });
    println!("cargo:rustc-env=PQEDITOR_BUILD_STAMP={}", stamp);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
