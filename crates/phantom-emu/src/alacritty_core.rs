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
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    self, Color as AnsiColor, CursorShape as AnsiCursorShape, CursorStyle as AnsiCursorStyle,
    NamedColor as AnsiNamed,
};

use crate::{
    BufferPoint, CellAttrs, CellColor, CursorShape, CursorState, MouseMode, MouseProtocol,
    NamedColor, ScrollState, SearchError, SearchMatch, SearchOptions, SearchRange, SelSide,
    SelectionKind, SnapCell, Snapshot, VtCore,
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
    active_search_match: Option<SearchRange>,
}

impl AlacrittyCore {
    /// Create a terminal of `rows` x `cols` cells with `scrollback_lines` of
    /// history. `default_cursor` is the shape used until an application changes
    /// it via DECSCUSR.
    pub fn new(rows: u16, cols: u16, scrollback_lines: u32, default_cursor: CursorShape) -> Self {
        let size = TermDimensions::new(cols as usize, rows as usize);
        let responses = Rc::new(RefCell::new(Vec::new()));

        let config = term_config(scrollback_lines, default_cursor);
        let term = Term::new(config, &size, ResponseSink(Rc::clone(&responses)));
        Self {
            term,
            parser: ansi::Processor::new(),
            responses,
            size,
            active_search_match: None,
        }
    }

    /// Push updated config-derived options into a live terminal: the default
    /// cursor shape (used until an application overrides it via DECSCUSR) and
    /// the scrollback depth (`Term::set_options` resizes history in place).
    pub fn set_terminal_options(&mut self, scrollback_lines: u32, default_cursor: CursorShape) {
        // Shrinking history rotates absolute grid coordinates; the app will
        // recompute live search results against the new layout.
        self.active_search_match = None;
        self.term
            .set_options(term_config(scrollback_lines, default_cursor));
    }
}

/// The `Term` config derived from app settings; `new` and
/// [`AlacrittyCore::set_terminal_options`] must stay in sync.
fn term_config(scrollback_lines: u32, default_cursor: CursorShape) -> Config {
    Config {
        scrolling_history: scrollback_lines as usize,
        semantic_escape_chars: PHANTOM_SEMANTIC_ESCAPE_CHARS.to_string(),
        default_cursor_style: AnsiCursorStyle {
            shape: to_ansi_cursor_shape(default_cursor),
            blinking: false,
        },
        ..Config::default()
    }
}

impl VtCore for AlacrittyCore {
    fn advance(&mut self, bytes: &[u8]) {
        // Output can rotate or reflow absolute grid coordinates. The app will
        // recompute live search results after advancing the terminal.
        self.active_search_match = None;
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        // Reflow changes the coordinate mapping even when the text is stable.
        self.active_search_match = None;
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
            let search_match = self
                .active_search_match
                .is_some_and(|range| range.contains(from_point(indexed.point)));
            cells[row as usize * cols_us + col] = SnapCell {
                c: cell.c,
                fg: map_color(cell.fg),
                bg: map_color(cell.bg),
                attrs: map_attrs(cell.flags),
                width,
                selected,
                search_match,
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

    fn selection_range(&self) -> Option<SearchRange> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        Some(SearchRange::new(
            from_point(range.start),
            from_point(range.end),
        ))
    }

    fn search_scrollback(
        &self,
        query: &str,
        options: SearchOptions,
        range: Option<SearchRange>,
    ) -> Result<Vec<SearchMatch>, SearchError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let pattern = search_pattern(query, options);
        let mut regex = RegexSearch::new(&pattern)
            .map_err(|error| SearchError::InvalidRegex(error.to_string()))?;
        let range = self.clamp_search_range(range.unwrap_or_else(|| self.full_search_range()));
        let start = to_point(range.start);
        let end = to_point(range.end);
        if start > end {
            return Ok(Vec::new());
        }

        let mut matches = Vec::new();
        let mut origin = start;
        while origin <= end {
            let Some(found) = self.term.regex_search_right(&mut regex, origin, end) else {
                break;
            };
            let found_start = *found.start();
            let found_end = *found.end();
            if found_start < start || found_end > end {
                break;
            }
            let found_range = SearchRange::new(from_point(found_start), from_point(found_end));
            if !options.whole_word || self.is_whole_word_match(found_range) {
                matches.push(SearchMatch { range: found_range });
            }

            let Some(next) = self.point_after(found_end, end) else {
                break;
            };
            origin = next;
        }

        Ok(matches)
    }

