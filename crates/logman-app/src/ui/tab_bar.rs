//! Horizontal tab strip used to switch between SSH sessions.

use std::rc::Rc;

use gpui::{
    App, ElementId, Hsla, MouseButton, Pixels, Point, ScrollHandle, SharedString, Window, div,
    prelude::*, px, transparent_black,
};

use super::menu::{MenuButton, MenuEntry};
use super::theme::{Theme, theme};
use super::tooltip::tooltip_label;

/// Glyph of the button opening the tab list.
const TAB_MENU_GLYPH: &str = "\u{25be}";

/// Marker put in the shortcut slot of the active tab's dropdown row.
const ACTIVE_MARK: &str = "\u{2713}";

/// Connection state rendered as a colored dot in front of a tab title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabStatus {
    /// A connection attempt is in flight.
    Connecting,
    /// The session is live.
    Connected,
    /// The session ended cleanly or was never started.
    Disconnected,
    /// The session failed.
    Error,
}

impl TabStatus {
    /// The dot color for this status under `theme`.
    fn color(self, theme: &Theme) -> Hsla {
        match self {
            TabStatus::Connecting => theme.accent,
            TabStatus::Connected => theme.success,
            TabStatus::Disconnected => theme.text_muted,
            TabStatus::Error => theme.danger,
        }
    }
}

/// One entry of a [`TabBar`].
#[derive(Debug, Clone)]
pub struct TabItem {
    /// Element id of the tab; must be unique within the bar.
    pub id: ElementId,
    /// Label shown to the user.
    pub title: SharedString,
    /// Connection state dot. `None` renders no dot at all.
    pub status: Option<TabStatus>,
}

impl TabItem {
    /// Creates a tab without a status dot.
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: None,
        }
    }

    /// Attaches a status dot to the tab.
    pub fn status(mut self, status: TabStatus) -> Self {
        self.status = Some(status);
        self
    }
}

/// Callback receiving the index of the tab that was acted upon.
type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// Callback receiving the index of the tab that was right-clicked, along with
/// the window-space position of the pointer.
type ContextHandler = Rc<dyn Fn(usize, Point<Pixels>, &mut Window, &mut App)>;

/// Callback for the "new tab" button.
type PlainHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Callback fired when the tab dropdown wants to open or close itself.
type OpenChangeHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A stateless tab strip.
///
/// The bar owns no selection state: the parent view passes the current tabs and
/// the active index on every render, and reacts to [`TabBar::on_select`],
/// [`TabBar::on_close`], [`TabBar::on_context_menu`] and [`TabBar::on_new`].
/// The context menu itself is the parent's too — the bar only reports where the
/// right-click landed.
///
/// The tab list scrolls horizontally once it overflows; the dropdown listing
/// every tab and the "+" button stay pinned to the right edge. Scrolling the
/// active tab back into view is the parent's job, through the handle it passes
/// to [`TabBar::scroll_handle`].
#[derive(IntoElement)]
pub struct TabBar {
    id: ElementId,
    tabs: Vec<TabItem>,
    active: usize,
    scroll_handle: Option<ScrollHandle>,
    menu_open: bool,
    on_select: Option<IndexHandler>,
    on_close: Option<IndexHandler>,
    on_context_menu: Option<ContextHandler>,
    on_new: Option<PlainHandler>,
    on_menu_open_change: Option<OpenChangeHandler>,
    menu_icon: Option<SharedString>,
    menu_tooltip: Option<SharedString>,
    new_tooltip: Option<SharedString>,
    close_tooltip: Option<SharedString>,
}

