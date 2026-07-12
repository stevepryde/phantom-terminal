//! Phantom Terminal VT core.
//!
//! Turns a stream of PTY bytes into a renderable grid [`Snapshot`], and encodes
//! key/input events back into PTY bytes. The concrete terminal model sits behind
//! the [`VtCore`] trait so the engine is swappable: today it is backed by
//! `alacritty_terminal` (the architecture Zed uses — borrow a battle-tested VT
//! core, own the renderer), and a future libghostty-vt backend could drop in
//! without `phantom-gfx`/`phantom-app` noticing.
//!
//! The snapshot/colour/attribute types here are deliberately *our own*, not
//! re-exports of the backend's, so the renderer never depends on engine
//! internals.

mod alacritty_core;
pub mod keys;

pub use alacritty_core::AlacrittyCore;
pub use keys::{encode_key, encode_mouse_legacy, encode_mouse_sgr, Key, Modifiers};

/// A terminal engine: feed it PTY output, resize it, and read back a renderable
/// snapshot plus any bytes the terminal wants written to the PTY (query replies).
pub trait VtCore {
    /// Feed a chunk of PTY output through the parser into the terminal model.
    fn advance(&mut self, bytes: &[u8]);

    /// Resize the terminal (and reflow), in character cells.
    fn resize(&mut self, rows: u16, cols: u16);

    /// Current size in cells, as `(rows, cols)`.
    fn size(&self) -> (u16, u16);

    /// Snapshot the visible viewport for rendering.
    fn snapshot(&self) -> Snapshot;

    /// Current scrollback position. `offset = 0` means the viewport is at the
    /// live prompt; larger offsets move back into history.
    fn scroll_state(&self) -> ScrollState;

    /// Bytes the terminal has produced for the PTY (e.g. responses to device
    /// queries). Draining clears the internal buffer. Callers forward these to
    /// the PTY writer.
    fn take_pty_output(&mut self) -> Vec<u8>;

    /// Whether the application has enabled "application cursor keys" (DECCKM),
    /// which changes how arrow/navigation keys must be encoded.
    fn application_cursor_keys(&self) -> bool;

    /// Scroll the viewport by `delta` lines (positive scrolls back into
    /// history, negative toward the prompt).
    fn scroll(&mut self, delta: i32);

    /// Move the viewport to an absolute scrollback offset.
    fn scroll_to_offset(&mut self, offset: usize);

    /// Begin a simple text selection at the given viewport cell.
    fn selection_start(&mut self, row: usize, col: usize, side: SelSide) {
        self.selection_start_kind(row, col, side, SelectionKind::Simple);
    }

    /// Begin a text selection at the given viewport cell with the requested
    /// expansion behavior.
    fn selection_start_kind(&mut self, row: usize, col: usize, side: SelSide, kind: SelectionKind);

    /// Extend the active selection to the given viewport cell.
    fn selection_update(&mut self, row: usize, col: usize, side: SelSide);

    /// Clear any active selection.
    fn selection_clear(&mut self);

    /// The selected text, if any.
    fn selection_text(&self) -> Option<String>;

    /// The selected terminal-cell range, in absolute scrollback coordinates.
    ///
    /// This is intended for capturing a stable selection-only search scope.
    fn selection_range(&self) -> Option<SearchRange>;

    /// Search the finite in-memory buffer in stable buffer order.
    fn search_scrollback(
        &self,
        query: &str,
        options: SearchOptions,
        range: Option<SearchRange>,
    ) -> Result<Vec<SearchMatch>, SearchError>;

    /// Set the temporary active find match and scroll it into view.
    ///
    /// This does not replace or mutate the user's ordinary terminal selection.
    fn set_active_search_match(&mut self, search_match: Option<SearchMatch>);

    /// The application's requested mouse-reporting mode.
    fn mouse_mode(&self) -> MouseMode;

    /// Whether bracketed-paste mode is enabled (paste should be wrapped in
    /// `ESC [ 200 ~` / `ESC [ 201 ~`).
    fn bracketed_paste(&self) -> bool;
}

/// A colour as the terminal model reports it. The renderer resolves `Named` and
/// `Indexed` against the active theme/palette; `Rgb` is truecolour passed
/// through. Keeping this independent of the backend's colour enum is the point
/// of the abstraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    Named(NamedColor),
    /// 256-colour palette index (0..=255).
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The 16 ANSI slots plus the special foreground/background/cursor roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Foreground,
    Background,
    Cursor,
}