    fn set_active_search_match(&mut self, search_match: Option<SearchMatch>) {
        self.active_search_match = search_match.map(|search_match| {
            let range = self.clamp_search_range(search_match.range);
            self.scroll_range_into_view(range);
            range
        });
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

    fn full_search_range(&self) -> SearchRange {
        SearchRange::new(
            BufferPoint {
                line: self.term.topmost_line().0,
                column: 0,
            },
            BufferPoint {
                line: self.term.bottommost_line().0,
                column: self.size.columns.saturating_sub(1),
            },
        )
    }

    fn clamp_search_range(&self, range: SearchRange) -> SearchRange {
        let full = self.full_search_range();
        SearchRange::new(
            BufferPoint {
                line: range.start.line.clamp(full.start.line, full.end.line),
                column: range.start.column.min(full.end.column),
            },
            BufferPoint {
                line: range.end.line.clamp(full.start.line, full.end.line),
                column: range.end.column.min(full.end.column),
            },
        )
    }

    fn point_after(&self, point: Point, limit: Point) -> Option<Point> {
        let next = if point.column.0 + 1 < self.size.columns {
            Point::new(point.line, point.column + 1)
        } else {
            Point::new(point.line + 1, Column(0))
        };
        (next <= limit).then_some(next)
    }

    fn scroll_range_into_view(&mut self, range: SearchRange) {
        let viewport_rows = self.size.screen_lines as i32;
        let current_offset = self.term.grid().display_offset() as i32;
        let viewport_top = -current_offset;
        let viewport_bottom = viewport_top + viewport_rows - 1;
        let target_line = if range.start.line < viewport_top {
            range.start.line
        } else if range.end.line > viewport_bottom {
            range.end.line - viewport_rows + 1
        } else {
            return;
        };
        self.scroll_to_offset(target_line.saturating_neg() as usize);
    }

    fn is_whole_word_match(&self, range: SearchRange) -> bool {
        let start = to_point(range.start);
        let end = to_point(range.end);
        let starts_with_word = regex_syntax::is_word_character(self.term.grid()[start].c);
        let ends_with_word = regex_syntax::is_word_character(self.term.grid()[end].c);
        let word_before = self
            .previous_text_point(start)
            .is_some_and(|point| regex_syntax::is_word_character(self.term.grid()[point].c));
        let word_after = self
            .next_text_point(end)
            .is_some_and(|point| regex_syntax::is_word_character(self.term.grid()[point].c));
        starts_with_word != word_before && ends_with_word != word_after
    }

    fn previous_text_point(&self, point: Point) -> Option<Point> {
        let mut line = point.line;
        let mut column = point.column.0;
        if column == 0 {
            if line <= self.term.topmost_line() {
                return None;
            }
            let previous_line = line - 1;
            let last_column = Column(self.size.columns.saturating_sub(1));
            if !self.term.grid()[Point::new(previous_line, last_column)]
                .flags
                .contains(Flags::WRAPLINE)
            {
                return None;
            }
            line = previous_line;
            column = self.size.columns;
        }

        while column > 0 {
            column -= 1;
            let candidate = Point::new(line, Column(column));
            if !self.term.grid()[candidate]
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                return Some(candidate);
            }
        }
        None
    }

    fn next_text_point(&self, point: Point) -> Option<Point> {
        let grid = self.term.grid();
        let mut line = point.line;
        let mut column = point.column.0
            + if grid[point].flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };

        loop {
            if column >= self.size.columns {
                let last_column = Column(self.size.columns.saturating_sub(1));
                if line >= self.term.bottommost_line()
                    || !grid[Point::new(line, last_column)]
                        .flags
                        .contains(Flags::WRAPLINE)
                {
                    return None;
                }
                line += 1;
                column = 0;
            }

            let candidate = Point::new(line, Column(column));
            if !grid[candidate]
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                return Some(candidate);
            }
            column += 1;
        }
    }
}

