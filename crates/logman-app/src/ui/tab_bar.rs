//! Horizontal tab strip used to switch between SSH sessions.

use std::rc::Rc;

use gpui::{
    App, ElementId, Hsla, IsZero, MouseButton, Pixels, Point, ScrollHandle, SharedString, Window,
    div, prelude::*, px, transparent_black,
};

use super::menu::{MenuButton, MenuEntry};
use super::scrollbar::Scrollbar;
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
    scrollbar: Option<Scrollbar>,
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
            scrollbar: None,
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

    /// Draws `bar` over the tabs as the overlay scroll indicator.
    ///
    /// Passed in rather than built here, and only while it should be on screen:
    /// a bar comes and goes with the scrolling, which is state this widget
    /// cannot keep — it is built afresh on every render. The owner keeps a
    /// [`ScrollbarState`](super::scrollbar::ScrollbarState) beside the handle it
    /// gives [`TabBar::scroll_handle`], and owns the drag too, since the same
    /// id it built the bar with is what tells that drag from any other.
    pub fn scrollbar(mut self, bar: Scrollbar) -> Self {
        self.scrollbar = Some(bar);
        self
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let id = self.id;
        let active = self.active;
        let on_select = self.on_select;
        let on_close = self.on_close;
        let on_context_menu = self.on_context_menu;
        let close_tooltip = self.close_tooltip;
        let scroll_handle = self.scroll_handle.clone();
        // Read here rather than inside the wheel handler below. gpui pushes an
        // element's text style around its children's layout, so this is the
        // line height the scrolling row itself is laid out with — the very one
        // gpui would convert a line-based wheel delta with. By the time an event
        // is dispatched that stack is empty again and the same call would answer
        // with the default font instead, scrolling a tab faster than the gap
        // beside it.
        let line_height = window.line_height();

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
                // The strip may sit inside a drag area when the toolbar
                // doubles as the title bar; occluding is what keeps a click on
                // a tab from being read as "move the window" instead.
                .occlude()
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
                .when_some(scroll_handle.clone(), |this, handle| {
                    // The occlusion above cuts the scrolling row out of the hit
                    // test wherever a tab covers it, and gpui asks the row's own
                    // hit box before it scrolls anything — so a wheel turned
                    // over a tab, which is most of the strip, would move
                    // nothing. Answering it here puts that back.
                    //
                    // Deliberately the same arithmetic gpui applies to the row
                    // itself, so the two are indistinguishable: a wheel over a
                    // tab and one over the gap beside it move the strip by the
                    // same amount. The row scrolls on one axis, which is why a
                    // vertical wheel — every plain mouse has one, and no
                    // horizontal one — folds onto it. Nothing is clamped here
                    // either: gpui pins the offset to the scrollable range on
                    // the next layout pass, and this writes to the very cell it
                    // pins.
                    this.on_scroll_wheel(move |event, window, _cx| {
                        let delta = event.delta.pixel_delta(line_height);
                        let delta_x = if delta.x.is_zero() { delta.y } else { delta.x };
                        if delta_x.is_zero() {
                            return;
                        }

                        let mut offset = handle.offset();
                        offset.x += delta_x;
                        handle.set_offset(offset);
                        window.refresh();
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
                            // Deliberately *not* occluding, unlike the tab and
                            // the "+": this button only ever sits inside a tab,
                            // whose own occlusion already keeps the drag area
                            // behind the strip from answering for it. Occluding
                            // here would be worse than redundant — gpui's hit
                            // test stops at the first occluding hitbox, so the
                            // tab's hitbox would drop out of the hit list the
                            // moment the pointer reached the button, the
                            // `group_hover` below would read as false, and the
                            // button would hide itself out from under the
                            // pointer. A hidden element paints no listeners, so
                            // the click that followed would land on nothing.
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
                // The wrapper is what the overlay bar is placed against, and
                // it exists only for that: the scrolling row cannot hold its
                // own bar, because its children are what scroll away. Sized to
                // the row, so the bar spans the tabs and stops short of the
                // dropdown and the "+" beside them.
                div()
                    .relative()
                    .flex_grow()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id(ElementId::from((id.clone(), "tabs")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .size_full()
                            .overflow_x_scroll()
                            .when_some(self.scroll_handle.as_ref(), |this, handle| {
                                this.track_scroll(handle)
                            })
                            .children(tabs),
                    )
                    .children(self.scrollbar.as_ref().and_then(|bar| bar.render(&theme))),
            )
            .children(menu)
            .when_some(self.on_new, |this, handler| {
                this.child(
                    div()
                        .id(ElementId::from((id.clone(), "new")))
                        // Same reason as the tab itself: a drag area may lie
                        // behind the strip.
                        .occlude()
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ops::Deref;

    use gpui::{
        Context, DragMoveEvent, Modifiers, MouseUpEvent, Render, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, VisualTestContext, WindowControlArea, point,
    };

    use super::super::scrollbar::{
        DraggedThumb, HIDE_AFTER, ScrollbarAxis, ScrollbarState, hide_later, scroll_to, scrolled,
    };

    use super::*;

    /// Vertical middle of the strip; every row of it is one tab tall.
    const ROW_MIDDLE: f32 = 18.;

    /// Vertical middle of the reference row, which sits under the strip.
    const REFERENCE_MIDDLE: f32 = 54.;

    /// A column comfortably inside the first tab, whatever its title.
    const INSIDE_FIRST_TAB: f32 = 12.;

    /// How far right of the strip's left edge the sweep looks for the button.
    ///
    /// Wide enough to clear one short tab title at any font the test platform
    /// picks, and still short of the "+" that follows the strip.
    const SWEEP_WIDTH: i32 = 200;

    /// Width of the reference row's single child.
    ///
    /// Only has to overflow the window by enough that a wheel turn or two never
    /// reaches the end of the scrollable range, where clamping would hide a
    /// difference in how far the two rows moved.
    const REFERENCE_CONTENT: f32 = 8000.;

    /// How many tabs the scrolling tests put in the strip.
    ///
    /// Same reasoning as [`REFERENCE_CONTENT`]: enough that the strip overflows
    /// any plausible test display several times over.
    const CROWDED: usize = 200;

    /// A row of the strip the overlay bar covers: low enough to be on the
    /// thumb, which rides the bottom edge.
    const BAR_MIDDLE: f32 = 31.;

    /// Element id of the harness's overlay bar, as the workspace names its own.
    const BAR: &str = "tab-scrollbar";

    /// The text size the workspace root sets, which the strip inherits.
    ///
    /// The harness repeats it because it is what a line-based wheel delta is
    /// converted with — a harness in the default font would be measuring a
    /// conversion the app never performs.
    const INHERITED_TEXT: f32 = 13.;

    /// What a sweep of the tab row answered with, counted per handler.
    #[derive(Clone, Default)]
    struct Tally {
        selected: Rc<Cell<usize>>,
        closed: Rc<Cell<usize>>,
        dragged: Rc<Cell<usize>>,
    }

    /// A tab strip in the arrangement that made the close button unclickable:
    /// inside an occluding drag area, the way the toolbar renders it when it
    /// doubles as the title bar.
    ///
    /// The drag area counts the presses that reach it. That stands in for what
    /// the real title bar does with them — start a window move on Linux, answer
    /// `HTCAPTION` on Windows — and both read the same hit test this does.
    ///
    /// Under it is a bare horizontally scrolling row with nothing occluding it,
    /// laid out in the same inherited text style: gpui's own answer to a wheel,
    /// to hold the strip's against.
    struct Harness {
        tally: Tally,
        tabs: Vec<TabItem>,
        strip: ScrollHandle,
        bar: ScrollbarState,
        reference: ScrollHandle,
        showing: Rc<Cell<bool>>,
    }

    impl Harness {
        /// The strip's overlay bar, the way the workspace builds it.
        fn scrollbar(&self) -> Scrollbar {
            Scrollbar::for_handle(BAR, ScrollbarAxis::Horizontal, &self.strip)
        }

        /// Lets go of the thumb, and starts the clock on the bar again.
        fn release(&mut self, cx: &mut Context<Self>) {
            if let Some(epoch) = self.bar.release() {
                hide_later(epoch, cx, |harness| Some(&mut harness.bar));
                cx.notify();
            }
        }
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let Tally {
                selected,
                closed,
                dragged,
            } = self.tally.clone();

            // The owner's half of a bar, copied from the workspace: notice the
            // strip moving, arm the expiry from inside the render that noticed
            // it, and answer drags of the thumb from the root.
            if let Some(epoch) = self
                .bar
                .moved(scrolled(&self.strip, ScrollbarAxis::Horizontal))
            {
                hide_later(epoch, cx, |harness| Some(&mut harness.bar));
            }
            let showing = self.bar.showing();
            self.showing.set(showing);

            div()
                .flex()
                .flex_col()
                .size_full()
                .text_size(px(INHERITED_TEXT))
                .on_drag_move::<DraggedThumb>(cx.listener(
                    |harness, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                        let Some(progress) = harness.scrollbar().dragged(event, cx) else {
                            return;
                        };
                        harness.bar.hold();
                        scroll_to(&harness.strip, ScrollbarAxis::Horizontal, progress);
                        cx.notify();
                    },
                ))
                // Both halves, as the workspace wires them: a drag that ends
                // with the pointer outside the window — which is where a thumb
                // dragged off the end of its track leaves it — is released by
                // the second.
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|harness, _: &MouseUpEvent, _window, cx| {
                        harness.release(cx);
                    }),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|harness, _: &MouseUpEvent, _window, cx| {
                        harness.release(cx);
                    }),
                )
                .child(
                    div()
                        .id("toolbar")
                        .occlude()
                        .window_control_area(WindowControlArea::Drag)
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            dragged.set(dragged.get() + 1)
                        })
                        .w_full()
                        .h(px(36.))
                        .child(
                            TabBar::new("tabs")
                                .tabs(self.tabs.clone())
                                .active(0)
                                .scroll_handle(&self.strip)
                                .when(showing, |strip| strip.scrollbar(self.scrollbar()))
                                .on_select(move |_, _, _| selected.set(selected.get() + 1))
                                .on_close(move |_, _, _| closed.set(closed.get() + 1)),
                        ),
                )
                .child(
                    div()
                        .id("reference")
                        .flex()
                        .flex_row()
                        .w_full()
                        .h(px(36.))
                        .overflow_x_scroll()
                        .track_scroll(&self.reference)
                        .child(div().flex_none().w(px(REFERENCE_CONTENT)).h_full()),
                )
        }
    }

    /// Everything a test needs to read back out of a running harness.
    struct Handles {
        tally: Tally,
        strip: ScrollHandle,
        reference: ScrollHandle,
        /// Whether the bar was on screen the last time the harness drew.
        showing: Rc<Cell<bool>>,
    }

    /// Opens a window on a strip of `tabs` and hands back its handles.
    fn open(cx: &mut TestAppContext, tabs: Vec<TabItem>) -> (Handles, VisualTestContext) {
        let handles = Handles {
            tally: Tally::default(),
            strip: ScrollHandle::new(),
            reference: ScrollHandle::new(),
            showing: Rc::new(Cell::new(false)),
        };

        let window = cx.add_window({
            let tally = handles.tally.clone();
            let strip = handles.strip.clone();
            let reference = handles.reference.clone();
            let showing = handles.showing.clone();
            move |_, _| Harness {
                tally,
                tabs,
                strip,
                bar: ScrollbarState::new(),
                reference,
                showing,
            }
        });
        let cx = VisualTestContext::from_window(*window.deref(), cx);
        cx.run_until_parked();

        (handles, cx)
    }

    /// One tab, which is all the click tests need.
    fn one_tab() -> Vec<TabItem> {
        vec![TabItem::new("tab-0", "one")]
    }

    /// Enough tabs that the strip overflows and can be scrolled.
    fn many_tabs() -> Vec<TabItem> {
        (0..CROWDED)
            .map(|index| TabItem::new(ElementId::from(("tab", index)), format!("tab {index}")))
            .collect()
    }

    /// Turns a wheel over `position`, hovering it first so the hit test is
    /// settled by the time the wheel arrives.
    fn turn_the_wheel(cx: &mut VisualTestContext, position: Point<Pixels>, delta: ScrollDelta) {
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta,
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
    }

    /// What a single clicked column answered with.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Answer {
        /// The tab took the click and asked to be selected.
        Select,
        /// The close button took the click.
        Close,
        /// The press reached the drag area behind the strip.
        Drag,
        /// Nothing at all answered.
        Nothing,
    }

    /// Clicks every column of the strip in turn, reporting what each answered.
    ///
    /// Sweeping rather than aiming: the close button's x depends on how wide the
    /// test platform draws the title, which is not something a test should
    /// pretend to know. Each column is hovered before it is clicked, because the
    /// button only exists once the pointer is on the tab.
    fn sweep_the_strip(cx: &mut TestAppContext) -> Vec<Answer> {
        let (handles, mut cx) = open(cx, one_tab());
        let tally = handles.tally;

        let mut answers = Vec::new();
        let mut seen = (0, 0, 0);
        for x in 0..SWEEP_WIDTH {
            let position = point(px(x as f32), px(ROW_MIDDLE));
            cx.simulate_mouse_move(position, None, Modifiers::none());
            cx.simulate_click(position, Modifiers::none());

            let now = (
                tally.selected.get(),
                tally.closed.get(),
                tally.dragged.get(),
            );
            answers.push(match (now.0 - seen.0, now.1 - seen.1, now.2 - seen.2) {
                (0, 0, 0) => Answer::Nothing,
                (1, 0, 0) => Answer::Select,
                (0, 1, 0) => Answer::Close,
                (0, 0, 1) => Answer::Drag,
                other => panic!("column {x} answered more than once: {other:?}"),
            });
            seen = now;
        }

        answers
    }

    /// The regression this file exists to hold on to: the close button used to
    /// occlude, which cut its own tab out of the hit test and so out of the
    /// `group_hover` that reveals it — leaving a button that hid itself from
    /// under the pointer and answered no click at all.
    #[gpui::test]
    fn the_close_button_takes_a_click_inside_a_drag_area(cx: &mut TestAppContext) {
        let answers = sweep_the_strip(cx);

        assert!(
            answers.contains(&Answer::Close),
            "no column of the tab closed it: {answers:?}"
        );
        assert!(
            answers.contains(&Answer::Select),
            "no column of the tab selected it: {answers:?}"
        );
    }

    /// A wheel puts the overlay bar up, and it comes up over the tabs rather
    /// than beside them: the strip is the same height with it as without.
    #[gpui::test]
    fn scrolling_the_strip_shows_the_overlay_bar(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());
        assert!(
            !handles.showing.get(),
            "the bar was up before anything moved"
        );

        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -3.)),
        );

        assert!(handles.showing.get(), "scrolling did not show the bar");
    }

    /// And it goes away again on its own, a little after the scrolling stops.
    #[gpui::test]
    fn the_overlay_bar_fades_once_the_scrolling_stops(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());
        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -3.)),
        );
        assert!(handles.showing.get());

        cx.executor().advance_clock(HIDE_AFTER / 2);
        cx.run_until_parked();
        assert!(
            handles.showing.get(),
            "the bar went before it had been up its full time"
        );

        cx.executor().advance_clock(HIDE_AFTER);
        cx.run_until_parked();
        assert!(!handles.showing.get(), "the bar never went away");
    }

    /// The thumb takes the strip wherever it is dragged, and keeps the point it
    /// was grabbed by under the pointer while it does.
    #[gpui::test]
    fn dragging_the_thumb_scrolls_the_strip(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());
        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -1.)),
        );
        assert!(handles.showing.get(), "the bar has to be up to be dragged");

        let bar = Scrollbar::for_handle(BAR, ScrollbarAxis::Horizontal, &handles.strip)
            .thumb()
            .expect("an overflowing strip");
        let track = handles.strip.bounds().size.width;
        let before = handles.strip.offset().x;

        // Down on the thumb, then past the threshold gpui uses to tell a drag
        // from a click, then a little way along, and finally clear off the end
        // of the track — which should leave the strip pinned to its end rather
        // than running past it.
        let grab = point(bar.start + px(4.), px(BAR_MIDDLE));
        cx.simulate_mouse_move(grab, None, Modifiers::none());
        cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(grab.x + px(40.), grab.y),
            Some(MouseButton::Left),
            Modifiers::none(),
        );

        let halfway = point(bar.start + (track - bar.length) / 2. + px(4.), grab.y);
        cx.simulate_mouse_move(halfway, Some(MouseButton::Left), Modifiers::none());
        let dragged = handles.strip.offset().x;
        assert!(
            dragged < before,
            "dragging the thumb left the strip where it was"
        );

        let past_the_end = point(track * 4., grab.y);
        cx.simulate_mouse_move(past_the_end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(past_the_end, MouseButton::Left, Modifiers::none());

        assert_eq!(
            handles.strip.offset().x,
            -handles.strip.max_offset().width,
            "dragging the thumb off the end did not reach the end"
        );

        // Letting go starts the clock again, rather than leaving the bar up for
        // good — which is what a hold that is never released would do.
        assert!(handles.showing.get(), "the bar went during the drag");
        cx.executor().advance_clock(HIDE_AFTER * 2);
        cx.run_until_parked();
        assert!(
            !handles.showing.get(),
            "the bar stayed up after the thumb was let go"
        );
    }

    /// A pointer that takes hold of the thumb and then keeps perfectly still
    /// keeps the bar up, however long it holds: a motionless drag scrolls
    /// nothing, and nothing scrolling is otherwise what takes a bar down.
    #[gpui::test]
    fn a_held_thumb_outlasts_the_clock(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());
        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -1.)),
        );

        let bar = Scrollbar::for_handle(BAR, ScrollbarAxis::Horizontal, &handles.strip)
            .thumb()
            .expect("an overflowing strip");
        let grab = point(bar.start + px(4.), px(BAR_MIDDLE));
        cx.simulate_mouse_move(grab, None, Modifiers::none());
        cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::none());
        // Twice: gpui turns the press into a drag on the first move past its
        // threshold, and only reports the moves after that one.
        for step in 1..=2 {
            cx.simulate_mouse_move(
                point(grab.x + px(step as f32 * 20.), grab.y),
                Some(MouseButton::Left),
                Modifiers::none(),
            );
        }

        cx.executor().advance_clock(HIDE_AFTER * 5);
        cx.run_until_parked();

        assert!(
            handles.showing.get(),
            "the bar went while a pointer was still holding it"
        );
    }

    /// The thumb is the only part of the bar that takes a press: the track it
    /// slides along is not drawn, so a click beside it reaches the tab under it
    /// exactly as it did before the bar existed.
    #[gpui::test]
    fn the_track_beside_the_thumb_still_reaches_the_tabs(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());
        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -1.)),
        );
        assert!(
            handles.showing.get(),
            "the bar has to be up to be in the way"
        );

        let bar = Scrollbar::for_handle(BAR, ScrollbarAxis::Horizontal, &handles.strip)
            .thumb()
            .expect("an overflowing strip");
        let beyond = bar.start + bar.length + px(40.);
        assert!(
            beyond < handles.strip.bounds().size.width,
            "the thumb filled the track, leaving no bare track to test"
        );

        let selected = handles.tally.selected.get();
        let position = point(beyond, px(BAR_MIDDLE));
        cx.simulate_mouse_move(position, None, Modifiers::none());
        cx.simulate_click(position, Modifiers::none());

        assert_eq!(
            handles.tally.selected.get(),
            selected + 1,
            "a click on the bare track did not reach the tab under it"
        );

        // And the thumb itself is the exception that makes the rule worth
        // stating: a press there belongs to the bar, and the tab under it must
        // not also answer. On Windows the same occlusion is what keeps the
        // press off the title bar's caption hit test.
        let selected = handles.tally.selected.get();
        let on_the_thumb = point(bar.start + bar.length / 2., px(BAR_MIDDLE));
        cx.simulate_mouse_move(on_the_thumb, None, Modifiers::none());
        cx.simulate_click(on_the_thumb, Modifiers::none());

        assert_eq!(
            handles.tally.selected.get(),
            selected,
            "a press on the thumb also selected the tab under it"
        );
        assert_eq!(
            handles.tally.dragged.get(),
            0,
            "a press on the thumb reached the window drag area"
        );
    }

    /// The other half of the same balance, and what the tab's own occlusion is
    /// for: no column of a tab may let the press through to the drag area, or
    /// the title bar takes it and moves the window instead of switching tabs.
    /// Past the last tab the strip is bare, and there the drag area should
    /// answer.
    #[gpui::test]
    fn the_drag_area_answers_only_past_the_last_tab(cx: &mut TestAppContext) {
        let answers = sweep_the_strip(cx);

        let last_tab_column = answers
            .iter()
            .rposition(|answer| matches!(answer, Answer::Select | Answer::Close))
            .expect("the sweep never landed on the tab");
        let first_drag_column = answers
            .iter()
            .position(|answer| *answer == Answer::Drag)
            .expect("the sweep never landed on the bare strip");

        assert!(
            first_drag_column > last_tab_column,
            "the drag area answered a press aimed at a tab: {answers:?}"
        );
    }

    /// The other cost of the tab's occlusion: it hides the scrolling row from
    /// the hit test wherever a tab covers it, which is nearly all of a crowded
    /// strip, so gpui's own wheel handling never ran. The tab answers the wheel
    /// itself now.
    #[gpui::test]
    fn a_wheel_over_a_tab_scrolls_the_strip(cx: &mut TestAppContext) {
        let (handles, mut cx) = open(cx, many_tabs());

        assert!(
            handles.strip.max_offset().width > px(0.),
            "the strip did not overflow, so there was nothing to scroll"
        );
        assert_eq!(
            handles.strip.offset().x,
            px(0.),
            "the strip started scrolled"
        );

        turn_the_wheel(
            &mut cx,
            point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)),
            ScrollDelta::Lines(point(0., -3.)),
        );

        assert!(
            handles.strip.offset().x < px(0.),
            "a wheel over a tab left the strip where it was"
        );
    }

    /// And it answers it the way the row itself would have. The reference row
    /// below the strip is plain gpui scrolling in the same inherited text style,
    /// so a wheel delta has to land both rows in the same place — otherwise a
    /// wheel would carry further over a tab than over the bare strip beside it.
    ///
    /// Both a line delta and a pixel delta, because only the first is converted
    /// through the line height, which is exactly what is easy to get wrong.
    #[gpui::test]
    fn a_wheel_moves_a_tab_and_plain_gpui_the_same_distance(cx: &mut TestAppContext) {
        for delta in [
            ScrollDelta::Lines(point(0., -3.)),
            ScrollDelta::Lines(point(-2., 0.)),
            ScrollDelta::Pixels(point(px(-40.), px(0.))),
            ScrollDelta::Pixels(point(px(0.), px(-40.))),
        ] {
            let (handles, mut cx) = open(cx, many_tabs());

            turn_the_wheel(&mut cx, point(px(INSIDE_FIRST_TAB), px(ROW_MIDDLE)), delta);
            turn_the_wheel(
                &mut cx,
                point(px(INSIDE_FIRST_TAB), px(REFERENCE_MIDDLE)),
                delta,
            );

            assert_eq!(
                handles.strip.offset().x,
                handles.reference.offset().x,
                "a {delta:?} moved the strip and plain gpui different distances"
            );
            assert!(
                handles.strip.offset().x < px(0.),
                "a {delta:?} moved neither row, so they agreed on nothing"
            );
        }
    }
}
