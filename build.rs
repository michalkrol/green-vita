use std::process::Command;

fn main() {
    // Re-run when HEAD moves (commit or checkout) so the compiled-in hash is the
    // commit the binary was actually built from.  Without this, cargo only
    // re-runs the build script when a package file changes, so a build made
    // right after a commit keeps the previous commit's hash and the settings
    // screen reports the wrong build.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/logs/HEAD");

    // Get git commit hash (short)
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Get build timestamp
    let build_timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    // Cargo pkg version is already available via env!
    println!("cargo:rustc-env=GIT_SHA={}", git_sha);
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", build_timestamp);
}