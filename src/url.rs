use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

/// A single row-span of a detected URL on the terminal screen.
#[derive(Debug, Clone)]
pub struct UrlSpan {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}

/// A URL detected on the terminal screen, possibly spanning multiple rows
/// when the terminal soft-wraps a long line.
#[derive(Debug, Clone)]
pub struct DetectedUrl {
    pub url: String,
    pub spans: Vec<UrlSpan>,
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

/// Check if a row appears to be soft-wrapped (its content continues on the
/// next row without a real newline).  We use two signals:
///
/// 1. `screen.row_wrapped(row)` — set by the vt100 parser when auto-wrapping
///    occurs.  This is reliable for direct output but gets cleared when tmux
///    redraws the screen with explicit cursor positioning.
/// 2. **Content heuristic** — if the last cell of the row contains a
///    non-whitespace character, the row filled all columns and likely wrapped.
///    False positives are harmless for URL detection because the regex
///    naturally stops at whitespace and non-URL characters.
fn row_appears_wrapped(screen: &vt100::Screen, row: u16, cols: u16) -> bool {
    if screen.row_wrapped(row) {
        return true;
    }
    // Fallback: check whether the last column is non-empty / non-space.
    if cols == 0 {
        return false;
    }
    if let Some(cell) = screen.cell(row, cols - 1) {
        let contents = cell.contents();
        !contents.is_empty() && contents != " "
    } else {
        false
    }
}

/// Scan all rows of a vt100 screen for URLs. Returns detected URLs with
/// their screen coordinates. Handles URLs that span multiple rows due to
/// terminal soft-wrapping by concatenating wrapped rows seamlessly.
pub fn scan_urls_from_screen(screen: &vt100::Screen) -> Vec<DetectedUrl> {
    let re = url_regex();
    let (rows, cols) = screen.size();

    // Build combined text from all rows, tracking each byte's (row, col).
    // We use byte-indexed positions because regex returns byte offsets.
    // Multi-byte characters get multiple entries mapping to the same (row, col).
    //
    // Rows that appear wrapped are concatenated without a separator so URLs
    // that span the wrap boundary are matched as one.  Non-wrapped rows get
    // a space separator to break URL matching at real line boundaries.
    let mut text = String::new();
    let mut byte_to_pos: Vec<(u16, u16)> = Vec::new();

    for row in 0..rows {
        // Insert separator between non-wrapped rows (not before the first row)
        if row > 0 && !row_appears_wrapped(screen, row - 1, cols) {
            byte_to_pos.push((u16::MAX, u16::MAX));
            text.push(' ');
        }

        let mut col = 0u16;
        while col < cols {
            if let Some(cell) = screen.cell(row, col) {
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                let contents = cell.contents();
                for ch in contents.chars() {
                    // Map every byte of this character to the same (row, col)
                    let byte_start = text.len();
                    text.push(ch);
                    for _ in byte_start..text.len() {
                        byte_to_pos.push((row, col));
                    }
                }
                if cell.is_wide() {
                    col += 2;
                } else {
                    col += 1;
                }
            } else {
                byte_to_pos.push((row, col));
                text.push(' ');
                col += 1;
            }
        }
    }

    // Find all URL matches in the combined text
    let mut results = Vec::new();
    for m in re.find_iter(&text) {
        let start_byte = m.start();
        let end_byte = m.end();

        // Strip trailing punctuation that's likely not part of the URL.
        // These are all ASCII (1 byte each), so stripped == bytes removed.
        let mut url = m.as_str().to_string();
        let mut stripped = 0usize;
        while url.ends_with('.') || url.ends_with(',') || url.ends_with(';') || url.ends_with(':') {
            url.pop();
            stripped += 1;
        }
        if url.is_empty() {
            continue;
        }
        let effective_end = end_byte - stripped;

        // Build spans by grouping consecutive bytes by row.
        // Skip duplicate (row, col) entries from multi-byte characters.
        let mut spans: Vec<UrlSpan> = Vec::new();
        let mut prev_pos: Option<(u16, u16)> = None;
        for pos_entry in byte_to_pos.iter().take(effective_end).skip(start_byte) {
            let pos = *pos_entry;
            // Skip duplicate positions from multi-byte chars
            if prev_pos == Some(pos) {
                continue;
            }
            prev_pos = Some(pos);
            let (r, c) = pos;
            if r == u16::MAX {
                continue;
            }
            match spans.last_mut() {
                Some(span) if span.row == r => {
                    let new_end = c + 1;
                    if new_end > span.col_end {
                        span.col_end = new_end;
                    }
                }
                _ => {
                    spans.push(UrlSpan {
                        row: r,
                        col_start: c,
                        col_end: c + 1,
                    });
                }
            }
        }

        if !spans.is_empty() {
            results.push(DetectedUrl { url, spans });
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

    /// Helper: create a vt100 parser with given dimensions, write text,
    /// return detected URLs.
    fn detect_with_size(rows: u16, cols: u16, text: &str) -> Vec<DetectedUrl> {
        let mut p = vt100::Parser::new(rows, cols, 0);
        p.process(text.as_bytes());
        scan_urls_from_screen(p.screen())
    }

    /// Helper: create a vt100 parser (24x120), write text, return detected URLs.
    fn detect(text: &str) -> Vec<DetectedUrl> {
        detect_with_size(24, 120, text)
    }

    #[test]
    fn detects_https_url() {
        let urls = detect("Visit https://example.com for info");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 6);
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
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].col_start, 0);
        assert_eq!(urls[0].spans[0].col_end, 12); // "https://x.co" is 12 chars
    }

