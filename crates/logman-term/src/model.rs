//! The terminal model: an ANSI parser plus a scrollback aware screen buffer.
//!
//! [`TerminalModel`] wraps `alacritty_terminal`'s [`Term`] and drives it with
//! bytes that arrive from an SSH channel. It deliberately does **not** spawn a
//! local PTY - `alacritty_terminal::tty` is unused - and it is not `Send`,
//! because it is only ever touched from the UI thread.

use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, GridCell, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor};

use crate::keys::TermModes;
use crate::snapshot::{CursorPos, RunFlags, StyledRun, TerminalLine, TerminalSnapshot};
use crate::theme::{Rgb, TerminalTheme};

/// Grid geometry handed to [`Term::new`] and [`Term::resize`].
#[derive(Debug, Clone, Copy)]
struct TermDimensions {
    columns: usize,
    screen_lines: usize,
    total_lines: usize,
}

impl TermDimensions {
    fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let columns = cols as usize;
        let screen_lines = rows as usize;
        Self { columns, screen_lines, total_lines: screen_lines.saturating_add(scrollback) }
    }
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.total_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// State the [`EventListener`] shares with the model.
#[derive(Debug, Default)]
struct SharedState {
    /// Title change requested since the last drain.
    ///
    /// The outer `Option` means "a change happened", the inner one carries the
    /// new title (`None` after `OSC 2` with an empty argument / `ResetTitle`).
    pending_title: Option<Option<String>>,
    /// Replies the terminal wants to send back over the channel, for example
    /// the answer to a Device Status Report.
    pty_output: Vec<u8>,
}

/// Event sink that funnels the few events we care about into [`SharedState`].
#[derive(Debug, Clone)]
struct EventProxy {
    state: Rc<RefCell<SharedState>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => self.state.borrow_mut().pending_title = Some(Some(title)),
            Event::ResetTitle => self.state.borrow_mut().pending_title = Some(None),
            Event::PtyWrite(text) => {
                self.state.borrow_mut().pty_output.extend_from_slice(text.as_bytes());
            },
            _ => {},
        }
    }
}

/// A terminal screen fed by a byte stream.
pub struct TerminalModel {
    term: Term<EventProxy>,
    parser: Processor,
    state: Rc<RefCell<SharedState>>,
    theme: TerminalTheme,
    scrollback: usize,
    title: Option<String>,
}

impl std::fmt::Debug for TerminalModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (cols, rows) = self.size();
        f.debug_struct("TerminalModel")
            .field("cols", &cols)
            .field("rows", &rows)
            .field("scrollback", &self.scrollback)
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

