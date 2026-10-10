use std::process::Command;

fn main() {
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