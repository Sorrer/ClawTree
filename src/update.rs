use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

/// Startup delay before checking for updates (seconds).
const UPDATE_CHECK_DELAY_SECS: u64 = 5;

/// Spawn a background thread that checks GitHub Releases for a newer version.
/// One-shot: sleeps, checks once, sends an event if newer, then exits.
pub fn spawn_update_checker(event_tx: UnboundedSender<AppEvent>) {
    std::thread::Builder::new()
        .name("update-checker".into())
        .spawn(move || {
            // Let the TUI start up first
            std::thread::sleep(Duration::from_secs(UPDATE_CHECK_DELAY_SECS));

            if let Some(latest) = fetch_latest_version() {
                let current = option_env!("CLAWTREE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
                if version_is_newer(&latest, current) {
                    let _ = event_tx.send(AppEvent::UpdateAvailable {
                        latest_version: latest,
                    });
                }
            }
        })
        .ok();
}

/// Fetch the latest release tag from GitHub using curl.
fn fetch_latest_version() -> Option<String> {
    let output = std::process::Command::new("curl")
        .args([
            "-sL",
            "--max-time",
            "10",
            "https://api.github.com/repos/Sorrer/ClawTree/releases/latest",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;

    // Strip leading 'v' prefix if present
    let version = tag.strip_prefix('v').unwrap_or(tag);
    Some(version.to_string())
}

/// Returns true if `latest` is strictly newer than `current` using semver comparison.
fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };

    let l = parse(latest);
    let c = parse(current);

    // Compare component by component
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv > cv {
            return true;
        }
        if lv < cv {
            return false;
        }
    }
    false
}

/// Return the platform-specific artifact name used in GitHub Releases.
fn platform_artifact_name() -> Option<&'static str> {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("x86_64-linux-musl")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        Some("aarch64-linux-musl")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        Some("x86_64-apple-darwin")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Some("aarch64-apple-darwin")
    } else {
        None
    }
}

/// Spawn a background thread that downloads and verifies a release binary.
/// Sends `UpdateDownloadComplete` when done.
pub fn spawn_update_download(version: String, event_tx: UnboundedSender<AppEvent>) {
    std::thread::Builder::new()
        .name("update-download".into())
        .spawn(move || {
            let result = download_and_verify(&version);
            let _ = event_tx.send(AppEvent::UpdateDownloadComplete { result, version });
        })
        .ok();
}

/// Download the release tarball, verify its SHA256 checksum, and extract the binary.
fn download_and_verify(version: &str) -> Result<PathBuf, String> {
    let artifact = platform_artifact_name()
        .ok_or_else(|| "Unsupported platform for self-update".to_string())?;

    let archive_name = format!("clawtree-{}.tar.gz", artifact);
    let tag = format!("v{}", version);
    let base_url = format!(
        "https://github.com/Sorrer/ClawTree/releases/download/{}/",
        tag
    );

    let tmp_dir = std::env::temp_dir().join(format!("clawtree-update-{}", version));
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {}", e))?;

    let archive_path = tmp_dir.join(&archive_name);
    let checksum_path = tmp_dir.join(format!("{}.sha256", archive_name));

    // Download archive
    let status = std::process::Command::new("curl")
        .args(["-sL", "--fail", "--max-time", "120", "-o"])
        .arg(archive_path.to_str().unwrap_or_default())
        .arg(format!("{}{}", base_url, archive_name))
        .status()
        .map_err(|e| format!("Failed to run curl: {}", e))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("Failed to download release archive".to_string());
    }

    // Download checksum
    let status = std::process::Command::new("curl")
        .args(["-sL", "--fail", "--max-time", "30", "-o"])
        .arg(checksum_path.to_str().unwrap_or_default())
        .arg(format!("{}{}.sha256", base_url, archive_name))
        .status()
        .map_err(|e| format!("Failed to run curl for checksum: {}", e))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("Failed to download checksum file".to_string());
    }

    // Read expected checksum
    let checksum_content = std::fs::read_to_string(&checksum_path)
        .map_err(|e| format!("Failed to read checksum file: {}", e))?;
    let expected_hash = checksum_content
        .split_whitespace()
        .next()
        .ok_or_else(|| "Invalid checksum file format".to_string())?
        .to_lowercase();

    // Compute actual checksum — try sha256sum first, fall back to shasum -a 256
    let actual_hash = compute_sha256(&archive_path)?;

    if actual_hash != expected_hash {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(format!(
            "SHA256 mismatch: expected {}, got {}",
            expected_hash, actual_hash
        ));
    }

    // Extract binary
    let status = std::process::Command::new("tar")
        .args(["xzf"])
        .arg(archive_path.to_str().unwrap_or_default())
        .arg("-C")
        .arg(tmp_dir.to_str().unwrap_or_default())
        .status()
        .map_err(|e| format!("Failed to extract archive: {}", e))?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("Failed to extract archive".to_string());
    }

    let binary_path = tmp_dir.join("clawtree");
    if !binary_path.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("Binary not found in archive".to_string());
    }

    Ok(binary_path)
}

