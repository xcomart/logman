//! Toolbar button that opens a dropdown application menu.
//!
//! Windows and Linux get no native menu bar from gpui — [`gpui::App::set_menus`]
//! only builds one on macOS — so the shell draws its own. [`MenuButton`] is that
//! drawing: a compact glyph button which, while open, paints a list of
//! [`MenuEntry`] rows over the rest of the window.
//!
//! Like every other widget here the button is stateless: the parent view owns
//! the open flag, passes it in through [`MenuButton::open`], and updates it from
//! [`MenuButton::on_open_change`].

use std::rc::Rc;

use gpui::{
    AnchoredPositionMode, App, Corner, ElementId, MouseButton, SharedString, Window, anchored,
    deferred, div, point, prelude::*, px,
};

use super::theme::theme;

/// Edge length of the trigger button.
const TRIGGER_SIZE: f32 = 28.;

/// Vertical distance from the top of the trigger to the top of the dropdown, so
/// that the panel clears the button it hangs from.
const DROP_OFFSET: f32 = TRIGGER_SIZE + 4.;

/// Width of the dropdown panel.
const PANEL_WIDTH: f32 = 240.;

/// Distance the dropdown keeps from the window edges when it would overflow.
const WINDOW_MARGIN: f32 = 6.;

/// Draw order of the click-catching backdrop, relative to other deferred draws.
const BACKDROP_PRIORITY: usize = 1;

/// Draw order of the dropdown panel; above [`BACKDROP_PRIORITY`] so that the
/// backdrop never eats clicks meant for a menu row.
const PANEL_PRIORITY: usize = 2;

/// Callback fired when a menu row is activated.
type ActivateHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when the menu wants to open or close itself.
type OpenChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// One row of a [`MenuButton`] dropdown.
///
/// A row is either a command — a label, an optional shortcut hint and a
/// callback — or a horizontal rule built with [`MenuEntry::separator`].
pub struct MenuEntry {
    /// Text shown on the left of the row.
    label: SharedString,
    /// Shortcut hint shown right-aligned and muted.
    shortcut: Option<SharedString>,
    /// Invoked when the row is clicked.
    on_activate: Option<ActivateHandler>,
    /// Whether the row is a rule rather than a command.
    separator: bool,
}

impl MenuEntry {
    /// Creates a command row with no shortcut hint and no callback.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            shortcut: None,
            on_activate: None,
            separator: false,
        }
    }

    /// Creates a horizontal rule between two groups of commands.
    pub fn separator() -> Self {
        Self {
            label: SharedString::default(),
            shortcut: None,
            on_activate: None,
            separator: true,
        }
    }

    /// Sets the shortcut hint shown at the right edge of the row.
    ///
    /// The hint is decoration only: the key binding itself is registered by the
    /// application, and the menu dispatches the same action the binding does.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Sets the callback run when the row is clicked.
    ///
    /// The menu closes itself afterwards, so the callback does not have to.
    pub fn on_activate(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_activate = Some(Rc::new(handler));
        self
    }
}

/// A toolbar button with a dropdown menu.
///
/// ```ignore
/// MenuButton::new("app-menu")
///     .open(self.menu_open)
///     .entries(vec![MenuEntry::new("New session").shortcut("Ctrl+T")])
///     .on_open_change(|open, _window, cx| { /* store `open` */ })
/// ```
#[derive(IntoElement)]
pub struct MenuButton {
    id: ElementId,
    glyph: SharedString,
    open: bool,
    entries: Vec<MenuEntry>,
    on_open_change: Option<OpenChangeHandler>,
}

