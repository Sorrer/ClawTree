use std::process::Command;

fn main() {
    // Re-run if git HEAD changes (new commit, tag, etc.)
    println!("cargo::rerun-if-changed=.git/HEAD");

    let version = Command::new("git")
        .args(["describe", "--tags", "--match", "v*", "--always"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));

    println!("cargo::rustc-env=CLAWTREE_VERSION={version}");
}
