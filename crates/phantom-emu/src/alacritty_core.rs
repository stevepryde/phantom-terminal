//! `alacritty_terminal`-backed [`VtCore`] implementation.
//!
//! Owns a [`Term`] plus an ANSI [`Processor`](ansi::Processor); PTY bytes are
//! pushed through the processor, and the visible grid is read out into our own
//! [`Snapshot`] type. Terminal query replies (DSR, DA, …) surface as
//! [`Event::PtyWrite`] events, which we buffer for the caller to forward to the
//! PTY. Single-threaded by design: the terminal lives on the UI thread and the
//! PTY reader hands bytes across a channel.

use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    self, Color as AnsiColor, CursorShape as AnsiCursorShape, CursorStyle as AnsiCursorStyle,
    NamedColor as AnsiNamed,
};

use crate::{
    CellAttrs, CellColor, CursorShape, CursorState, MouseMode, MouseProtocol, NamedColor,
    ScrollState, SelSide, SelectionKind, SnapCell, Snapshot, VtCore,
};

const PHANTOM_SEMANTIC_ESCAPE_CHARS: &str = "\t !\"#$%&'()*+,./:;<=>?@[\\]^`{|}~│";

/// Buffers bytes the terminal wants written back to the PTY. `send_event` takes
/// `&self`, so interior mutability is required; `Rc<RefCell<…>>` is fine because
/// the terminal is confined to one thread.
#[derive(Clone, Default)]
struct ResponseSink(Rc<RefCell<Vec<u8>>>);

impl EventListener for ResponseSink {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.borrow_mut().extend_from_slice(text.as_bytes());
        }
        // Title/Bell/clipboard/etc. are handled at the app layer (Phase 3).
    }
}

/// Cell dimensions handed to `Term` (scrollback depth comes from `Config`).
#[derive(Clone, Copy)]
struct TermDimensions {
    columns: usize,
    screen_lines: usize,
}

impl TermDimensions {
    fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns: columns.max(1),
            screen_lines: screen_lines.max(1),
        }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct AlacrittyCore {
    term: Term<ResponseSink>,
    parser: ansi::Processor,
    responses: Rc<RefCell<Vec<u8>>>,
    size: TermDimensions,
}

impl AlacrittyCore {
    /// Create a terminal of `rows` x `cols` cells with `scrollback_lines` of
    /// history. `default_cursor` is the shape used until an application changes
    /// it via DECSCUSR.
    pub fn new(rows: u16, cols: u16, scrollback_lines: u32, default_cursor: CursorShape) -> Self {
        let size = TermDimensions::new(cols as usize, rows as usize);
        let responses = Rc::new(RefCell::new(Vec::new()));

        let config = Config {
            scrolling_history: scrollback_lines as usize,
            semantic_escape_chars: PHANTOM_SEMANTIC_ESCAPE_CHARS.to_string(),
            default_cursor_style: AnsiCursorStyle {
                shape: to_ansi_cursor_shape(default_cursor),
                blinking: false,
            },
            ..Config::default()
        };

        let term = Term::new(config, &size, ResponseSink(Rc::clone(&responses)));
        Self {
            term,
            parser: ansi::Processor::new(),
            responses,
            size,
        }
    }
}

