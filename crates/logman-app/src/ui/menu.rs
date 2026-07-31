//! Dropdown menus: the toolbar application menu, and the menu a right-click
//! opens at the pointer.
//!
//! Windows and Linux get no native menu bar from gpui — [`gpui::App::set_menus`]
//! only builds one on macOS — so the shell draws its own. [`MenuButton`] is that
//! drawing: a compact glyph button which, while open, paints a list of
//! [`MenuEntry`] rows over the rest of the window. [`ContextMenu`] paints the
//! same list, without a trigger, wherever the caller says.
//!
//! Like every other widget here both are stateless: the parent view owns the
//! open flag — and, for a context menu, the position that goes with it — passes
//! it in on every render, and closes the menu from
//! [`MenuButton::on_open_change`] or [`ContextMenu::on_dismiss`].

use std::rc::Rc;

use gpui::{
    AnyElement, App, Corner, ElementId, Pixels, Point, SharedString, Size, Window, anchored,
    deferred, div, point, prelude::*, px,
};

use super::theme::{Theme, theme};

/// Edge length of the trigger button.
const TRIGGER_SIZE: f32 = 28.;

/// Vertical distance from the top of the trigger to the top of the dropdown, so
/// that the panel clears the button it hangs from.
const DROP_OFFSET: f32 = TRIGGER_SIZE + 4.;

/// Width of a dropdown panel.
///
/// Wide enough for the longest row either menu has — the pane commands name the
/// thing they act on ("Split right of current tab") and carry a shortcut hint —
/// with room for a translation of it, since a row neither wraps nor ellipsises.
const PANEL_WIDTH: f32 = 280.;

/// Distance the dropdown keeps from the window edges when it would overflow.
const WINDOW_MARGIN: f32 = 6.;

/// Draw order of the click-catching backdrop, relative to other deferred draws.
const BACKDROP_PRIORITY: usize = 1;

/// Draw order of the dropdown panel; above [`BACKDROP_PRIORITY`] so that the
/// backdrop never eats clicks meant for a menu row.
const PANEL_PRIORITY: usize = 2;

/// Callback fired when a menu row is activated.
type ActivateHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when an open menu wants to close itself.
type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when the menu wants to open or close itself.
type OpenChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// One row of a [`MenuButton`] or [`ContextMenu`] dropdown.
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

/// Builds the full-window sheet that sits under an open menu.
///
/// A pointer press anywhere it can see dismisses the menu — either mouse button,
/// so that a right-click outside is not swallowed without effect. The panel is
/// drawn above it and occludes it, so presses on a row never reach here.
///
/// Callers wrap this in `anchored`, whose positions are window-relative by
/// default, so the sheet covers the window rather than the caller's own box.
fn menu_backdrop(
    id: ElementId,
    viewport: Size<Pixels>,
    on_dismiss: Option<DismissHandler>,
) -> AnyElement {
    div()
        .id(id)
        .w(viewport.width)
        .h(viewport.height)
        .occlude()
        .when_some(on_dismiss, |this, dismiss| {
            this.on_any_mouse_down(move |_, window, cx| dismiss(window, cx))
        })
        .into_any_element()
}

/// Builds the floating panel listing `entries`.
///
/// Opaque on purpose: a translucent window allows only one tinted fill per
/// pixel, and the terminal surface underneath already owns it.
fn menu_panel(
    id: ElementId,
    entries: Vec<MenuEntry>,
    on_dismiss: Option<DismissHandler>,
    theme: &Theme,
) -> AnyElement {
    let row_theme = theme.clone();
    let rows = entries.into_iter().enumerate().map(move |(index, entry)| {
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

        let on_dismiss = on_dismiss.clone();
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
                if let Some(dismiss) = on_dismiss.clone() {
                    dismiss(window, cx);
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

    div()
        .id(id)
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
        .children(rows)
        .into_any_element()
}

/// A menu opened at a point of the caller's choosing, with no trigger of its
/// own.
///
/// Rendered by the view that owns the pointer position — typically from an
/// `on_mouse_down(MouseButton::Right, …)` handler that stored the event's
/// window-space position. The element takes no space in its parent's layout, so
/// it can be dropped in anywhere the view already renders:
///
/// ```ignore
/// ContextMenu::new("tab-context")
///     .position(position)
///     .entries(vec![MenuEntry::new("Close tab")])
///     .on_dismiss(|_window, cx| { /* clear the stored position */ })
/// ```
#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    position: Point<Pixels>,
    entries: Vec<MenuEntry>,
    on_dismiss: Option<DismissHandler>,
}

impl ContextMenu {
    /// Creates an empty menu anchored at the window's top-left corner.
    ///
    /// `id` must be unique among the siblings of the menu.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            position: point(px(0.), px(0.)),
            entries: Vec::new(),
            on_dismiss: None,
        }
    }

    /// Puts the top-left corner of the panel at `position`, in window
    /// coordinates.
    ///
    /// A panel that would hang off an edge is pulled back inside the window
    /// instead.
    pub fn position(mut self, position: Point<Pixels>) -> Self {
        self.position = position;
        self
    }

    /// Sets the rows of the menu, in display order.
    pub fn entries(mut self, entries: Vec<MenuEntry>) -> Self {
        self.entries = entries;
        self
    }

    /// Called when the menu should go away: after a row is activated, or when
    /// the pointer goes down outside the panel.
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let viewport = window.viewport_size();
        let backdrop = menu_backdrop(
            ElementId::from((self.id.clone(), "backdrop")),
            viewport,
            self.on_dismiss.clone(),
        );
        let panel = menu_panel(
            ElementId::from((self.id.clone(), "panel")),
            self.entries,
            self.on_dismiss,
            &theme,
        );

        // Absolutely positioned and zero-sized: both children are `anchored` in
        // window coordinates, so this box only has to stay out of the way of the
        // layout it is dropped into.
        div()
            .id(self.id)
            .absolute()
            .w(px(0.))
            .h(px(0.))
            .child(
                deferred(anchored().position(point(px(0.), px(0.))).child(backdrop))
                    .with_priority(BACKDROP_PRIORITY),
            )
            .child(
                deferred(
                    anchored()
                        .anchor(Corner::TopLeft)
                        .position(self.position)
                        .snap_to_window_with_margin(px(WINDOW_MARGIN))
                        .child(panel),
                )
                .with_priority(PANEL_PRIORITY),
            )
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

        let on_dismiss: Option<DismissHandler> = on_open_change.clone().map(|handler| {
            Rc::new(move |window: &mut Window, cx: &mut App| handler(false, window, cx))
                as DismissHandler
        });

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

        // A full-window sheet under the panel, deferred so that it covers the
        // whole window rather than just the toolbar row this button sits in.
        let backdrop = menu_backdrop(
            ElementId::from((self.id.clone(), "backdrop")),
            viewport,
            on_dismiss.clone(),
        );

        let panel = menu_panel(
            ElementId::from((self.id.clone(), "panel")),
            self.entries,
            on_dismiss,
            &theme,
        );

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
                deferred(anchored().position(point(px(0.), px(0.))).child(backdrop))
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