fn from_point(point: Point) -> BufferPoint {
    BufferPoint {
        line: point.line.0,
        column: point.column.0,
    }
}

fn to_point(point: BufferPoint) -> Point {
    Point::new(Line(point.line), Column(point.column))
}

fn search_pattern(query: &str, options: SearchOptions) -> String {
    let query = if options.regex {
        query.to_owned()
    } else {
        escape_regex(query)
    };
    let query = format!("(?:{query})");
    if options.case_sensitive {
        format!("(?-i:{query})")
    } else {
        format!("(?i:{query})")
    }
}

fn escape_regex(literal: &str) -> String {
    let mut escaped = String::with_capacity(literal.len());
    for character in literal.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
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
    fn wrapped_prompt_survives_repeated_width_resizes_without_duplication() {
        let prompt = b"phantom-terminal via v1.96.0 on main (abcdef) [!]\r\n> ";
        let mut term = core(8, 52, 100);
        term.advance(prompt);

        for cols in [18, 36, 14, 52] {
            term.resize(8, cols);
        }

        let snap = term.snapshot();
        let visible = (0..snap.rows as usize)
            .map(|row| snap.row_text(row))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(visible.matches("phantom-terminal").count(), 1);
        assert_eq!(visible.matches("v1.96.0").count(), 1);
        assert!(visible.contains('>'));
    }

    #[test]
    fn height_only_resize_keeps_viewport_top_stable() {
        let mut term = core(6, 20, 100);
        term.advance(b"one\r\ntwo\r\nthree");
        assert_eq!(term.snapshot().row_text(0), "one");

        term.resize(10, 20);
        assert_eq!(term.snapshot().row_text(0), "one");

        term.resize(4, 20);
        assert_eq!(term.snapshot().row_text(0), "one");
    }

    #[test]
    fn wrapped_prompt_stays_at_top_after_mixed_resizes() {
        let prompt = b"phantom-terminal via v1.96.0 on main (abcdef) [!]\r\n> ";
        let mut term = core(5, 52, 100);
        term.advance(prompt);

        for (rows, cols) in [(12, 18), (9, 14), (5, 52)] {
            term.resize(rows, cols);
        }

        let snap = term.snapshot();
        let visible = (0..snap.rows as usize)
            .map(|row| snap.row_text(row))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            snap.row_text(0),
            "phantom-terminal via v1.96.0 on main (abcdef) [!]"
        );
        assert_eq!(visible.matches("phantom-terminal").count(), 1);
        assert_eq!(visible.matches("v1.96.0").count(), 1);
        assert!(visible.contains('>'));
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
    fn set_terminal_options_updates_default_cursor_shape_on_a_live_terminal() {
        let mut term = AlacrittyCore::new(24, 80, 100, CursorShape::Block);
        term.advance(b"hello");

        term.set_terminal_options(100, CursorShape::Beam);

        assert_eq!(term.snapshot().cursor.shape, CursorShape::Beam);
    }

    #[test]
    fn set_terminal_options_resizes_scrollback_on_a_live_terminal() {
        let mut term = core(2, 10, 100);
        for i in 0..20 {
            term.advance(format!("line{i:02}\r\n").as_bytes());
        }
        assert!(term.scroll_state().history > 5);

        term.set_terminal_options(5, CursorShape::Block);

        assert_eq!(term.scroll_state().history, 5);
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

    #[test]
    fn literal_search_is_case_insensitive_and_escapes_regex_syntax() {
        let mut term = core(4, 40, 100);
        term.advance(b"Alpha ALPHA alpha a.*b");

        let matches = term
            .search_scrollback("ALPHA", SearchOptions::default(), None)
            .unwrap();
        assert_eq!(matches.len(), 3);

        let literal = term
            .search_scrollback("a.*b", SearchOptions::default(), None)
            .unwrap();
        assert_eq!(literal.len(), 1);
        assert_eq!(literal[0].range.start.column, 18);
        assert_eq!(literal[0].range.end.column, 21);
    }

    #[test]
    fn case_sensitive_and_unicode_whole_word_are_independent() {
        let mut term = core(4, 40, 100);
        term.advance("café caféine CAFÉ".as_bytes());

        let whole_word = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        assert_eq!(
            term.search_scrollback("café", whole_word, None)
                .unwrap()
                .len(),
            2
        );

        let exact = SearchOptions {
            case_sensitive: true,
            ..whole_word
        };
        assert_eq!(
            term.search_scrollback("café", exact, None).unwrap().len(),
            1
        );
    }

    #[test]
    fn regex_search_reports_invalid_patterns_without_literal_fallback() {
        let mut term = core(4, 40, 100);
        term.advance(b"item-12 item-345");
        let regex = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };

        assert_eq!(
            term.search_scrollback(r"item-\d+", regex, None)
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            term.search_scrollback("[", regex, None),
            Err(SearchError::InvalidRegex(_))
        ));
    }

    #[test]
    fn search_maps_wrapped_wide_unicode_and_scrollback_cells() {
        let mut term = core(2, 5, 100);
        term.advance("abc世界z\r\nother\r\ntail".as_bytes());

        let wrapped = term
            .search_scrollback("c世界", SearchOptions::default(), None)
            .unwrap();
        assert_eq!(wrapped.len(), 1);
        assert!(wrapped[0].range.start.line < wrapped[0].range.end.line);

        let history = term
            .search_scrollback("abc", SearchOptions::default(), None)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].range.start.line < 0);
    }

    #[test]
    fn captured_selection_limits_search_without_mutating_selection() {
        let mut term = core(4, 30, 100);
        term.advance(b"find outside find");
        term.selection_start(0, 0, SelSide::Left);
        term.selection_update(0, 3, SelSide::Right);
        let captured = term.selection_range().unwrap();

        let matches = term
            .search_scrollback("find", SearchOptions::default(), Some(captured))
            .unwrap();
        assert_eq!(matches.len(), 1);

        assert_eq!(term.selection_range(), Some(captured));
        assert_eq!(term.selection_text().as_deref(), Some("find"));
    }

    #[test]
    fn live_selection_scope_rotates_with_output_and_expires_with_history() {
        let mut term = core(2, 20, 2);
        term.advance(b"target");
        term.selection_start(0, 0, SelSide::Left);
        term.selection_update(0, 5, SelSide::Right);
        let initial = term.selection_range().unwrap();

        term.advance(b"\r\nnext\r\nline");
        let rotated = term.selection_range().unwrap();
        assert_ne!(rotated, initial);
        assert_eq!(
            term.search_scrollback("target", SearchOptions::default(), Some(rotated))
                .unwrap()
                .len(),
            1
        );

        term.advance(b"\r\nthree\r\nfour\r\nfive");
        assert!(term.selection_range().is_none());
    }

    #[test]
    fn active_search_match_highlight_is_separate_and_scrolls_into_view() {
        let mut term = core(2, 20, 100);
        term.advance(b"selected\r\nline1\r\nline2\r\nline3\r\ntarget");
        let target = term
            .search_scrollback("selected", SearchOptions::default(), None)
            .unwrap()[0];
        term.set_active_search_match(Some(target));
        term.selection_start(0, 0, SelSide::Left);
        term.selection_update(0, 7, SelSide::Right);
        let captured = term.selection_range().unwrap();

        assert!(term.scroll_state().offset > 0);
        let snapshot = term.snapshot();
        assert!(snapshot.cells.iter().any(|cell| cell.search_match));
        assert!(snapshot.cells.iter().any(|cell| cell.selected));
        assert_eq!(term.selection_range(), Some(captured));

        term.set_active_search_match(None);
        assert!(!term.snapshot().cells.iter().any(|cell| cell.search_match));

        term.set_active_search_match(Some(target));
        term.advance(b"x");
        assert!(!term.snapshot().cells.iter().any(|cell| cell.search_match));
    }
}
