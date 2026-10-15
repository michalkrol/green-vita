use std::process::Command;

/// Compiles the build identity into the binary so the settings screen can show
/// exactly which version/commit was flashed:
///   APP_VERSION     = Cargo.toml version (env!("CARGO_PKG_VERSION"))
///   GIT_COMMIT      = short commit hash
///   BUILD_TIMESTAMP = UTC build time
///
/// The hash is normally supplied by scripts/docker-build.sh, because the build
/// runs inside the vitasdk container where git may not be installed.  When the
/// environment does not provide it we fall back to `git rev-parse` and to
/// `date -u` so a plain `cargo vita build` still reports something sensible.
fn main() {
    // Re-run when HEAD moves (commit/checkout) so we never bake in a stale hash.
    if std::path::Path::new(".git/HEAD").exists() {
        println!("cargo:rerun-if-changed=.git/HEAD");
    }
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-env-changed=BUILD_TIMESTAMP");

    let git_sha = non_empty_env("GIT_SHA").unwrap_or_else(|| {
        command_output("git", &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
    });
    let build_timestamp = non_empty_env("BUILD_TIMESTAMP")
        .unwrap_or_else(|| command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_else(|| "unknown".to_owned()));

    println!("cargo:rustc-env=GIT_SHA={}", git_sha);
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", build_timestamp);
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_owned())
        .filter(|value| !value.is_empty())
}