impl TerminalModel {
    /// Create a terminal of `cols` x `rows` cells with `scrollback` lines of
    /// history.
    ///
    /// Both dimensions are clamped to at least one cell.
    pub fn new(cols: u16, rows: u16, scrollback: usize, theme: TerminalTheme) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);

        let state = Rc::new(RefCell::new(SharedState::default()));
        let term = Self::build_term(cols, rows, scrollback, Rc::clone(&state));

        Self { term, parser: Processor::new(), state, theme, scrollback, title: None }
    }

    fn build_term(
        cols: u16,
        rows: u16,
        scrollback: usize,
        state: Rc<RefCell<SharedState>>,
    ) -> Term<EventProxy> {
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let dimensions = TermDimensions::new(cols, rows, scrollback);
        Term::new(config, &dimensions, EventProxy { state })
    }

    /// Feed raw bytes coming from the remote shell into the parser.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);

        if let Some(title) = self.state.borrow_mut().pending_title.take() {
            self.title = title;
        }
    }

    /// Resize the terminal, clamping both dimensions to at least one cell.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.size() == (cols, rows) {
            return;
        }
        self.term.resize(TermDimensions::new(cols, rows, self.scrollback));
    }

    /// Current size as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        let grid = self.term.grid();
        (grid.columns() as u16, grid.screen_lines() as u16)
    }

    /// Build an immutable view of the visible screen for the renderer.
    pub fn snapshot(&self) -> TerminalSnapshot {
        let grid = self.term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let display_offset = grid.display_offset();

        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let line = Line(row as i32 - display_offset as i32);
            lines.push(self.build_line(&grid[line], cols));
        }

        let content = self.term.renderable_content();
        let cursor_point = content.cursor.point;
        let cursor_row = cursor_point.line.0 as isize + display_offset as isize;
        let cursor_in_view = cursor_row >= 0 && (cursor_row as usize) < rows;
        let cursor = CursorPos {
            line: cursor_row.clamp(0, rows.saturating_sub(1) as isize) as u16,
            col: cursor_point.column.0.min(cols.saturating_sub(1)) as u16,
        };
        let cursor_visible = cursor_in_view && content.cursor.shape != CursorShape::Hidden;

        TerminalSnapshot {
            cols: cols as u16,
            rows: rows as u16,
            lines,
            cursor,
            cursor_visible,
            display_offset,
            total_scrollback: grid.history_size(),
        }
    }

    /// Turn a single grid row into style runs.
    fn build_line(&self, row: &alacritty_terminal::grid::Row<Cell>, cols: usize) -> TerminalLine {
        // Drop trailing cells that render as blank default background so that
        // an untouched row produces no runs at all.
        let mut len = cols;
        while len > 0 && row[Column(len - 1)].is_empty() {
            len -= 1;
        }

        let mut runs: Vec<StyledRun> = Vec::new();
        let mut current: Option<StyledRun> = None;

        for col in 0..len {
            let cell = &row[Column(col)];

            // The trailing half of a double width character carries no glyph of
            // its own; skipping it keeps the column alignment intact because the
            // wide character already occupies two cells when rendered.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let (fg, bg, flags) = self.cell_style(cell);
            let extends = current
                .as_ref()
                .is_some_and(|run| run.fg == fg && run.bg == bg && run.flags == flags);

            if extends {
                let run = current.as_mut().expect("checked above");
                push_cell(&mut run.text, cell);
            } else {
                if let Some(run) = current.take() {
                    runs.push(run);
                }
                let mut text = String::new();
                push_cell(&mut text, cell);
                current = Some(StyledRun { text, start_col: col as u16, fg, bg, flags });
            }
        }

        if let Some(run) = current {
            runs.push(run);
        }

        TerminalLine { runs }
    }

    /// Resolve the final colors and attributes of a single cell.
    fn cell_style(&self, cell: &Cell) -> (Rgb, Rgb, RunFlags) {
        let flags = run_flags(cell.flags);
        let mut fg = self.theme.resolve(cell.fg, true, flags);
        let mut bg = self.theme.resolve(cell.bg, false, flags);

        if flags.contains(RunFlags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(RunFlags::HIDDEN) {
            fg = bg;
        }

        (fg, bg, flags)
    }

    /// Scroll the viewport; positive values move up into the scrollback.
    pub fn scroll_lines(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    /// Jump back to the bottom of the scrollback.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Replace the color palette. Existing content is re-colored on the next
    /// [`TerminalModel::snapshot`].
    pub fn set_theme(&mut self, theme: TerminalTheme) {
        self.theme = theme;
    }

    /// The palette currently in use.
    pub fn theme(&self) -> &TerminalTheme {
        &self.theme
    }

    /// Window title set through `OSC 0` / `OSC 2`, if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Terminal modes relevant for key encoding.
    pub fn modes(&self) -> TermModes {
        let mode = self.term.mode();
        TermModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        }
    }

    /// Take the bytes the terminal wants to send back to the remote side.
    ///
    /// Escape sequences such as a Device Status Report (`CSI 6 n`) expect an
    /// answer on the same channel; the caller is responsible for writing the
    /// returned bytes to the SSH channel. Returns an empty vector when there is
    /// nothing to send.
    pub fn take_pty_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.state.borrow_mut().pty_output)
    }

    /// Reset the terminal to its initial state, dropping screen, scrollback,
    /// title and any half-parsed escape sequence.
    pub fn reset(&mut self) {
        let (cols, rows) = self.size();
        self.term = Self::build_term(cols, rows, self.scrollback, Rc::clone(&self.state));
        self.parser = Processor::new();
        self.title = None;
        *self.state.borrow_mut() = SharedState::default();
    }
}

/// Append a cell's glyph plus any combining marks to `text`.
fn push_cell(text: &mut String, cell: &Cell) {
    text.push(cell.c);
    if let Some(zerowidth) = cell.zerowidth() {
        text.extend(zerowidth);
    }
}