impl TabBar {
    /// Creates an empty tab bar.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            tabs: Vec::new(),
            active: 0,
            scroll_handle: None,
            menu_open: false,
            on_select: None,
            on_close: None,
            on_context_menu: None,
            on_new: None,
            on_menu_open_change: None,
            menu_icon: None,
            menu_tooltip: None,
            new_tooltip: None,
            close_tooltip: None,
        }
    }

    /// Draws the asset at `path` on the tab dropdown instead of its glyph.
    pub fn menu_icon(mut self, path: impl Into<SharedString>) -> Self {
        self.menu_icon = Some(path.into());
        self
    }

    /// Sets the hover labels of the bar's three buttons: the tab dropdown, the
    /// "+", and the close button on every tab.
    ///
    /// Passed in rather than looked up, like every other string here: this layer
    /// carries no text of its own, so the localised wording belongs to the view
    /// that builds the bar. Any of them left unset simply shows no tooltip.
    pub fn tooltips(
        mut self,
        menu: impl Into<SharedString>,
        new: impl Into<SharedString>,
        close: impl Into<SharedString>,
    ) -> Self {
        self.menu_tooltip = Some(menu.into());
        self.new_tooltip = Some(new.into());
        self.close_tooltip = Some(close.into());
        self
    }

    /// Sets the tabs to render, in display order.
    pub fn tabs(mut self, tabs: Vec<TabItem>) -> Self {
        self.tabs = tabs;
        self
    }

    /// Sets the index of the highlighted tab.
    pub fn active(mut self, index: usize) -> Self {
        self.active = index;
        self
    }

    /// Tracks the horizontal scroll of the tab list with `handle`.
    ///
    /// The handle indexes the tabs in display order, so the parent can bring the
    /// active tab back into view with [`gpui::ScrollHandle::scroll_to_item`].
    pub fn scroll_handle(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_handle = Some(handle.clone());
        self
    }

    /// Sets whether the tab dropdown is currently shown.
    pub fn menu_open(mut self, open: bool) -> Self {
        self.menu_open = open;
        self
    }

    /// Called with the index of the tab the user clicked.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Called with the index of the tab whose close button was clicked.
    ///
    /// Setting this handler is what makes the close buttons appear.
    pub fn on_close(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Called with the index of the right-clicked tab and the window-space
    /// position of the pointer, for the parent to open a context menu at.
    ///
    /// A right-click deliberately does *not* also select the tab: the commands a
    /// tab menu offers differ for the active tab and for any other one, so the
    /// selection has to survive the click that opens the menu.
    pub fn on_context_menu(
        mut self,
        handler: impl Fn(usize, Point<Pixels>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_context_menu = Some(Rc::new(handler));
        self
    }

    /// Called when the "+" button is clicked.
    ///
    /// Setting this handler is what makes the "+" button appear.
    pub fn on_new(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_new = Some(Rc::new(handler));
        self
    }

    /// Called with the open state the tab dropdown would like to be in.
    ///
    /// Setting this handler is what makes the dropdown button appear; it is
    /// still left out while the bar has no tabs to list.
    pub fn on_menu_open_change(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_menu_open_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TabBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let id = self.id;
        let active = self.active;
        let on_select = self.on_select;
        let on_close = self.on_close;
        let on_context_menu = self.on_context_menu;
        let close_tooltip = self.close_tooltip;

        // An empty bar has nothing to list, so its dropdown stays away.
        let on_menu_open_change = self.on_menu_open_change.filter(|_| !self.tabs.is_empty());
        let menu = on_menu_open_change.map(|on_open_change| {
            let entries = self
                .tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| {
                    let mut entry = MenuEntry::new(tab.title.clone());
                    if index == active {
                        entry = entry.shortcut(ACTIVE_MARK);
                    }
                    if let Some(handler) = on_select.clone() {
                        entry = entry.on_activate(move |window, cx| handler(index, window, cx));
                    }
                    entry
                })
                .collect();

            MenuButton::new(ElementId::from((id.clone(), "tab-menu")))
                .glyph(TAB_MENU_GLYPH)
                .when_some(self.menu_icon.clone(), MenuButton::icon)
                .when_some(self.menu_tooltip.clone(), MenuButton::tooltip)
                .open(self.menu_open)
                .entries(entries)
                .on_open_change(move |open, window, cx| on_open_change(open, window, cx))
        });

        let tab_theme = theme.clone();
        let tabs = self.tabs.into_iter().enumerate().map(move |(index, tab)| {
            let theme = &tab_theme;
            let is_active = index == active;
            let group = SharedString::from(format!("logman-tab-{index}"));
            let close_id = ElementId::from((tab.id.clone(), "close"));

            div()
                .id(tab.id)
                .group(group.clone())
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h_full()
                .px(px(10.))
                .border_b_2()
                .border_color(if is_active {
                    theme.accent
                } else {
                    transparent_black()
                })
                .bg(if is_active {
                    theme.surface_active
                } else {
                    transparent_black()
                })
                .text_size(px(13.))
                .text_color(if is_active {
                    theme.text
                } else {
                    theme.text_muted
                })
                .cursor_pointer()
                .hover(|style| {
                    style.bg(if is_active {
                        theme.surface_active
                    } else {
                        theme.surface_hover
                    })
                })
                .when_some(on_select.clone(), |this, handler| {
                    this.on_click(move |_, window, cx| handler(index, window, cx))
                })
                .when_some(on_context_menu.clone(), |this, handler| {
                    this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                        // The press belongs to the menu, not to whatever is
                        // underneath the strip.
                        cx.stop_propagation();
                        handler(index, event.position, window, cx);
                    })
                })
                .when_some(tab.status, |this, status| {
                    this.child(
                        div()
                            .flex_none()
                            .size(px(6.))
                            .rounded_full()
                            .bg(status.color(theme)),
                    )
                })
                .child(div().whitespace_nowrap().child(tab.title))
                .when_some(on_close.clone(), |this, handler| {
                    this.child(
                        div()
                            .id(close_id)
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .rounded_sm()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                            .hover(|style| style.bg(theme.surface_hover).text_color(theme.text))
                            // Keep the click from also selecting the tab.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, window, cx| handler(index, window, cx))
                            .when_some(close_tooltip.clone(), |this, tooltip| {
                                this.tooltip(tooltip_label(tooltip))
                            })
                            .child("\u{00d7}"),
                    )
                })
        });

        div()
            .id(id.clone())
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h(px(36.))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id(ElementId::from((id.clone(), "tabs")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_grow()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .when_some(self.scroll_handle.as_ref(), |this, handle| {
                        this.track_scroll(handle)
                    })
                    .children(tabs),
            )
            .children(menu)
            .when_some(self.on_new, |this, handler| {
                this.child(
                    div()
                        .id(ElementId::from((id.clone(), "new")))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(28.))
                        .mx(px(4.))
                        .rounded_md()
                        .text_size(px(16.))
                        .text_color(theme.text_muted)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.surface_hover).text_color(theme.text))
                        .on_click(move |_, window, cx| handler(window, cx))
                        .when_some(self.new_tooltip.clone(), |this, tooltip| {
                            this.tooltip(tooltip_label(tooltip))
                        })
                        .child("+"),
                )
            })
    }
}
