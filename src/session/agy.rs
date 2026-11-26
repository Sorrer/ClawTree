// Antigravity CLI (`agy`) specifics: reading its state from the screen and
// finding the conversation a running process is in.
//
// Pure functions + /proc reads, free of other crate modules so `lib.rs` can
// include it for integration tests.
//
// What agy 1.5 draws at the bottom of its screen:
//
//   ────────────────────────────────────────────   ← rule
//   > Auto mode: edits & safe commands auto-…      ← input box (placeholder or draft)
//   ────────────────────────────────────────────   ← rule
//   ? for shortcuts            auto · Gemini 4 Neon · high
//
// While a turn runs the footer's left side reads `esc to cancel` and a braille
// spinner line (`⣽  Generating...`) sits above the box. A tool approval
// replaces the box with `Requesting permission for:` and a numbered menu (the
// footer still says `esc to cancel`), and the workspace-trust question is a
// bare menu; both end in an `↑/↓ Navigate` hint. When the user has typed a
// draft the left side of the footer is blank.

use std::path::{Path, PathBuf};

/// What the bottom of an agy screen says about its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    /// A turn is running.
    Working,
    /// At the input box, waiting for the next prompt.
    Idle,
    /// A question is on screen: a tool approval, the trust prompt, a picker.
    NeedsInput,
    /// Nothing recognisable (startup, sign-in, a full-screen view).
    Unknown,
}

/// How many non-empty lines from the bottom the probes inspect.
/// Room for the input box plus agy's sub-agent panel (one row per agent).
const BOTTOM_LINES: usize = 32;

fn bottom_lines(content: &str) -> Vec<&str> {
    content
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(BOTTOM_LINES)
        .collect()
}

/// A full-width rule of `─`: at least 10 characters, nearly all of them `─`.
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let total = t.chars().count();
    total >= 10 && t.chars().filter(|&c| c == '─' || c == '━').count() * 10 >= total * 9
}

/// The footer's left-hand hint (`? for shortcuts`, `esc to cancel`), or ""
/// when it is blank (a draft is being typed).
fn footer_hint(footer: &str) -> &str {
    let t = footer.trim_start();
    if footer.len() - t.len() >= 2 {
        return ""; // indented: only the right-hand status is present
    }
    match t.find("  ") {
        Some(i) => t[..i].trim(),
        None if t.contains(" · ") => "",
        None => t,
    }
}

/// The footer's right-hand status (`auto · Gemini 4 Neon · high`).
fn footer_status(footer: &str) -> Option<&str> {
    let t = footer.trim();
    let right = match t.rfind("  ") {
        Some(i) => t[i..].trim(),
        None => t,
    };
    right.contains(" · ").then_some(right)
}

/// The input box: rule, one or more lines starting with `> ` (or wrapped
/// continuations), rule. It sits right above the footer, or above agy's
/// sub-agent panel (`● Agent(…) · 2m27s` rows closed by another rule).
fn has_input_box(lines: &[&str]) -> bool {
    // lines[0] is the footer; rules are found bottom-up.
    let rules: Vec<usize> = (1..lines.len()).filter(|&i| is_rule(lines[i])).collect();
    if rules.first() != Some(&1) {
        return false;
    }
    rules.windows(2).take(3).any(|w| {
        let (lower, upper) = (w[0], w[1]);
        // The line just under the upper rule must be the prompt.
        let first = lines[upper - 1].trim_start();
        upper > lower + 1 && (first == ">" || first.starts_with("> "))
    })
}

fn mentions_question(lines: &[&str]) -> bool {
    lines.iter().any(|l| {
        l.contains("Requesting permission")
            || l.contains("Do you trust the contents")
            || l.contains("↑/↓ Navigate")
    })
}