/// Compute the SHA256 hash of a file using available system tools.
fn compute_sha256(path: &std::path::Path) -> Result<String, String> {
    // Try sha256sum first (Linux)
    if let Ok(output) = std::process::Command::new("sha256sum")
        .arg(path.to_str().unwrap_or_default())
        .output()
    {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            if let Some(hash) = out.split_whitespace().next() {
                return Ok(hash.to_lowercase());
            }
        }
    }

    // Fall back to shasum -a 256 (macOS)
    if let Ok(output) = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path.to_str().unwrap_or_default())
        .output()
    {
        if output.status.success() {
            let out = String::from_utf8_lossy(&output.stdout);
            if let Some(hash) = out.split_whitespace().next() {
                return Ok(hash.to_lowercase());
            }
        }
    }

    Err("No SHA256 tool available (tried sha256sum and shasum)".to_string())
}

/// Spawn a background thread that replaces the current binary with the downloaded one.
/// Sends `UpdateReplaceComplete` when done.
pub fn spawn_update_replace(
    binary_path: PathBuf,
    version: String,
    event_tx: UnboundedSender<AppEvent>,
) {
    std::thread::Builder::new()
        .name("update-replace".into())
        .spawn(move || {
            let result = replace_binary(&binary_path);
            let _ = event_tx.send(AppEvent::UpdateReplaceComplete { result, version });
        })
        .ok();
}

/// Replace the current running binary with the new one.
fn replace_binary(new_binary: &std::path::Path) -> Result<(), String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current executable path: {}", e))?;

    // Resolve symlinks to get the actual binary path
    let current_exe = std::fs::canonicalize(&current_exe).unwrap_or(current_exe);

    let backup_path = current_exe.with_extension("old");

    // Rename current binary to .old backup
    std::fs::rename(&current_exe, &backup_path)
        .map_err(|e| format!("Failed to create backup: {}", e))?;

    // Copy new binary into place
    match std::fs::copy(new_binary, &current_exe) {
        Ok(_) => {}
        Err(e) => {
            // Restore from backup
            let _ = std::fs::rename(&backup_path, &current_exe);
            return Err(format!("Failed to install new binary: {}", e));
        }
    }

    // Set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755));
    }

    // Clean up backup and temp dir
    let _ = std::fs::remove_file(&backup_path);
    if let Some(parent) = new_binary.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_is_newer() {
        assert!(version_is_newer("1.1.0", "1.0.0"));
        assert!(version_is_newer("2.0.0", "1.9.9"));
        assert!(version_is_newer("1.0.1", "1.0.0"));
        assert!(!version_is_newer("1.0.0", "1.0.0"));
        assert!(!version_is_newer("0.9.0", "1.0.0"));
        assert!(version_is_newer("1.0.0", "0.1.0"));
    }
}