impl MenuButton {
    /// Creates a closed menu button showing the default hamburger glyph.
    ///
    /// `id` must be unique among the siblings of the button.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            glyph: SharedString::new_static("\u{2630}"),
            open: false,
            entries: Vec::new(),
            on_open_change: None,
        }
    }

    /// Replaces the glyph drawn on the trigger button.
    pub fn glyph(mut self, glyph: impl Into<SharedString>) -> Self {
        self.glyph = glyph.into();
        self
    }

    /// Sets whether the dropdown is currently shown.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Sets the rows of the dropdown, in display order.
    pub fn entries(mut self, entries: Vec<MenuEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// Called with the open state the menu would like to be in.
    ///
    /// Fires with `true` when the trigger is clicked while closed, and with
    /// `false` when the trigger is clicked again, when a row is activated, or
    /// when the pointer goes down anywhere outside the panel.
    pub fn on_open_change(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let viewport = window.viewport_size();
        let open = self.open;
        let on_open_change = self.on_open_change;

        let trigger = div()
            .id(ElementId::from((self.id.clone(), "trigger")))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(TRIGGER_SIZE))
            .rounded_md()
            .text_size(px(14.))
            .text_color(if open { theme.text } else { theme.text_muted })
            .bg(if open {
                theme.surface_active
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme.surface_hover).text_color(theme.text))
            .when_some(on_open_change.clone(), |this, handler| {
                this.on_click(move |_, window, cx| handler(!open, window, cx))
            })
            .child(self.glyph);

        // A full-window sheet under the panel: a pointer press anywhere it can
        // see closes the menu. It is deferred so that it covers the whole
        // window rather than just the toolbar row this button sits in.
        let backdrop = div()
            .id(ElementId::from((self.id.clone(), "backdrop")))
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            .when_some(on_open_change.clone(), |this, handler| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    handler(false, window, cx)
                })
            });

        let row_theme = theme.clone();
        let rows = self
            .entries
            .into_iter()
            .enumerate()
            .map(move |(index, entry)| {
                let theme = &row_theme;
                if entry.separator {
                    return div()
                        .id(ElementId::from(("menu-separator", index)))
                        .flex_none()
                        .h(px(1.))
                        .my(px(4.))
                        .mx(px(6.))
                        .bg(theme.border);
                }

                let on_open_change = on_open_change.clone();
                div()
                    .id(ElementId::from(("menu-entry", index)))
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap(px(16.))
                    .h(px(28.))
                    .px(px(10.))
                    .mx(px(4.))
                    .rounded_sm()
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .on_click(move |_, window, cx| {
                        if let Some(activate) = entry.on_activate.clone() {
                            activate(window, cx);
                        }
                        if let Some(handler) = on_open_change.clone() {
                            handler(false, window, cx);
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .child(entry.label.clone()),
                    )
                    .children(entry.shortcut.clone().map(|shortcut| {
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme.text_muted)
                            .whitespace_nowrap()
                            .child(shortcut)
                    }))
            });

        let panel = div()
            .id(ElementId::from((self.id.clone(), "panel")))
            .occlude()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PANEL_WIDTH))
            .py(px(4.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .shadow_lg()
            .text_color(theme.text)
            .children(rows);

        // The dropdown hangs off a zero-width box laid out *before* the
        // trigger, not off the trigger itself. An `anchored` element is
        // absolutely positioned, and an absolutely positioned box is aligned by
        // its parent's `align-items`; hanging it directly in the `items_center`
        // row would centre the whole panel on the 28px button instead of
        // starting it at the button's top-left corner. This box neither centres
        // its children nor takes up space, so the panel starts exactly one
        // [`DROP_OFFSET`] below the top-left of the trigger.
        let overlays = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(0.))
            .h(px(TRIGGER_SIZE))
            .child(
                deferred(
                    anchored()
                        .position(point(px(0.), px(0.)))
                        .position_mode(AnchoredPositionMode::Window)
                        .child(backdrop),
                )
                .with_priority(BACKDROP_PRIORITY),
            )
            .child(
                deferred(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .offset(point(px(0.), px(DROP_OFFSET)))
                        .snap_to_window_with_margin(px(WINDOW_MARGIN))
                        .child(panel),
                )
                .with_priority(PANEL_PRIORITY),
            );

        div()
            .id(self.id)
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .children(open.then_some(overlays))
            .child(trigger)
    }
}