/// Classify the visible screen of an agy session.
pub fn screen_state(content: &str) -> ScreenState {
    let lines = bottom_lines(content);
    let Some(footer) = lines.first() else {
        return ScreenState::Unknown;
    };
    let busy = footer_hint(footer).starts_with("esc to cancel");
    // A visible input box means no question is open (approvals and pickers
    // replace it), whatever the transcript above happens to say.
    if has_input_box(&lines) {
        return if busy {
            ScreenState::Working
        } else {
            ScreenState::Idle
        };
    }
    if mentions_question(&lines) {
        return ScreenState::NeedsInput;
    }
    if busy {
        return ScreenState::Working;
    }
    if lines.iter().any(|l| l.contains("↑/↓ Scroll")) {
        // A read-only view the user opened (quota, help, a transcript):
        // nothing is waiting on them.
        return ScreenState::Unknown;
    }
    if footer_status(footer).is_some() {
        // agy's footer without its input box: some other view or question.
        return ScreenState::NeedsInput;
    }
    ScreenState::Unknown
}

/// The agent mode the footer shows: `auto`, `accept-edits`, `plan`, or
/// `default` when the footer shows only model and effort. `None` when no
/// agy footer is visible.
pub fn footer_mode(content: &str) -> Option<String> {
    let lines = bottom_lines(content);
    let status = footer_status(lines.first()?)?;
    let first = status.split(" · ").next()?.trim();
    Some(
        if super::tmux::AGY_MODES.contains(&first) {
            first
        } else {
            "default"
        }
        .to_string(),
    )
}

/// How much background work the footer says is running: sub-agents
/// (`… · 3 subagent(s)`) plus background tasks (`… · 1 task(s) · /tasks`,
/// e.g. a long shell command agy is waiting on).
pub fn running_subagents(content: &str) -> usize {
    let lines = bottom_lines(content);
    let Some(status) = lines.first().and_then(|f| footer_status(f)) else {
        return 0;
    };
    status
        .split(" · ")
        .filter_map(|seg| {
            let (n, rest) = seg.trim().split_once(' ')?;
            (rest.starts_with("subagent") || rest.starts_with("task"))
                .then(|| n.parse::<usize>().ok())
                .flatten()
        })
        .sum()
}

/// agy's data folder: `~/.gemini/antigravity-cli`.
pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli")
}

/// Where agy keeps a conversation: `conversations/<id>.db` (SQLite).
pub fn conversation_path(home: &Path, id: &str) -> PathBuf {
    data_dir(home)
        .join("conversations")
        .join(format!("{}.db", id))
}

pub fn conversation_exists(home: &Path, id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && conversation_path(home, id).is_file()
}

/// The conversation a running agy process is in. For as long as it has a
/// conversation open, agy holds `presence/<id>.lock`; during a turn it also
/// has `conversations/<id>.db` open. With several (a conversation and its
/// sub-agents), the one already known wins, else the lowest fd — the
/// conversation opened first.
pub fn conversation_id_for_pid(home: &Path, pid: u32, current: Option<&str>) -> Option<String> {
    // fd links are resolved paths; ~/.gemini may itself be a symlink.
    let base = data_dir(home);
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    let presence = base.join("presence");
    let conversations = base.join("conversations");
    let mut open: Vec<(u32, String)> = std::fs::read_dir(format!("/proc/{}/fd", pid))
        .ok()?
        .flatten()
        .filter_map(|fd| {
            let n: u32 = fd.file_name().to_string_lossy().parse().ok()?;
            let target = std::fs::read_link(fd.path()).ok()?;
            let parent = target.parent()?;
            let name = target.file_name()?.to_string_lossy().to_string();
            let id = if parent == presence {
                name.strip_suffix(".lock")?
            } else if parent == conversations {
                name.strip_suffix(".db")?
            } else {
                return None;
            };
            (!id.is_empty()).then(|| (n, id.to_string()))
        })
        .collect();
    if let Some(cur) = current {
        if open.iter().any(|(_, id)| id == cur) {
            return Some(cur.to_string());
        }
    }
    open.sort();
    open.into_iter().next().map(|(_, id)| id)
}

// ── Usage (quota) ────────────────────────────────────────────────────────
//
// agy hands its quota to the title command (clawtree's `__agy-title`) on every
// state change, per bucket: `gemini-5h`, `gemini-weekly`, and `3p-5h` /
// `3p-weekly` for third-party models. Each has `remaining_fraction` and
// `reset_in_seconds` (0, with a 1970 `reset_time`, when the bucket is unused).
// The hook saves the latest copy for the clawtree window's Usage panel.

