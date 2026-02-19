use ratatui::style::Color;

// ── Panel borders ────────────────────────────────────────────────────
pub const BORDER_FOCUSED_SIDEBAR: Color = Color::Cyan;
pub const BORDER_FOCUSED_TERMINAL: Color = Color::Rgb(220, 80, 30);
pub const BORDER_FOCUSED_PROMPT_QUEUE: Color = Color::Green;
pub const BORDER_UNFOCUSED: Color = Color::DarkGray;

// ── Sidebar ──────────────────────────────────────────────────────────
/// Cursor/selection highlight (blue-tint).
pub const SIDEBAR_SEL_BG: Color = Color::Rgb(50, 50, 60);
/// Active session highlight (green-tint).
pub const SIDEBAR_ACTIVE_BG: Color = Color::Rgb(30, 50, 35);
/// Both selected and active.
pub const SIDEBAR_SEL_ACTIVE_BG: Color = Color::Rgb(40, 55, 50);

// ── Status bar ───────────────────────────────────────────────────────
pub const STATUS_BAR_BG: Color = Color::Rgb(30, 30, 30);

// ── Mode badge backgrounds ───────────────────────────────────────────
pub const MODE_NORMAL_BG: Color = Color::Cyan;
pub const MODE_TERMINAL_BG: Color = Color::Rgb(220, 80, 30);
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
pub const SCROLLBAR_TRACK: Color = Color::Rgb(40, 40, 40);

// ── Spinner ──────────────────────────────────────────────────────────
pub const SPINNER_COLOR: Color = Color::Yellow;
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ── Mini mode ───────────────────────────────────────────────────
pub const MINI_SELECTED_BG: Color = Color::Rgb(50, 50, 60);
pub const MINI_DRILLDOWN_HEADER_BG: Color = Color::Rgb(60, 30, 60);
pub const MODE_MINI_BG: Color = Color::Magenta;
pub const AGENT_WORKING: Color = Color::Yellow;
pub const AGENT_IDLE: Color = Color::Green;
pub const AGENT_NEEDS_INPUT: Color = Color::Cyan;
pub const AGENT_EXITED: Color = Color::DarkGray;

// ── Brand ───────────────────────────────────────────────────────
pub const BRAND_CLAW: Color = Color::Rgb(90, 195, 90);
pub const BRAND_NAME: Color = Color::Rgb(65, 150, 65);

// ── Brand logo (big) ────────────────────────────────────────────
//  Three claw marks + box-drawing "CLAWTREE"
pub const LOGO_BIG: &[&str] = &[
    "  ╱ ╱ ╱   ╔═╗ ╦   ╔═╗ ╦ ╦ ╔╦╗ ╦═╗ ╔═╗ ╔═╗",
    " ╱ ╱ ╱    ║   ║   ╠═╣ ║║║  ║  ╠╦╝ ║╣  ║╣ ",
    "╱ ╱ ╱     ╚═╝ ╩═╝ ╩ ╩ ╚╩╝  ╩  ╩╚═ ╚═╝ ╚═╝",
];

// ── Brand logo (small, inline for status bar) ───────────────────
pub const LOGO_SMALL: &str = "╱╱╱";