impl VtCore for AlacrittyCore {
    fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.size = TermDimensions::new(cols as usize, rows as usize);
        self.term.resize(self.size);
    }

    fn size(&self) -> (u16, u16) {
        (self.size.screen_lines as u16, self.size.columns as u16)
    }

    fn snapshot(&self) -> Snapshot {
        let (rows, cols) = self.size();
        let (rows_us, cols_us) = (rows as usize, cols as usize);
        let mut cells = vec![SnapCell::default(); rows_us * cols_us];

        let content = self.term.renderable_content();
        // `display_iter` yields absolute grid lines; viewport row 0 sits at
        // line `-display_offset`, so add the offset to get a 0-based row.
        let offset = content.display_offset as i32;
        let selection = content.selection;

        for indexed in content.display_iter {
            let cell = indexed.cell;
            // The trailing half of a wide character carries no glyph of its own.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row as usize >= rows_us || col >= cols_us {
                continue;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let selected = selection.is_some_and(|range| range.contains(indexed.point));
            cells[row as usize * cols_us + col] = SnapCell {
                c: cell.c,
                fg: map_color(cell.fg),
                bg: map_color(cell.bg),
                attrs: map_attrs(cell.flags),
                width,
                selected,
            };
        }

        let cur = content.cursor;
        let crow = cur.point.line.0 + offset;
        let ccol = cur.point.column.0;
        let visible = !matches!(cur.shape, AnsiCursorShape::Hidden)
            && crow >= 0
            && (crow as usize) < rows_us
            && ccol < cols_us;
        let cursor = CursorState {
            row: crow.clamp(0, rows_us.saturating_sub(1) as i32) as usize,
            col: ccol.min(cols_us.saturating_sub(1)),
            shape: map_cursor_shape(cur.shape),
            visible,
        };

        Snapshot {
            rows,
            cols,
            cells,
            cursor,
            scroll: self.scroll_state(),
        }
    }

    fn scroll_state(&self) -> ScrollState {
        let grid = self.term.grid();
        ScrollState {
            offset: grid.display_offset(),
            history: grid.total_lines().saturating_sub(grid.screen_lines()),
            viewport_rows: self.size.screen_lines,
        }
    }

    fn take_pty_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses.borrow_mut())
    }

    fn application_cursor_keys(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    fn scroll_to_offset(&mut self, offset: usize) {
        let current = self.term.grid().display_offset();
        let delta = offset as i64 - current as i64;
        let delta = delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        self.term.scroll_display(Scroll::Delta(delta));
    }

    fn selection_start_kind(&mut self, row: usize, col: usize, side: SelSide, kind: SelectionKind) {
        let point = self.viewport_point(row, col);
        self.term.selection = Some(Selection::new(
            map_selection_kind(kind),
            point,
            map_side(side),
        ));
    }

    fn selection_update(&mut self, row: usize, col: usize, side: SelSide) {
        let point = self.viewport_point(row, col);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, map_side(side));
        }
    }

    fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    fn mouse_mode(&self) -> MouseMode {
        let mode = self.term.mode();
        let protocol = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseProtocol::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseProtocol::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseProtocol::Click
        } else {
            MouseProtocol::Off
        };
        MouseMode {
            protocol,
            sgr: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }
}

impl AlacrittyCore {
    /// Convert a viewport cell `(row, col)` into an absolute grid point,
    /// accounting for the current scrollback offset.
    fn viewport_point(&self, row: usize, col: usize) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let max_col = self.size.columns.saturating_sub(1);
        Point::new(Line(row as i32 - offset), Column(col.min(max_col)))
    }
}

fn map_side(side: SelSide) -> Side {
    match side {
        SelSide::Left => Side::Left,
        SelSide::Right => Side::Right,
    }
}

fn map_selection_kind(kind: SelectionKind) -> SelectionType {
    match kind {
        SelectionKind::Simple => SelectionType::Simple,
        SelectionKind::Semantic => SelectionType::Semantic,
        SelectionKind::Lines => SelectionType::Lines,
    }
}

fn map_color(color: AnsiColor) -> CellColor {
    match color {
        AnsiColor::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => CellColor::Indexed(i),
        AnsiColor::Named(named) => CellColor::Named(map_named(named)),
    }
}

fn map_named(named: AnsiNamed) -> NamedColor {
    use AnsiNamed::*;
    match named {
        Black => NamedColor::Black,
        Red => NamedColor::Red,
        Green => NamedColor::Green,
        Yellow => NamedColor::Yellow,
        Blue => NamedColor::Blue,
        Magenta => NamedColor::Magenta,
        Cyan => NamedColor::Cyan,
        White => NamedColor::White,
        BrightBlack => NamedColor::BrightBlack,
        BrightRed => NamedColor::BrightRed,
        BrightGreen => NamedColor::BrightGreen,
        BrightYellow => NamedColor::BrightYellow,
        BrightBlue => NamedColor::BrightBlue,
        BrightMagenta => NamedColor::BrightMagenta,
        BrightCyan => NamedColor::BrightCyan,
        BrightWhite => NamedColor::BrightWhite,
        Background => NamedColor::Background,
        Cursor => NamedColor::Cursor,
        // Foreground, and the dim/bright-foreground special slots, all render as
        // the default foreground until the renderer grows a fuller palette.
        _ => NamedColor::Foreground,
    }
}