/// Where the hook saves agy's latest quota: `~/.clawtree/agy-usage.json`.
pub fn usage_path(home: &Path) -> PathBuf {
    home.join(".clawtree").join("agy-usage.json")
}

/// Save the quota from an agy status payload (atomically), if it has one.
pub fn save_usage(home: &Path, status: &serde_json::Value, now: u64) {
    let Some(quota) = status
        .get("quota")
        .and_then(|q| q.as_object())
        .filter(|q| !q.is_empty())
    else {
        return;
    };
    let saved = serde_json::json!({
        "updated_at": now,
        "plan_tier": status.get("plan_tier"),
        "quota": quota,
    });
    let path = usage_path(home);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&tmp, saved.to_string()).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Saved usage older than this isn't shown.
const USAGE_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

/// Usage windows from saved quota as `(label, percent used, reset ISO time)`:
/// `agy 5h` / `agy 7d` always, third-party (`agy 3p …`) ones once used.
/// A window whose reset time has passed reads 0% until agy reports again.
pub fn usage_windows(saved: &serde_json::Value, now: u64) -> Vec<(String, f64, Option<String>)> {
    let updated = saved.get("updated_at").and_then(|v| v.as_u64()).unwrap_or(0);
    if now.saturating_sub(updated) > USAGE_MAX_AGE_SECS {
        return Vec::new();
    }
    let Some(quota) = saved.get("quota").and_then(|q| q.as_object()) else {
        return Vec::new();
    };
    let known = [
        ("gemini-5h", "agy 5h", true),
        ("gemini-weekly", "agy 7d", true),
        ("3p-5h", "agy 3p 5h", false),
        ("3p-weekly", "agy 3p 7d", false),
    ];
    let mut out = Vec::new();
    let mut push = |bucket: &serde_json::Value, label: String, always: bool| {
        let remaining = bucket
            .get("remaining_fraction")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);
        let reset_in = bucket.get("reset_in_seconds").and_then(|v| v.as_u64()).unwrap_or(0);
        let used = ((1.0 - remaining) * 100.0).clamp(0.0, 100.0);
        if !always && used <= 0.0 {
            return;
        }
        if reset_in > 0 && now >= updated + reset_in {
            out.push((label, 0.0, None)); // the window has rolled over since
            return;
        }
        let reset = bucket
            .get("reset_time")
            .and_then(|v| v.as_str())
            .filter(|t| reset_in > 0 && !t.starts_with("1970"))
            .map(|t| t.to_string());
        out.push((label, used, reset));
    };
    for (key, label, always) in known {
        if let Some(b) = quota.get(key) {
            push(b, label.to_string(), always);
        }
    }
    let mut others: Vec<&String> = quota
        .keys()
        .filter(|k| !known.iter().any(|(key, _, _)| key == k))
        .collect();
    others.sort();
    for k in others {
        push(&quota[k.as_str()], format!("agy {}", k), false);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str =
        "────────────────────────────────────────────────────────────────────────────────";

    fn screen(lines: &[&str]) -> String {
        let mut s = String::from("  Antigravity CLI 1.5.17\n\n");
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("\n\n");
        s
    }

    #[test]
    fn idle_with_placeholder() {
        let s = screen(&[
            "  The command printed finished, and notes.txt contains hello.",
            RULE,
            "> Auto mode: edits & safe commands auto-approved (shift+tab to cycle)",
            RULE,
            "? for shortcuts                                         auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
        assert_eq!(footer_mode(&s).as_deref(), Some("auto"));
    }

    #[test]
    fn idle_with_empty_prompt_and_default_mode() {
        let s = screen(&[
            RULE,
            ">",
            RULE,
            "? for shortcuts                                                Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
        assert_eq!(footer_mode(&s).as_deref(), Some("default"));
    }

    #[test]
    fn idle_while_typing_a_draft() {
        // The hint disappears while a draft is in the box.
        let s = screen(&[
            RULE,
            "> Run the shell command `sleep 6; echo finished` and then tell me what notes.txt",
            "  contains, in one short sentence.",
            RULE,
            "                                                        plan · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
        assert_eq!(footer_mode(&s).as_deref(), Some("plan"));
    }

    #[test]
    fn working() {
        let s = screen(&[
            "> Run the shell command `sleep 6; echo finished`",
            "○ Running (sleep 6; echo finished) (ctrl+o to expand)",
            "⣷  Running command... 4s",
            RULE,
            ">",
            RULE,
            "esc to cancel                                           auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Working);
    }

    #[test]
    fn tool_approval_needs_input() {
        let s = screen(&[
            "● Ran (echo hi > out.txt && cat out.txt) (ctrl+o to expand)",
            "Command",
            RULE,
            "Requesting permission for:",
            "   echo hi > out.txt && cat out.txt",
            "Run this command?",
            "> 1. Yes, run command",
            "  2. Yes, and always allow in this conversation",
            "  4. No, cancel",
            "  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command",
            "esc to cancel                                                  Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::NeedsInput);
    }

    #[test]
    fn trust_prompt_needs_input() {
        let s = screen(&[
            "Do you trust the contents of this project?",
            "> Yes, I trust this folder",
            "  No, exit",
            "  ↑/↓ Navigate · enter Confirm",
            "                                                        auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::NeedsInput);
    }

    #[test]
    fn unrelated_screens_are_unknown() {
        assert_eq!(screen_state(""), ScreenState::Unknown);
        assert_eq!(screen_state("$ ls\nfoo bar\n$ "), ScreenState::Unknown);
        assert_eq!(footer_mode("$ ls\nfoo bar\n"), None);
    }

    #[test]
    fn conversation_id_from_open_fd() {
        let home = tempfile::tempdir().unwrap();
        let dir = data_dir(home.path()).join("conversations");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let _f = std::fs::File::create(dir.join(format!("{}.db", id))).unwrap();
        let _wal = std::fs::File::create(dir.join(format!("{}.db-wal", id))).unwrap();
        let me = std::process::id();
        assert_eq!(
            conversation_id_for_pid(home.path(), me, None).as_deref(),
            Some(id)
        );
        assert!(conversation_exists(home.path(), id));
        assert!(!conversation_exists(home.path(), "nope"));
        // A second open conversation doesn't displace the known one.
        let other = "00000000-0000-4000-8000-000000000000";
        let _g = std::fs::File::create(dir.join(format!("{}.db", other))).unwrap();
        assert_eq!(
            conversation_id_for_pid(home.path(), me, Some(other)).as_deref(),
            Some(other)
        );
    }

    #[test]
    fn conversation_id_from_presence_lock_through_a_symlinked_gemini_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-gemini");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(real.join("antigravity-cli").join("conversations")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::os::unix::fs::symlink(&real, home.join(".gemini")).unwrap();
        let id = "11111111-2222-4333-8444-555555555555";
        // Between turns only the presence lock is held.
        std::fs::create_dir_all(real.join("antigravity-cli").join("presence")).unwrap();
        let _f = std::fs::File::create(
            data_dir(&home).join("presence").join(format!("{}.lock", id)),
        )
        .unwrap();
        assert_eq!(
            conversation_id_for_pid(&home, std::process::id(), None).as_deref(),
            Some(id)
        );
    }

    #[test]
    fn idle_above_the_subagent_panel() {
        // Seen live (texts changed): agy's own turn is over, three sub-agents work on.
        let s = screen(&[
            "  three sub-agents are reviewing the modules. I'll report back once they finish.",
            RULE,
            ">",
            RULE,
            "  ● Agent(research: API reviewer) · Gemini 4 Neon  Reading the docs… · 2m27s",
            "  ● Agent(research: Storage reviewer) · Gemini 4 Neon  Running command · 2m28s",
            "  ● Agent(research: Tests reviewer) · Gemini 4 Neon  Reading file · 2m29s",
            RULE,
            "? for shortcuts                                      auto · Gemini 4 Neon · high · 3 subagent(s)",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
        assert_eq!(running_subagents(&s), 3);
        assert_eq!(footer_mode(&s).as_deref(), Some("auto"));
    }

    #[test]
    fn no_subagents_by_default() {
        let s = screen(&[
            RULE,
            ">",
            RULE,
            "? for shortcuts                                                auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(running_subagents(&s), 0);
    }

    #[test]
    fn transcript_mentioning_a_question_is_still_idle() {
        let s = screen(&[
            "  The status flips when the screen says Requesting permission for: or ↑/↓ Navigate.",
            RULE,
            ">",
            RULE,
            "? for shortcuts                                                auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
    }

    #[test]
    fn usage_from_a_real_quota_payload() {
        // From agy 1.5.17's title payload.
        let status: serde_json::Value = serde_json::from_str(
            r#"{"plan_tier":"Google AI Ultra","quota":{
                "3p-5h":{"remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z","reset_in_seconds":0},
                "3p-weekly":{"remaining_fraction":1,"reset_time":"1970-01-01T00:00:00Z","reset_in_seconds":0},
                "gemini-5h":{"remaining_fraction":0.96220595,"reset_time":"2026-11-26T12:02:15Z","reset_in_seconds":15421},
                "gemini-weekly":{"remaining_fraction":0.9855365,"reset_time":"2026-12-03T07:02:15Z","reset_in_seconds":602221}}}"#,
        )
        .unwrap();
        let home = tempfile::tempdir().unwrap();
        let now = 1_795_660_000;
        save_usage(home.path(), &status, now);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(usage_path(home.path())).unwrap()).unwrap();
        let w = usage_windows(&saved, now + 60);
        let labels: Vec<&str> = w.iter().map(|x| x.0.as_str()).collect();
        // Unused third-party buckets stay hidden.
        assert_eq!(labels, vec!["agy 5h", "agy 7d"]);
        assert!((w[0].1 - 3.78).abs() < 0.01, "{}", w[0].1);
        assert_eq!(w[0].2.as_deref(), Some("2026-11-26T12:02:15Z"));
        // After the 5h window resets it reads 0% until agy reports again.
        let later = usage_windows(&saved, now + 15_421 + 5);
        assert_eq!(later[0], ("agy 5h".to_string(), 0.0, None));
        assert!(later[1].1 > 1.0);
        // Week-old data isn't shown at all.
        assert!(usage_windows(&saved, now + 8 * 24 * 3600).is_empty());
        // A payload without quota leaves the saved copy alone.
        save_usage(home.path(), &serde_json::json!({"agent_state": "idle"}), now + 99);
        assert!(std::fs::read_to_string(usage_path(home.path())).unwrap().contains("gemini-5h"));
    }

    #[test]
    fn background_tasks_count_as_sub_work() {
        // Seen live: idle, with a shell task listed under the input box.
        let s = screen(&[
            "  rojo build and the lint all passed; waiting for the remaining stages to finish.",
            RULE,
            ">",
            RULE,
            "  ● [02:36:52] cd /work && bash scripts/check.sh --quick running",
            RULE,
            "? for shortcuts                          auto · Gemini 4 Neon · high · 1 task(s) · /tasks",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Idle);
        assert_eq!(running_subagents(&s), 1);
        let both = screen(&[
            RULE,
            ">",
            RULE,
            "? for shortcuts            auto · Gemini 4 Neon · high · 2 subagent(s) · 1 task(s) · /tasks",
        ]);
        assert_eq!(running_subagents(&both), 3);
    }

    #[test]
    fn a_scroll_view_is_not_a_question() {
        let s = screen(&[
            "    Quota available",
            "  │Within each group, models share a weekly limit and a 5-hour limit.",
            "  ↑/↓ Scroll · pgup/pgdown Page · ctrl+end Bottom · ctrl+home Top · esc Close",
            "                                                            auto · Gemini 4 Neon · high",
        ]);
        assert_eq!(screen_state(&s), ScreenState::Unknown);
    }
}
