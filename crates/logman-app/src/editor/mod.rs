//! The multi-line plain-text editor: a rope, and a gpui element that draws only
//! what fits on screen.
//!
//! [`crate::ui::TextInput`] is a single line by construction — it replaces `\n`
//! with a space — so the editor is a new widget rather than an extension of it.
//! What carries over is the discipline, not the code: byte offsets everywhere,
//! UTF-16 only at the platform boundary, grapheme clusters for every caret step,
//! and an `EntityInputHandler` that the IME can drive without ever being handed
//! an offset that is not on a character boundary. [`mod@view`] documents each
//! departure and why it is one.
//!
//! # The two things that make it hold at a gigabyte
//!
//! * **The buffer is a rope.** An insert is O(log n), and so are
//!   `byte <-> line` and `byte <-> UTF-16 code unit`. [`mod@buffer`].
//! * **Only the visible lines are shaped.** The element works out the row range
//!   from the scroll offset and shapes those and no others. [`mod@element`].
//!
//! Nothing here parses what it is showing. The buffer is plain text — a log, a
//! config file, whatever the file panel opened — and every line is drawn in one
//! colour. Syntax highlighting, if it is ever wanted, goes in at the one place
//! marked for it in [`mod@element`] — `runs_for`, where a line is turned into
//! the runs that shape it.
//!
//! # Using it
//!
//! ```ignore
//! editor::init(cx);                    // once, after `ui::init`
//!
//! let editor = cx.new(EditorView::new);
//! // The colours and the font are the host's to supply, from whatever surface
//! // the editor is sitting in; see `palette_for`.
//! editor.update(cx, |editor, cx| {
//!     editor.set_palette(palette_for(&scheme), cx);
//!     editor.set_font(font, px(font_size), cx);
//! });
//! cx.subscribe(&editor, |_, editor, event: &EditorEvent, cx| {
//!     if matches!(event, EditorEvent::Changed) {
//!         let text = editor.read(cx).text();
//!         // hand it to whoever is saving
//!     }
//! })
//! .detach();
//! ```
//!
//! [`crate::editor_pane`] is the host that does all of this for a file opened
//! out of the file panel.
//!
//! # Out of scope, deliberately
//!
//! Multiple cursors would change the shape of every command in [`mod@view`],
//! so they go in as a list of selections in one piece or not at all. Code
//! folding needs a row-to-line map between the buffer and the renderer, which
//! nothing else wants yet. Soft wrapping needs the same map and a shaping pass
//! that can split a line, and would cost the one property the element is built
//! around: that row *n* is at `n * line_height`. File reading and writing are
//! the host's, not the widget's — [`EditorView::set_text`] and
//! [`EditorView::text`] are the whole of that boundary.

pub mod buffer;
pub mod element;
pub mod find;
pub mod history;
pub mod view;

// The names a host mounting the editor writes, gathered so that it writes
// `editor::EditorView` rather than `editor::view::EditorView`. Only some of
// them have a caller — see [`crate::editor_pane`] — and inside a binary crate a
// re-export nobody has imported yet reads as an unused import.
#[allow(unused_imports)]
pub use self::{
    buffer::Buffer,
    element::EditorElement,
    find::{FindState, find_all},
    history::{Edit, EditKind, History, SelectionState, Transaction},
    view::{EditorEvent, EditorView, init},
};

use gpui::Hsla;
use logman_term::{Rgb, TerminalTheme};

use crate::terminal_view::to_hsla;

