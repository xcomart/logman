//! A grid of selectable cards, each previewing one color scheme.
//!
//! The widget knows nothing about terminals: callers hand it plain colors, so
//! the same picker can preview anything that has a background, a foreground and
//! a handful of accents.

use std::rc::Rc;

use gpui::{App, ElementId, Hsla, SharedString, Window, div, prelude::*, px};

use super::theme::theme;

/// Default number of cards per row.
const DEFAULT_COLUMNS: usize = 3;

/// Height of the color preview strip inside a card.
const PREVIEW_HEIGHT: f32 = 34.;

/// Diameter of one accent chip in the preview strip.
const CHIP_SIZE: f32 = 9.;

/// Callback fired with the id of the newly picked scheme.
type SelectHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The colors drawn inside one card's preview strip.
#[derive(Debug, Clone)]
pub struct SchemePreview {
    /// Background the strip is filled with.
    pub background: Hsla,
    /// Color of the sample text drawn on the background.
    pub foreground: Hsla,
    /// Accent chips drawn next to the sample text, in display order.
    pub ansi: Vec<Hsla>,
}

/// One entry of a [`SchemePicker`].
#[derive(Debug, Clone)]
pub struct SchemeSwatch {
    /// Stable id reported to [`SchemePicker::on_select`].
    id: SharedString,
    /// Label shown under the preview.
    name: SharedString,
    /// Colors to preview. `None` renders a muted "inherits" card instead,
    /// which is how a per-session picker offers "use the global scheme".
    preview: Option<SchemePreview>,
}

impl SchemeSwatch {
    /// Creates an entry with no preview, drawn as a muted placeholder card.
    pub fn new(id: impl Into<SharedString>, name: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            preview: None,
        }
    }

    /// Attaches the colors to draw in this entry's preview strip.
    pub fn preview(mut self, preview: SchemePreview) -> Self {
        self.preview = Some(preview);
        self
    }
}

/// A stateless grid of color-scheme cards.
///
/// The picker owns no state: the parent view passes the entries and the
/// selected id on every render and reacts to [`SchemePicker::on_select`].
///
/// The grid takes a single tab stop. While focused, the arrow keys move the
/// selection within the grid — `Left`/`Right` by one card, `Up`/`Down` by one
/// row — without wrapping, which is how a grid of radio buttons behaves
/// everywhere else.
///
/// ```ignore
/// SchemePicker::new("scheme")
///     .options(swatches)
///     .selected(Some(self.scheme.clone()))
///     .columns(3)
///     .on_select(cx.listener(..))
/// ```
#[derive(IntoElement)]
pub struct SchemePicker {
    id: ElementId,
    options: Vec<SchemeSwatch>,
    selected: Option<SharedString>,
    columns: usize,
    tab_index: Option<isize>,
    on_select: Option<SelectHandler>,
}

impl SchemePicker {
    /// Creates an empty picker.
    ///
    /// `id` must be unique among the siblings of the picker.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            selected: None,
            columns: DEFAULT_COLUMNS,
            tab_index: None,
            on_select: None,
        }
    }

    /// Sets the entries, in display order.
    pub fn options(mut self, options: impl IntoIterator<Item = SchemeSwatch>) -> Self {
        self.options = options.into_iter().collect();
        self
    }

    /// Sets the id of the highlighted entry. An unknown id highlights nothing.
    pub fn selected(mut self, selected: Option<impl Into<SharedString>>) -> Self {
        self.selected = selected.map(Into::into);
        self
    }

    /// Sets how many cards share a row. Zero is treated as one.
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = columns.max(1);
        self
    }

    /// Places the grid at `index` in the window's tab order.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = Some(index);
        self
    }

    /// Sets the callback invoked with the id of the picked entry.
    ///
    /// Never fired for the entry that is already selected.
    pub fn on_select(mut self, handler: impl Fn(&str, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SchemePicker {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let columns = self.columns;
        let selected = self.selected;
        let on_select = self.on_select;
        let container_id = self.id;
        let outer_id = container_id.clone();
        let tab_index = self.tab_index;

        let ids: Vec<SharedString> = self.options.iter().map(|entry| entry.id.clone()).collect();
        let current = selected
            .as_ref()
            .and_then(|id| ids.iter().position(|candidate| candidate == id));

        let rows: Vec<_> = self
            .options
            .chunks(columns)
            .map(|entries| {
                let cards: Vec<_> = entries
                    .iter()
                    .map(|entry| {
                        let is_selected = Some(&entry.id) == selected.as_ref();
                        let handler = on_select.clone().filter(|_| !is_selected);
                        let id = entry.id.clone();

                        let strip = match &entry.preview {
                            Some(preview) => div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(3.))
                                .h(px(PREVIEW_HEIGHT))
                                .px(px(6.))
                                .rounded_sm()
                                .bg(preview.background)
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(11.))
                                        .text_color(preview.foreground)
                                        .child("Aa"),
                                )
                                .children(preview.ansi.iter().map(|color| {
                                    div()
                                        .flex_none()
                                        .size(px(CHIP_SIZE))
                                        .rounded_full()
                                        .bg(*color)
                                })),
                            None => div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_center()
                                .h(px(PREVIEW_HEIGHT))
                                .px(px(6.))
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .text_size(px(11.))
                                .text_color(theme.text_muted)
                                .child("inherits"),
                        };

                        div()
                            .id(ElementId::from((container_id.clone(), entry.id.clone())))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .p(px(4.))
                            .rounded_md()
                            .border_1()
                            .border_color(if is_selected {
                                theme.accent
                            } else {
                                theme.border
                            })
                            .bg(if is_selected {
                                theme.surface_active
                            } else {
                                theme.surface
                            })
                            .when(!is_selected, |this| {
                                this.cursor_pointer()
                                    .hover(|style| style.bg(theme.surface_hover))
                            })
                            .when_some(handler, |this, handler| {
                                this.on_click(move |_, window, cx| handler(&id, window, cx))
                            })
                            .child(strip)
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(if is_selected {
                                        theme.text
                                    } else {
                                        theme.text_muted
                                    })
                                    .child(entry.name.clone()),
                            )
                            .into_any_element()
                    })
                    .collect();

                // Pad the last row so its cards keep the width of a full row
                // instead of stretching to fill it.
                let padding = (columns - entries.len()) % columns;
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .gap(px(6.))
                    .children(cards)
                    .children((0..padding).map(|_| div().flex_1().min_w_0().into_any_element()))
            })
            .collect();

        div()
            .id(outer_id)
            .flex()
            .flex_col()
            .w_full()
            .gap(px(6.))
            .p(px(2.))
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .when_some(tab_index.filter(|_| !ids.is_empty()), |this, index| {
                let accent = theme.accent;
                let arrow_handler = on_select.clone();
                this.tab_index(index)
                    .focus(move |style| style.border_color(accent))
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let Some(current) = current else { return };
                        let last = ids.len() - 1;
                        let next = match event.keystroke.key.as_str() {
                            "left" => current.checked_sub(1),
                            "right" => (current < last).then(|| current + 1),
                            "up" => current.checked_sub(columns),
                            "down" => (current + columns <= last).then(|| current + columns),
                            _ => return,
                        };
                        let (Some(next), Some(handler)) = (next, arrow_handler.as_ref()) else {
                            return;
                        };
                        cx.stop_propagation();
                        handler(&ids[next], window, cx);
                    })
            })
            .children(rows)
    }
}