fn map_attrs(flags: Flags) -> CellAttrs {
    CellAttrs {
        bold: flags.intersects(Flags::BOLD | Flags::DIM_BOLD),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.contains(Flags::UNDERLINE),
        inverse: flags.contains(Flags::INVERSE),
        dim: flags.contains(Flags::DIM),
        strikeout: flags.contains(Flags::STRIKEOUT),
        hidden: flags.contains(Flags::HIDDEN),
    }
}

fn map_cursor_shape(shape: AnsiCursorShape) -> CursorShape {
    match shape {
        AnsiCursorShape::Beam => CursorShape::Beam,
        AnsiCursorShape::Underline => CursorShape::Underline,
        // Block, HollowBlock, Hidden -> Block; visibility is tracked separately.
        _ => CursorShape::Block,
    }
}

fn to_ansi_cursor_shape(shape: CursorShape) -> AnsiCursorShape {
    match shape {
        CursorShape::Block => AnsiCursorShape::Block,
        CursorShape::Underline => AnsiCursorShape::Underline,
        CursorShape::Beam => AnsiCursorShape::Beam,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(rows: u16, cols: u16, scrollback: u32) -> AlacrittyCore {
        AlacrittyCore::new(rows, cols, scrollback, CursorShape::Block)
    }

    #[test]
    fn writes_plain_text_into_the_grid() {
        let mut term = core(24, 80, 1000);
        term.advance(b"hello");
        let snap = term.snapshot();
        assert_eq!(snap.row_text(0), "hello");
        assert_eq!((snap.cursor.row, snap.cursor.col), (0, 5));
        assert!(snap.cursor.visible);
    }

    #[test]
    fn carriage_return_newline_advances_row() {
        let mut term = core(24, 80, 1000);
        term.advance(b"ab\r\ncd");
        let snap = term.snapshot();
        assert_eq!(snap.row_text(0), "ab");
        assert_eq!(snap.row_text(1), "cd");
    }

    #[test]
    fn autowraps_text_past_the_right_margin() {
        let mut term = core(4, 4, 100);
        term.advance(b"abcdef");
        let snap = term.snapshot();
        assert_eq!(snap.row_text(0), "abcd");
        assert_eq!(snap.row_text(1), "ef");
    }

    #[test]
    fn reflow_rejoins_wrapped_line_when_widened() {
        let mut term = core(4, 4, 100);
        term.advance(b"abcdef"); // autowrap sets WRAPLINE on row 0
        assert_eq!(term.snapshot().row_text(1), "ef");

        term.resize(4, 10);
        let snap = term.snapshot();
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.row_text(0), "abcdef");
    }

    #[test]
    fn resize_updates_reported_dimensions() {
        let mut term = core(10, 20, 100);
        term.resize(30, 100);
        assert_eq!(term.size(), (30, 100));
        let snap = term.snapshot();
        assert_eq!((snap.rows, snap.cols), (30, 100));
        assert_eq!(snap.cells.len(), 30 * 100);
    }

    #[test]
    fn wide_characters_report_width_two() {
        let mut term = core(4, 10, 100);
        term.advance("世".as_bytes()); // a fullwidth CJK glyph
        let snap = term.snapshot();
        assert_eq!(snap.cell(0, 0).unwrap().c, '世');
        assert_eq!(snap.cell(0, 0).unwrap().width, 2);
        // The spacer column stays a blank default cell.
        assert_eq!(snap.cell(0, 1).unwrap().c, ' ');
    }

    #[test]
    fn honors_configured_default_cursor_shape() {
        let term = AlacrittyCore::new(24, 80, 100, CursorShape::Beam);
        assert_eq!(term.snapshot().cursor.shape, CursorShape::Beam);
    }

    #[test]
    fn device_status_report_produces_pty_response() {
        let mut term = core(24, 80, 100);
        // DSR 6n: "report cursor position" -> ESC [ row ; col R
        term.advance(b"\x1b[6n");
        let out = term.take_pty_output();
        assert!(!out.is_empty(), "expected a DSR reply");
        assert_eq!(out[0], 0x1b);
        assert_eq!(*out.last().unwrap(), b'R');
        // Draining clears the buffer.
        assert!(term.take_pty_output().is_empty());
    }

    #[test]
    fn tracks_application_cursor_keys_mode() {
        let mut term = core(24, 80, 100);
        assert!(!term.application_cursor_keys());
        term.advance(b"\x1b[?1h"); // DECCKM set
        assert!(term.application_cursor_keys());
        term.advance(b"\x1b[?1l"); // DECCKM reset
        assert!(!term.application_cursor_keys());
    }

    #[test]
    fn selection_yields_text_and_clears() {
        let mut term = core(4, 10, 100);
        term.advance(b"hello");
        term.selection_start(0, 0, SelSide::Left);
        term.selection_update(0, 4, SelSide::Right);
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
        // Selected cells are flagged in the snapshot.
        assert!(term.snapshot().cell(0, 0).unwrap().selected);

        term.selection_clear();
        assert!(term.selection_text().is_none());
        assert!(!term.snapshot().cell(0, 0).unwrap().selected);
    }

    #[test]
    fn semantic_selection_keeps_dash_and_underscore_inside_words() {
        let mut term = core(4, 30, 100);
        term.advance(b"foo-bar_baz.qux");

        term.selection_start_kind(0, 4, SelSide::Left, SelectionKind::Semantic);
        assert_eq!(term.selection_text().as_deref(), Some("foo-bar_baz"));

        term.selection_start_kind(0, 12, SelSide::Left, SelectionKind::Semantic);
        assert_eq!(term.selection_text().as_deref(), Some("qux"));
    }

    #[test]
    fn line_selection_selects_the_whole_line() {
        let mut term = core(4, 20, 100);
        term.advance(b"first\r\nsecond line");

        term.selection_start_kind(1, 4, SelSide::Left, SelectionKind::Lines);

        assert_eq!(term.selection_text().as_deref(), Some("second line\n"));
    }

    #[test]
    fn scroll_back_into_history_changes_viewport() {
        let mut term = core(2, 10, 100);
        for i in 0..20 {
            term.advance(format!("line{i:02}\r\n").as_bytes());
        }
        let bottom = term.snapshot().row_text(0);
        term.scroll(5);
        let scrolled = term.snapshot().row_text(0);
        assert_ne!(bottom, scrolled, "scrolling back should change the top row");
    }

    #[test]
    fn scroll_state_reports_offset_and_history() {
        let mut term = core(2, 10, 100);
        for i in 0..20 {
            term.advance(format!("line{i:02}\r\n").as_bytes());
        }

        let at_bottom = term.scroll_state();
        assert_eq!(at_bottom.offset, 0);
        assert!(at_bottom.history > 0);
        assert_eq!(at_bottom.viewport_rows, 2);

        term.scroll_to_offset(3);
        let scrolled = term.scroll_state();
        assert_eq!(scrolled.offset, 3);
        assert_eq!(scrolled.history, at_bottom.history);
    }

    #[test]
    fn mouse_and_paste_modes_track_the_terminal() {
        let mut term = core(24, 80, 100);
        assert!(!term.mouse_mode().reports());
        assert!(!term.bracketed_paste());

        term.advance(b"\x1b[?1000h"); // X10 mouse report (click)
        term.advance(b"\x1b[?1006h"); // SGR encoding
        term.advance(b"\x1b[?2004h"); // bracketed paste
        let mode = term.mouse_mode();
        assert!(mode.reports());
        assert!(mode.sgr);
        assert!(term.bracketed_paste());
    }
}
