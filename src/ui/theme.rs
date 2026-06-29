use ratatui::style::Color;
use std::sync::OnceLock;

// ══════════════════════════════════════════════════════════════════════
// Color mode detection & runtime theme
// ══════════════════════════════════════════════════════════════════════

/// Terminal color capability level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Color256,
    Basic,
}

/// Runtime theme holding the 10 RGB-dependent colors.
/// Initialized once at startup via [`init`].
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub sidebar_sel_bg: Color,
    pub sidebar_active_bg: Color,
    pub sidebar_sel_active_bg: Color,
    pub sidebar_hover_bg: Color,
    /// Background for a session with unread output the user hasn't viewed yet.
    pub sidebar_unread_bg: Color,
    pub status_bar_bg: Color,
    pub scrollbar_track: Color,
    pub border_focused_terminal: Color,
    pub mode_terminal_bg: Color,
    pub brand_claw: Color,
    pub brand_name: Color,
}

impl Theme {
    /// Build a theme for the given color mode.
    pub fn for_mode(mode: ColorMode) -> Self {
        match mode {
            ColorMode::TrueColor => Self {
                sidebar_sel_bg: Color::Rgb(50, 50, 60),
                sidebar_active_bg: Color::Rgb(30, 50, 35),
                sidebar_sel_active_bg: Color::Rgb(40, 55, 50),
                sidebar_hover_bg: Color::Rgb(40, 40, 48),
                sidebar_unread_bg: Color::Rgb(40, 80, 48),
                status_bar_bg: Color::Rgb(30, 30, 30),
                scrollbar_track: Color::Rgb(40, 40, 40),
                border_focused_terminal: Color::Rgb(220, 80, 30),
                mode_terminal_bg: Color::Rgb(220, 80, 30),
                brand_claw: Color::Rgb(187, 134, 252),
                brand_name: Color::Rgb(149, 117, 205),
            },
            ColorMode::Color256 => Self {
                sidebar_sel_bg: Color::Indexed(236),
                sidebar_active_bg: Color::Indexed(22),
                sidebar_sel_active_bg: Color::Indexed(23),
                sidebar_hover_bg: Color::Indexed(237),
                sidebar_unread_bg: Color::Indexed(28),
                status_bar_bg: Color::Indexed(235),
                scrollbar_track: Color::Indexed(236),
                border_focused_terminal: Color::Indexed(166),
                mode_terminal_bg: Color::Indexed(166),
                brand_claw: Color::Indexed(141),
                brand_name: Color::Indexed(140),
            },
            ColorMode::Basic => Self {
                sidebar_sel_bg: Color::DarkGray,
                sidebar_active_bg: Color::DarkGray,
                sidebar_sel_active_bg: Color::DarkGray,
                sidebar_hover_bg: Color::DarkGray,
                sidebar_unread_bg: Color::Green,
                status_bar_bg: Color::DarkGray,
                scrollbar_track: Color::DarkGray,
                border_focused_terminal: Color::Red,
                mode_terminal_bg: Color::Red,
                brand_claw: Color::Magenta,
                brand_name: Color::Magenta,
            },
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Detect terminal color capability from environment variables.
pub fn detect_color_mode() -> ColorMode {
    // 1. Explicit override via CLAWTREE_COLOR_MODE
    if let Ok(val) = std::env::var("CLAWTREE_COLOR_MODE") {
        match val.to_lowercase().as_str() {
            "truecolor" | "true" | "24bit" => return ColorMode::TrueColor,
            "256" | "256color" => return ColorMode::Color256,
            "basic" | "16" | "ansi" => return ColorMode::Basic,
            _ => {} // fall through to auto-detection
        }
    }

    // 2. COLORTERM — set by most modern terminals
    if let Ok(val) = std::env::var("COLORTERM") {
        match val.to_lowercase().as_str() {
            "truecolor" | "24bit" => return ColorMode::TrueColor,
            _ => {}
        }
    }

    // 3. TERM — check for 256color suffix
    if let Ok(val) = std::env::var("TERM") {
        if val.contains("256color") {
            return ColorMode::Color256;
        }
    }

    // 4. Default: 256-color (safe middle ground)
    ColorMode::Color256
}

/// Initialize the global theme. Call once at startup before any UI code.
/// If `mode` is `None`, auto-detects from environment.
pub fn init(mode: Option<ColorMode>) {
    let mode = mode.unwrap_or_else(detect_color_mode);
    let _ = THEME.set(Theme::for_mode(mode));
}

/// Get the active theme. Panics if [`init`] was never called.
pub fn get() -> &'static Theme {
    THEME
        .get()
        .expect("theme::init() must be called before theme::get()")
}

// ══════════════════════════════════════════════════════════════════════
// Static constants (non-RGB, unchanged)
// ══════════════════════════════════════════════════════════════════════

// ── Panel borders ────────────────────────────────────────────────────
pub const BORDER_FOCUSED_SIDEBAR: Color = Color::Cyan;
pub const BORDER_FOCUSED_PROMPT_QUEUE: Color = Color::Green;
pub const BORDER_UNFOCUSED: Color = Color::DarkGray;

// ── Sidebar ──────────────────────────────────────────────────────────
/// Maximum sidebar width in columns (sized to fit usage panel content).
/// Usage line: " 7d: 100% ██████ Feb 19 16:00 EST" ≈ 34 inner + 2 border + 2 pad = 38 + extra pad.
pub const SIDEBAR_MAX_WIDTH: u16 = 42;

// ── Mode badge backgrounds ───────────────────────────────────────────
pub const MODE_NORMAL_BG: Color = Color::Cyan;
pub const MODE_DIALOG_BG: Color = Color::Yellow;

// ── Dialog borders (semantic) ────────────────────────────────────────
pub const DIALOG_CREATION: Color = Color::Green;
pub const DIALOG_DESTRUCTIVE: Color = Color::Red;
pub const DIALOG_WARNING: Color = Color::Yellow;
pub const DIALOG_NEUTRAL: Color = Color::Cyan;

// ── File status colors ───────────────────────────────────────────────
pub const FILE_UNTRACKED: Color = Color::Blue;
pub const FILE_MODIFIED: Color = Color::Yellow;
pub const FILE_DELETED: Color = Color::Red;
pub const FILE_ADDED: Color = Color::Green;
pub const FILE_DEFAULT: Color = Color::White;

// ── Status message severity ──────────────────────────────────────────
pub const STATUS_INFO: Color = Color::Yellow;
pub const STATUS_SUCCESS: Color = Color::Green;
pub const STATUS_WARNING: Color = Color::Yellow;
pub const STATUS_ERROR: Color = Color::Red;
pub const STATUS_FADED: Color = Color::DarkGray;

// ── Scrollbar ────────────────────────────────────────────────────────
pub const SCROLLBAR_THUMB: Color = Color::DarkGray;

// ── Spinner ──────────────────────────────────────────────────────────
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ── Agent status colors ─────────────────────────────────────────
pub const AGENT_NEEDS_INPUT: Color = Color::Cyan;
pub const AGENT_PLANNING: Color = Color::Indexed(141); // light purple
pub const AGENT_RATE_LIMITED: Color = Color::Red;

// ── Brand logo (big) ────────────────────────────────────────────
//  Three claw marks + box-drawing "CLAWTREE"
pub const LOGO_BIG: &[&str] = &[
    "  ╱ ╱ ╱   ╔═╗ ╦   ╔═╗ ╦ ╦ ╔╦╗ ╦═╗ ╔═╗ ╔═╗",
    " ╱ ╱ ╱    ║   ║   ╠═╣ ║║║  ║  ╠╦╝ ║╣  ║╣ ",
    "╱ ╱ ╱     ╚═╝ ╩═╝ ╩ ╩ ╚╩╝  ╩  ╩╚═ ╚═╝ ╚═╝",
];

// ── Brand logo (small, inline for status bar) ───────────────────
pub const LOGO_SMALL: &str = "╱╱╱";