/// The colours the text surface is drawn in.
///
/// Nine slots, all of them derived from the *terminal* colour scheme rather than
/// from the application [`Theme`](crate::ui::Theme): an editor pane sits beside
/// a terminal pane showing the same host, and the two surfaces reading as one
/// material is what stops the split looking like two applications glued
/// together. It also means a user who picked Solarized for their shell gets
/// Solarized for the file they open out of it, without having chosen twice.
///
/// A syntax palette — one slot per token kind — is what a *code* editor needs
/// and is exactly what this avoids needing, because nothing here decides that
/// one run of bytes means more than another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditorPalette {
    /// Behind everything. Opaque, unlike most of the app's surfaces.
    pub background: Hsla,
    /// The text.
    pub foreground: Hsla,
    /// The caret.
    pub cursor: Hsla,
    /// Behind the selection.
    pub selection: Hsla,
    /// A wash across the line the caret is on.
    pub line_highlight: Hsla,
    /// The line numbers.
    pub gutter: Hsla,
    /// The line number of the caret's line.
    pub gutter_active: Hsla,
    /// Behind a match of the find query.
    pub find_match: Hsla,
    /// Behind the match the find bar is currently on.
    pub find_current: Hsla,
}

/// How far the caret's line is lifted off the background, as a mix of the
/// foreground into it.
///
/// Small enough that a wash across the full width of the pane does not read as
/// a selection, which is the one thing it must never be confused with.
const LINE_HIGHLIGHT_MIX: f32 = 0.08;

/// How far a line number is lifted off the background.
///
/// Halfway is what makes the gutter legible without competing with the text: a
/// scheme's own `foreground` is what the *content* is drawn in, and numbers as
/// loud as the content would be read as content.
const GUTTER_MIX: f32 = 0.5;

/// How strongly a find match is tinted, and how much more strongly the one the
/// find bar is currently on.
const FIND_MATCH_MIX: f32 = 0.35;
/// See [`FIND_MATCH_MIX`].
const FIND_CURRENT_MIX: f32 = 0.7;

/// Index of normal yellow in the sixteen-slot ANSI palette.
const ANSI_YELLOW: usize = 3;
/// Index of bright yellow in the sixteen-slot ANSI palette.
const ANSI_BRIGHT_YELLOW: usize = 11;

/// `a` with `t` of `b` mixed into it, channel by channel.
///
/// Mixing rather than compositing with an alpha, because every slot this feeds
/// is painted as an *opaque* fill under the text. The editor background is
/// itself opaque — see [`EditorView::render`](view::EditorView) — so a
/// translucent wash would be composited against it anyway, and doing the
/// arithmetic here means the four highlight fills stack predictably instead of
/// each one darkening whatever it happens to land on top of.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0., 1.);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Rgb::new(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b))
}

/// The editor palette for a terminal colour `scheme`.
///
/// The four slots a scheme names outright — background, foreground, cursor and
/// selection — are taken verbatim, so a caret in the editor and a caret in the
/// terminal beside it are the same mark, and a selection in either is the same
/// block of colour. The five it does not name are mixed out of the ones it
/// does, which is what keeps a scheme's contrast intact whether it is a dark
/// one or a light one: nothing here assumes the background is the darker of the
/// two.
///
/// Find matches take the scheme's yellow, normal for a match and bright for the
/// one the bar is on, because yellow is the one hue a palette reserves for
/// *look here* rather than for an error or for success. Note that the selection
/// is painted after the matches and is opaque, so the current match disappears
/// under it — which is right, because [`EditorView::find_next`](view::EditorView)
/// selects the match it moves to, and the selection is then the mark.
pub fn palette_for(scheme: &TerminalTheme) -> EditorPalette {
    let background = scheme.background;
    let foreground = scheme.foreground;
    EditorPalette {
        background: to_hsla(background),
        foreground: to_hsla(foreground),
        cursor: to_hsla(scheme.cursor),
        selection: to_hsla(scheme.selection),
        line_highlight: to_hsla(mix(background, foreground, LINE_HIGHLIGHT_MIX)),
        gutter: to_hsla(mix(background, foreground, GUTTER_MIX)),
        gutter_active: to_hsla(foreground),
        find_match: to_hsla(mix(background, scheme.ansi[ANSI_YELLOW], FIND_MATCH_MIX)),
        find_current: to_hsla(mix(
            background,
            scheme.ansi[ANSI_BRIGHT_YELLOW],
            FIND_CURRENT_MIX,
        )),
    }
}

#[cfg(test)]
mod tests;
