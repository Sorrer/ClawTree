use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

/// Spawn a background thread that checks GitHub Releases for a newer version.
/// One-shot: sleeps 5s, checks once, sends an event if newer, then exits.
pub fn spawn_update_checker(event_tx: UnboundedSender<AppEvent>) {
    std::thread::Builder::new()
        .name("update-checker".into())
        .spawn(move || {
            // Let the TUI start up first
            std::thread::sleep(Duration::from_secs(5));

            if let Some(latest) = fetch_latest_version() {
                let current = env!("CARGO_PKG_VERSION");
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