    #[test]
    fn url_on_second_line() {
        // vt100 needs \r\n to move to column 0 of the next line
        let urls = detect("first line\r\nhttps://line2.com");
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 1);
        assert_eq!(urls[0].spans[0].col_start, 0);
    }

    #[test]
    fn stops_at_quotes_and_angles() {
        let urls = detect(r#"<https://example.com> and "https://other.com""#);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[1].url, "https://other.com");
    }

    // ── Wrapped URL tests ────────────────────────────────────────────

    #[test]
    fn url_wrapping_across_two_lines() {
        // Use a narrow terminal (40 cols) so the URL wraps.
        // "https://example.com/very/long/path/that/wraps" is 46 chars,
        // which will wrap at col 40 onto the next row.
        let url_text = "https://example.com/very/long/path/that/wraps";
        let urls = detect_with_size(5, 40, url_text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, url_text);
        assert_eq!(urls[0].spans.len(), 2);
        // First span: row 0, cols 0..40
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 0);
        assert_eq!(urls[0].spans[0].col_end, 40);
        // Second span: row 1, cols 0..6 ("wraps" = 5 chars + the 'w' starts at 0)
        assert_eq!(urls[0].spans[1].row, 1);
        assert_eq!(urls[0].spans[1].col_start, 0);
        assert_eq!(urls[0].spans[1].col_end, 5); // "wraps" is 5 chars
    }

    #[test]
    fn url_wrapping_across_three_lines() {
        // Use a 20-col terminal. URL = "https://example.com/a/b/c/d/e/f/g/h/i/j/k"
        // That's 42 chars, wrapping at cols 20, 40 -> 3 rows.
        let url_text = "https://example.com/a/b/c/d/e/f/g/h/i/j/k";
        let urls = detect_with_size(5, 20, url_text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, url_text);
        assert_eq!(urls[0].spans.len(), 3);
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 0);
        assert_eq!(urls[0].spans[0].col_end, 20);
        assert_eq!(urls[0].spans[1].row, 1);
        assert_eq!(urls[0].spans[1].col_start, 0);
        assert_eq!(urls[0].spans[1].col_end, 20);
        assert_eq!(urls[0].spans[2].row, 2);
        assert_eq!(urls[0].spans[2].col_start, 0);
        assert_eq!(urls[0].spans[2].col_end, 1); // "k" = 1 char (the "/" is on row 1)
    }

    #[test]
    fn non_wrapped_lines_dont_merge() {
        // Two separate lines (using \r\n) should NOT merge into a URL.
        // Put a partial URL on line 1 and continuation on line 2.
        let text = "https://example.com/pa\r\nth/rest";
        let urls = detect_with_size(5, 80, text);
        // The first line has "https://example.com/pa" which IS a valid URL
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com/pa");
        // Should be single-span (not merged with "th/rest")
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 0);
    }

    #[test]
    fn wrapped_url_span_coordinates() {
        // "prefix https://example.com/long/path/here" in a 30-col terminal.
        // "prefix " = 7 chars, URL starts at col 7.
        // Row 0: "prefix https://example.com/lon" (30 chars, URL portion = cols 7..30)
        // Row 1: "g/path/here" (URL portion = cols 0..11)
        let text = "prefix https://example.com/long/path/here";
        let urls = detect_with_size(5, 30, text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com/long/path/here");
        assert_eq!(urls[0].spans.len(), 2);
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 7);
        assert_eq!(urls[0].spans[0].col_end, 30);
        assert_eq!(urls[0].spans[1].row, 1);
        assert_eq!(urls[0].spans[1].col_start, 0);
        assert_eq!(urls[0].spans[1].col_end, 11);
    }

    // ── Unicode / multi-byte character tests ─────────────────────────

    #[test]
    fn url_after_unicode_chars_on_same_row() {
        // "▸ " is 2 screen columns but "▸" is 3 bytes in UTF-8.
        // The URL should still be detected at the correct screen column.
        let text = "▸ https://example.com";
        let urls = detect(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[0].spans.len(), 1);
        // "▸" at col 0, " " at col 1, URL starts at col 2
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 2);
        assert_eq!(urls[0].spans[0].col_end, 21); // 2 + 19 chars
    }

    #[test]
    fn url_after_unicode_on_previous_row() {
        // Unicode chars on row 0 should not affect URL detection on row 1.
        // "│ box drawing │" has multi-byte characters.
        let text = "│ box drawing │\r\nhttps://example.com";
        let urls = detect(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com");
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 1);
        assert_eq!(urls[0].spans[0].col_start, 0);
        assert_eq!(urls[0].spans[0].col_end, 19);
    }

    #[test]
    fn url_with_many_unicode_rows_before() {
        // Multiple rows of Unicode content before a URL.
        let text = "─── Header ───\r\n│ Content here │\r\n▸ https://example.com/path";
        let urls = detect(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com/path");
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 2);
        assert_eq!(urls[0].spans[0].col_start, 2);
    }

    // ── Cursor-positioned (tmux redraw) tests ────────────────────────

    #[test]
    fn wrapped_url_via_cursor_positioning() {
        // Simulate tmux redrawing the screen with explicit cursor
        // positioning (CSI row;col H).  The vt100 row_wrapped flag
        // won't be set, but our content heuristic should still detect
        // the wrap because the last cell of row 0 is non-whitespace.
        let cols: u16 = 40;
        let url_text = "https://example.com/very/long/path/that/wraps";
        // Row 0 portion fills all 40 cols
        let row0: &str = &url_text[..cols as usize]; // "https://example.com/very/long/path/that/"
        let row1: &str = &url_text[cols as usize..]; // "wraps"

        // Use CSI sequences to position cursor: \x1b[row;colH
        let input = format!(
            "\x1b[1;1H{}\x1b[2;1H{}",
            row0, row1
        );
        let urls = detect_with_size(5, cols, &input);
        assert_eq!(urls.len(), 1, "should detect one merged URL");
        assert_eq!(urls[0].url, url_text);
        assert_eq!(urls[0].spans.len(), 2);
        assert_eq!(urls[0].spans[0].row, 0);
        assert_eq!(urls[0].spans[0].col_start, 0);
        assert_eq!(urls[0].spans[0].col_end, 40);
        assert_eq!(urls[0].spans[1].row, 1);
        assert_eq!(urls[0].spans[1].col_start, 0);
        assert_eq!(urls[0].spans[1].col_end, 5);
    }

    #[test]
    fn cursor_positioned_non_full_row_no_merge() {
        // If a row doesn't fill all columns, it should NOT merge with the
        // next row, even when using cursor positioning.
        let cols: u16 = 80;
        let input = format!(
            "\x1b[1;1Hhttps://example.com/pa\x1b[2;1Hth/rest"
        );
        let urls = detect_with_size(5, cols, &input);
        // "https://example.com/pa" is only 22 chars in an 80-col terminal,
        // so the last cell of row 0 is whitespace → no merge
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].url, "https://example.com/pa");
        assert_eq!(urls[0].spans.len(), 1);
        assert_eq!(urls[0].spans[0].row, 0);
    }
}
