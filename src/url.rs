use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

/// A URL detected on the terminal screen, with its screen coordinates.
#[derive(Debug, Clone)]
pub struct DetectedUrl {
    pub url: String,
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}

/// Cached URL scan results to avoid re-scanning every frame.
pub struct UrlCache {
    pub urls: Vec<DetectedUrl>,
    pub last_scan: Instant,
    pub hovered: Option<usize>,
}

impl Default for UrlCache {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            last_scan: Instant::now(),
            hovered: None,
        }
    }
}

/// Get the compiled URL regex (compiled once, reused forever).
fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"https?://[^\s<>"'`\\\]|)}>]+"#).unwrap()
    })
}

/// Scan all rows of a vt100 screen for URLs. Returns detected URLs with
/// their screen coordinates (row, col_start, col_end).
pub fn scan_urls_from_screen(screen: &vt100::Screen) -> Vec<DetectedUrl> {
    let re = url_regex();
    let (rows, cols) = screen.size();
    let mut results = Vec::new();

    for row in 0..rows {
        // Build the text for this row and track the mapping from char index
        // to screen column (to handle wide characters correctly).
        let mut text = String::new();
        let mut char_to_col: Vec<u16> = Vec::new();

        let mut col = 0u16;
        while col < cols {
            if let Some(cell) = screen.cell(row, col) {
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                let contents = cell.contents();
                for ch in contents.chars() {
                    char_to_col.push(col);
                    text.push(ch);
                }
                // Advance past wide characters
                if cell.is_wide() {
                    col += 2;
                } else {
                    col += 1;
                }
            } else {
                char_to_col.push(col);
                text.push(' ');
                col += 1;
            }
        }

        // Find all URL matches in this row's text
        for m in re.find_iter(&text) {
            let start_idx = m.start();
            let end_idx = m.end();

            // Map char indices back to screen columns
            let col_start = char_to_col.get(start_idx).copied().unwrap_or(0);
            let col_end = if end_idx > 0 && end_idx <= char_to_col.len() {
                // col_end is exclusive: one past the last character
                char_to_col.get(end_idx - 1).copied().unwrap_or(cols) + 1
            } else {
                col_start
            };

            // Strip trailing punctuation that's likely not part of the URL
            let mut url = m.as_str().to_string();
            while url.ends_with('.') || url.ends_with(',') || url.ends_with(';') || url.ends_with(':') {
                url.pop();
            }

            if !url.is_empty() {
                let actual_end = col_start + url.len() as u16;
                results.push(DetectedUrl {
                    url,
                    row,
                    col_start,
                    col_end: actual_end.min(col_end),
                });
            }
        }
    }

    results
}

/// Open a URL in the system browser.
/// WSL: tries `wslview` first, then `powershell.exe Start-Process`.
/// Linux: `xdg-open`. macOS: `open`.
pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    let is_wsl = std::env::var("WSL_DISTRO_NAME").is_ok();

    if is_wsl {
        // Try wslview first (from wslu package)
        if let Ok(status) = std::process::Command::new("wslview")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if status.success() {
                return Ok(());
            }
        }
        // Fallback: powershell.exe Start-Process
        if let Ok(status) = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &format!("Start-Process '{}'", url)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if status.success() {
                return Ok(());
            }
        }
        return Err("Failed to open URL (tried wslview, powershell.exe)".to_string());
    }

    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(target_os = "macos"))]
    let cmd = "xdg-open";

    match std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(format!("{} returned non-zero", cmd)),
        Err(e) => Err(format!("Failed to run {}: {}", cmd, e)),
    }
}

/// Copy text to the system clipboard.
/// WSL: `clip.exe`. Linux: `xclip` or `xsel`. macOS: `pbcopy`.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let is_wsl = std::env::var("WSL_DISTRO_NAME").is_ok();

    if is_wsl {
        return pipe_to_command("clip.exe", &[], text);
    }

    #[cfg(target_os = "macos")]
    return pipe_to_command("pbcopy", &[], text);

    #[cfg(not(target_os = "macos"))]
    {
        // Try xclip first, then xsel
        if pipe_to_command("xclip", &["-selection", "clipboard"], text).is_ok() {
            return Ok(());
        }
        pipe_to_command("xsel", &["--clipboard", "--input"], text)
    }
}

/// Pipe text to a command's stdin.
fn pipe_to_command(cmd: &str, args: &[&str], text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to run {}: {}", cmd, e))?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(text.as_bytes())
            .map_err(|e| format!("Failed to write to {}: {}", cmd, e))?;
    }
    let status = child.wait().map_err(|e| format!("Failed to wait for {}: {}", cmd, e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} returned non-zero", cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a vt100 parser, write text, return detected URLs.
    fn detect(text: &str) -> Vec<DetectedUrl> {
        let parser = vt100::Parser::new(24, 120, 0);
        // Write the text into the parser so it appears on screen
        let mut p = parser;
        p.process(text.as_bytes());
        scan_urls_from_screen(p.screen())
    }

    #[test]
    fn detects_https_url() {
        let urls = detect("Visit https://example.com for info");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[0].row, 0);
        assert_eq!(urls[0].col_start, 6);
    }

    #[test]
    fn detects_http_url() {
        let urls = detect("http://foo.bar/baz");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "http://foo.bar/baz");
    }

    #[test]
    fn detects_url_with_path_and_query() {
        let urls = detect("Go to https://auth.example.com/login?code=abc123&state=xyz");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://auth.example.com/login?code=abc123&state=xyz");
    }

    #[test]
    fn strips_trailing_punctuation() {
        let urls = detect("See https://example.com.");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
    }

    #[test]
    fn multiple_urls_one_line() {
        let urls = detect("A: https://a.com B: https://b.com/path");
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].url, "https://a.com");
        assert_eq!(urls[1].url, "https://b.com/path");
    }

    #[test]
    fn no_urls() {
        let urls = detect("nothing here, just text");
        assert!(urls.is_empty());
    }

    #[test]
    fn url_columns_correct() {
        // "https://x.co" starts at col 0
        let urls = detect("https://x.co rest");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].col_start, 0);
        assert_eq!(urls[0].col_end, 12); // "https://x.co" is 12 chars
    }

    #[test]
    fn url_on_second_line() {
        // vt100 needs \r\n to move to column 0 of the next line
        let urls = detect("first line\r\nhttps://line2.com");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].row, 1);
        assert_eq!(urls[0].col_start, 0);
    }

    #[test]
    fn stops_at_quotes_and_angles() {
        let urls = detect(r#"<https://example.com> and "https://other.com""#);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[1].url, "https://other.com");
    }
}