/// Per-cell rendering attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
    pub strikeout: bool,
    pub hidden: bool,
}

/// One rendered grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapCell {
    pub c: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub attrs: CellAttrs,
    /// Cell width in columns: 1 for normal glyphs, 2 for wide (CJK, emoji).
    /// The renderer draws a wide glyph across two cell widths; the trailing
    /// spacer column is left as a blank default cell.
    pub width: u8,
    /// True when the cell is within the active text selection.
    pub selected: bool,
    /// True when the cell is within the active find match.
    pub search_match: bool,
}

impl Default for SnapCell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: CellColor::Named(NamedColor::Foreground),
            bg: CellColor::Named(NamedColor::Background),
            attrs: CellAttrs::default(),
            width: 1,
            selected: false,
            search_match: false,
        }
    }
}

/// A cell in the terminal's finite buffer, using Alacritty-compatible absolute
/// line coordinates. History lines are negative and visible screen lines start
/// at zero when the viewport is at the live prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BufferPoint {
    pub line: i32,
    pub column: usize,
}

/// An inclusive, ordered terminal-cell range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchRange {
    pub start: BufferPoint,
    pub end: BufferPoint,
}

impl SearchRange {
    pub fn new(a: BufferPoint, b: BufferPoint) -> Self {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        Self { start, end }
    }

    pub fn contains(self, point: BufferPoint) -> bool {
        self.start <= point && point <= self.end
    }
}

/// Independent constraints applied to literal or regular-expression search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// One inclusive match in terminal-cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub range: SearchRange,
}

/// A search query which could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    InvalidRegex(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRegex(message) => {
                write!(formatter, "invalid regular expression: {message}")
            }
        }
    }
}

impl std::error::Error for SearchError {}

/// Which half of a cell a selection anchor is on (affects which cell the anchor
/// includes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelSide {
    Left,
    Right,
}

/// How a new selection should expand from its anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Select exactly the dragged cell span.
    Simple,
    /// Expand to semantic word boundaries.
    Semantic,
    /// Expand to full logical lines.
    Lines,
}

/// The mouse-reporting protocol the running application has requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocol {
    /// No reporting; the UI owns the mouse (selection, scroll).
    Off,
    /// Report button press/release only.
    Click,
    /// Report press/release and drag motion (motion while a button is held).
    Drag,
    /// Report all motion.
    Motion,
}

/// Mouse-reporting state queried from the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseMode {
    pub protocol: MouseProtocol,
    /// Whether to encode events in SGR form (`CSI < … M/m`) vs legacy X10.
    pub sgr: bool,
}

impl MouseMode {
    pub fn reports(&self) -> bool {
        self.protocol != MouseProtocol::Off
    }
}

/// Cursor shape, mapped from the terminal's reported style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
}

/// Cursor position (in viewport cell coordinates) and presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// False when the cursor is hidden or scrolled out of the viewport.
    pub visible: bool,
}

/// Scrollbar-facing view of the terminal's scrollback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollState {
    /// Lines the viewport is currently scrolled back from the live prompt.
    pub offset: usize,
    /// Total lines currently available above the live viewport.
    pub history: usize,
    /// Visible terminal rows.
    pub viewport_rows: usize,
}

impl ScrollState {
    pub fn is_scrollable(self) -> bool {
        self.history > 0 && self.viewport_rows > 0
    }
}

/// An immutable, renderer-facing view of the visible terminal viewport.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub rows: u16,
    pub cols: u16,
    /// Row-major `rows * cols` cells of the visible viewport.
    pub cells: Vec<SnapCell>,
    pub cursor: CursorState,
    pub scroll: ScrollState,
}

impl Snapshot {
    /// The cell at `(row, col)`, or `None` if out of bounds.
    pub fn cell(&self, row: usize, col: usize) -> Option<&SnapCell> {
        if row >= self.rows as usize || col >= self.cols as usize {
            return None;
        }
        self.cells.get(row * self.cols as usize + col)
    }

    /// The text of one viewport row, trailing blanks trimmed. Intended for tests
    /// and debugging, not the render path.
    pub fn row_text(&self, row: usize) -> String {
        let cols = self.cols as usize;
        if row >= self.rows as usize {
            return String::new();
        }
        let start = row * cols;
        let line: String = self.cells[start..start + cols]
            .iter()
            .map(|c| c.c)
            .collect();
        line.trim_end().to_string()
    }
}