/// Translate `alacritty_terminal`'s cell flags into renderer facing ones.
fn run_flags(flags: Flags) -> RunFlags {
    let mut out = RunFlags::empty();
    out.set(RunFlags::BOLD, flags.contains(Flags::BOLD));
    out.set(RunFlags::ITALIC, flags.contains(Flags::ITALIC));
    out.set(RunFlags::UNDERLINE, flags.intersects(Flags::ALL_UNDERLINES));
    out.set(RunFlags::STRIKEOUT, flags.contains(Flags::STRIKEOUT));
    out.set(RunFlags::INVERSE, flags.contains(Flags::INVERSE));
    out.set(RunFlags::DIM, flags.contains(Flags::DIM));
    out.set(RunFlags::HIDDEN, flags.contains(Flags::HIDDEN));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(cols: u16, rows: u16) -> TerminalModel {
        TerminalModel::new(cols, rows, 100, TerminalTheme::dark())
    }

    #[test]
    fn dimensions_are_clamped() {
        let term = TerminalModel::new(0, 0, 0, TerminalTheme::dark());
        assert_eq!(term.size(), (1, 1));
        assert_eq!(term.snapshot().lines.len(), 1);
    }

    #[test]
    fn plain_text_lands_on_the_first_line() {
        let mut term = model(20, 5);
        term.feed(b"hello");

        let snapshot = term.snapshot();
        assert_eq!(snapshot.lines.len(), 5);
        assert_eq!(snapshot.lines[0].text(), "hello");
        assert_eq!(snapshot.cursor, CursorPos { line: 0, col: 5 });
        assert!(snapshot.cursor_visible);
    }

    #[test]
    fn identical_styles_are_merged_into_one_run() {
        let mut term = model(20, 3);
        term.feed(b"hello");

        let line = &term.snapshot().lines[0];
        assert_eq!(line.runs.len(), 1);
        assert_eq!(line.runs[0].start_col, 0);
        assert_eq!(line.runs[0].text, "hello");
    }

    #[test]
    fn sgr_bold_red_is_reflected_in_the_run() {
        let mut term = model(20, 3);
        term.feed(b"\x1b[1;31mX");

        let line = &term.snapshot().lines[0];
        assert_eq!(line.runs.len(), 1);
        let run = &line.runs[0];
        assert_eq!(run.text, "X");
        assert!(run.flags.contains(RunFlags::BOLD));
        // Bold promotes red to bright red.
        assert_eq!(run.fg, term.theme().ansi[9]);
        assert!(run.fg.r > 150 && run.fg.r > run.fg.g && run.fg.r > run.fg.b);
    }

    #[test]
    fn style_changes_split_runs() {
        let mut term = model(20, 3);
        term.feed(b"ab\x1b[31mcd");

        let line = &term.snapshot().lines[0];
        assert_eq!(line.runs.len(), 2);
        assert_eq!(line.runs[0].text, "ab");
        assert_eq!(line.runs[0].start_col, 0);
        assert_eq!(line.runs[1].text, "cd");
        assert_eq!(line.runs[1].start_col, 2);
    }

    #[test]
    fn inverse_swaps_foreground_and_background() {
        let mut term = model(20, 3);
        term.feed(b"a\x1b[7mb");

        let line = &term.snapshot().lines[0];
        assert_eq!(line.runs.len(), 2);

        let normal = &line.runs[0];
        let inverted = &line.runs[1];
        assert_eq!(normal.fg, term.theme().foreground);
        assert_eq!(normal.bg, term.theme().background);
        assert_eq!(inverted.fg, normal.bg);
        assert_eq!(inverted.bg, normal.fg);
        assert!(inverted.flags.contains(RunFlags::INVERSE));
    }

    #[test]
    fn hidden_paints_text_in_the_background_color() {
        let mut term = model(20, 3);
        term.feed(b"\x1b[8mX");

        let run = &term.snapshot().lines[0].runs[0];
        assert!(run.flags.contains(RunFlags::HIDDEN));
        assert_eq!(run.fg, run.bg);
    }

    #[test]
    fn erase_display_leaves_empty_lines() {
        let mut term = model(20, 4);
        term.feed(b"hello\r\nworld\r\n");
        assert!(!term.snapshot().lines[0].is_empty());

        term.feed(b"\x1b[2J");
        let snapshot = term.snapshot();
        assert_eq!(snapshot.lines.len(), 4);
        for line in &snapshot.lines {
            assert!(line.is_empty(), "expected blank line, got {:?}", line);
        }
    }

    #[test]
    fn resize_after_newlines_keeps_the_snapshot_consistent() {
        let mut term = model(20, 5);
        for i in 0..40 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }

        for (cols, rows) in [(40u16, 10u16), (10, 3), (1, 1), (80, 24), (20, 5)] {
            term.resize(cols, rows);
            let snapshot = term.snapshot();
            assert_eq!(snapshot.cols, cols);
            assert_eq!(snapshot.rows, rows);
            assert_eq!(snapshot.lines.len(), rows as usize);
            assert_eq!(term.size(), (cols, rows));
        }
    }

    #[test]
    fn scrollback_offset_moves_and_returns_to_the_bottom() {
        let mut term = model(20, 5);
        for i in 0..30 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }

        let snapshot = term.snapshot();
        assert_eq!(snapshot.display_offset, 0);
        assert!(snapshot.total_scrollback >= 5, "scrollback: {}", snapshot.total_scrollback);

        term.scroll_lines(5);
        assert_eq!(term.snapshot().display_offset, 5);

        term.scroll_lines(-2);
        assert_eq!(term.snapshot().display_offset, 3);

        term.scroll_to_bottom();
        assert_eq!(term.snapshot().display_offset, 0);
    }

    #[test]
    fn scrolled_viewport_shows_older_content() {
        let mut term = model(20, 5);
        for i in 0..30 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }

        let bottom = term.snapshot();
        term.scroll_lines(2);
        let scrolled = term.snapshot();

        // Scrolling up by two moves every visible row two positions down.
        assert_ne!(bottom.lines[0].text(), scrolled.lines[0].text());
        assert_eq!(bottom.lines[0].text(), scrolled.lines[2].text());
        assert_eq!(bottom.lines[1].text(), scrolled.lines[3].text());
    }

    #[test]
    fn scrolling_beyond_the_history_is_clamped() {
        let mut term = model(20, 5);
        term.feed(b"just one line");

        term.scroll_lines(1000);
        assert_eq!(term.snapshot().display_offset, 0);

        term.scroll_lines(-1000);
        assert_eq!(term.snapshot().display_offset, 0);
    }

    #[test]
    fn osc_sets_and_resets_the_window_title() {
        let mut term = model(20, 5);
        assert_eq!(term.title(), None);

        term.feed(b"\x1b]0;logman\x07");
        assert_eq!(term.title(), Some("logman"));

        term.feed(b"\x1b]2;other\x1b\\");
        assert_eq!(term.title(), Some("other"));

        term.reset();
        assert_eq!(term.title(), None);
    }

    #[test]
    fn modes_track_the_terminal_state() {
        let mut term = model(20, 5);
        assert_eq!(term.modes(), TermModes::default());

        term.feed(b"\x1b[?1h\x1b[?2004h");
        let modes = term.modes();
        assert!(modes.app_cursor);
        assert!(modes.bracketed_paste);

        term.feed(b"\x1b[?1l\x1b[?2004l");
        let modes = term.modes();
        assert!(!modes.app_cursor);
        assert!(!modes.bracketed_paste);
    }

    #[test]
    fn cursor_visibility_follows_dectcem_and_the_viewport() {
        let mut term = model(20, 5);
        assert!(term.snapshot().cursor_visible);

        term.feed(b"\x1b[?25l");
        assert!(!term.snapshot().cursor_visible);

        term.feed(b"\x1b[?25h");
        assert!(term.snapshot().cursor_visible);

        for i in 0..30 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }
        term.scroll_lines(10);
        assert!(!term.snapshot().cursor_visible);
        term.scroll_to_bottom();
        assert!(term.snapshot().cursor_visible);
    }

    #[test]
    fn reset_clears_screen_and_scrollback() {
        let mut term = model(20, 5);
        for i in 0..30 {
            term.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(term.snapshot().total_scrollback > 0);

        term.reset();
        let snapshot = term.snapshot();
        assert_eq!(snapshot.total_scrollback, 0);
        assert_eq!(snapshot.display_offset, 0);
        assert_eq!(term.size(), (20, 5));
        for line in &snapshot.lines {
            assert!(line.is_empty());
        }
    }

    #[test]
    fn wide_characters_keep_the_column_layout() {
        let mut term = model(20, 3);
        term.feed("한글x".as_bytes());

        let line = &term.snapshot().lines[0];
        // The spacer cells are dropped, so the text holds three characters ...
        assert_eq!(line.text(), "한글x");
        // ... while `x` still starts at column four.
        let last = line.runs.last().expect("a run");
        assert_eq!(last.start_col, 0);
        assert_eq!(last.text.chars().count(), 3);
    }

    #[test]
    fn combining_marks_are_attached_to_the_base_character() {
        let mut term = model(20, 3);
        // `e` followed by a combining acute accent.
        term.feed("e\u{0301}".as_bytes());
        assert_eq!(term.snapshot().lines[0].text(), "e\u{0301}");
    }

    #[test]
    fn changing_the_theme_recolors_the_snapshot() {
        let mut term = model(20, 3);
        term.feed(b"x");
        assert_eq!(term.snapshot().lines[0].runs[0].fg, TerminalTheme::dark().foreground);

        term.set_theme(TerminalTheme::light());
        assert_eq!(term.snapshot().lines[0].runs[0].fg, TerminalTheme::light().foreground);
        assert_eq!(term.theme().background, TerminalTheme::light().background);
    }

    #[test]
    fn device_status_reports_are_queued_for_the_channel() {
        let mut term = model(20, 5);
        assert!(term.take_pty_output().is_empty());

        term.feed(b"\x1b[6n");
        assert_eq!(term.take_pty_output(), b"\x1b[1;1R");
        assert!(term.take_pty_output().is_empty());
    }

    #[test]
    fn split_escape_sequences_are_resumed_across_feeds() {
        let mut term = model(20, 3);
        term.feed(b"\x1b[1");
        term.feed(b";31m");
        term.feed(b"Z");

        let run = &term.snapshot().lines[0].runs[0];
        assert_eq!(run.text, "Z");
        assert!(run.flags.contains(RunFlags::BOLD));
    }
}
