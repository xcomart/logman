//! Overlay scroll indicators.
//!
//! A thumb with no track behind it, drawn over the content rather than beside
//! it, shown while a surface is being scrolled and taken down again a moment
//! after it stops — the behaviour macOS gives every scrollable view. Nothing
//! here reserves layout space, so turning it on costs a surface no width.
//!
//! While it is up the thumb can be dragged, and once it is gone it is gone
//! completely: a hidden bar is not rendered at all, so it has no hit box and
//! there is nothing over the content to press. That is the whole of the
//! "disappears means untouchable" guarantee — no state to get wrong.
//!
//! Four pieces, kept apart on purpose:
//!
//! * [`thumb`] and [`dragged_to`] are the geometry, and pure: offset to thumb,
//!   and pointer back to offset. Every awkward case — a surface with nothing to
//!   scroll, a thumb that ratio alone would shrink to a speck, a pointer
//!   dragged past either end — is decided there, where it can be tested without
//!   a window.
//! * [`ScrollbarState`] is the "is it showing?" flip-flop.  It carries no timer
//!   of its own: the owning view arms one with [`hide_later`], because only the
//!   view can notify itself when it fires.
//! * [`Scrollbar`] describes one bar, and both draws it and reads drags of it.
//! * [`DraggedThumb`] is what a drag of one carries.
//!
//! ## How a drag finds its way home
//!
//! gpui hands a `DragMoveEvent` to *every* element that listens for that drag
//! type, ancestor or not, and the `bounds` on the event are the listening
//! element's rather than the dragged one's. So the thumb carries its own track
//! — in window coordinates, as measured on the frame the drag began — inside
//! [`DraggedThumb`], along with where in the thumb the press landed. A listener
//! anywhere in the view can then map the pointer to an offset, and every bar's
//! drag is told apart from every other bar's by the id in the same payload.
//! Views therefore listen once, on their own root, and need no wiring around
//! each individual bar.

use std::cell::Cell;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, Div, DragMoveEvent, ElementId, Pixels, Point, ScrollHandle, Size,
    Stateful, div, prelude::*, px,
};

use super::theme::Theme;

/// Thickness of the thumb, in pixels.
const THICKNESS: f32 = 6.;

/// Default gap between the thumb and the container edges it rides.
pub const INSET: f32 = 2.;

/// Shortest the thumb may get, in pixels.
///
/// Length is otherwise the visible fraction of the content, which on a long
/// enough surface — a terminal with a full scrollback — would round to a speck
/// too small to read as a position, let alone to catch with a pointer.
const MIN_LENGTH: f32 = 24.;

/// How long the thumb stays up after the last movement.
///
/// Two seconds, asked for by name: long enough to still be there when a wheel
/// is turned in bursts, or to be reached for and dragged, and short enough that
/// it is gone before it becomes furniture.
pub const HIDE_AFTER: Duration = Duration::from_secs(2);

/// Which edge of its container a bar rides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarAxis {
    /// Along the bottom edge, for a surface that scrolls sideways.
    Horizontal,
    /// Down the right-hand edge, for a surface that scrolls up and down.
    Vertical,
}

impl ScrollbarAxis {
    /// The component of `size` that runs along this axis.
    fn along(self, size: Size<Pixels>) -> Pixels {
        match self {
            ScrollbarAxis::Horizontal => size.width,
            ScrollbarAxis::Vertical => size.height,
        }
    }

    /// The component of `point` that runs along this axis.
    fn of(self, point: Point<Pixels>) -> Pixels {
        match self {
            ScrollbarAxis::Horizontal => point.x,
            ScrollbarAxis::Vertical => point.y,
        }
    }
}

/// Where the thumb sits along its container's edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// Distance from the container's leading edge to the start of the thumb.
    pub start: Pixels,
    /// Length of the thumb along the scrolling axis.
    pub length: Pixels,
}

/// The thumb for a surface `track` pixels long, or `None` when there is nothing
/// to scroll and so nothing to say.
///
/// `visible`, `scrollable` and `scrolled` are all in the *same* unit, whatever
/// it is — pixels for a gpui scroll container, lines for a terminal — because
/// only their ratios are used: `visible / (visible + scrollable)` sets the
/// length and `scrolled / scrollable` sets the position. `scrollable` is how
/// much lies beyond the visible part, which is exactly what
/// [`gpui::ScrollHandle::max_offset`] reports.
///
/// An offset past either end is clamped rather than refused: a surface is
/// briefly scrolled out of range whenever gpui applies a wheel delta, and is
/// pulled back on the next layout pass.
pub fn thumb(track: Pixels, visible: f32, scrollable: f32, scrolled: f32) -> Option<Thumb> {
    if !visible.is_finite() || !scrollable.is_finite() || !scrolled.is_finite() {
        return None;
    }
    if track <= px(0.) || visible <= 0. || scrollable <= 0. {
        return None;
    }

    let length = (track * (visible / (visible + scrollable)))
        .max(px(MIN_LENGTH))
        .min(track);
    let start = (track - length) * (scrolled / scrollable).clamp(0., 1.);

    Some(Thumb { start, length })
}

/// How far along its range a thumb dragged to `pointer` has reached, as a
/// fraction from `0.` at the start to `1.` at the end.
///
/// The inverse of [`thumb`], and the reason a drag needs no running total:
/// `pointer` is measured from the track's leading edge and `grab` is how far
/// into the thumb the press landed, so the same point of the thumb stays under
/// the pointer however far the gesture wanders, including outside the window.
///
/// `None` when the thumb fills its track, where there is nowhere to drag it and
/// the division would be by zero.
pub fn dragged_to(track: Pixels, length: Pixels, pointer: Pixels, grab: Pixels) -> Option<f32> {
    let travel = track - length;
    if travel <= px(0.) {
        return None;
    }

    let progress = f32::from(pointer - grab) / f32::from(travel);
    progress.is_finite().then(|| progress.clamp(0., 1.))
}

/// What a thumb drag carries with it.
///
/// Everything a listener needs to answer "where has this gone?" without having
/// seen the press: which bar it is, the track it runs in, and where the pointer
/// took hold. See the module docs for why it travels rather than being read off
/// the event.
pub struct DraggedThumb {
    /// The bar being dragged, so a view with several tells them apart.
    id: ElementId,
    /// The bar's axis, so the pointer is read along the right one.
    axis: ScrollbarAxis,
    /// The track in window coordinates, as measured when the drag began.
    track: Bounds<Pixels>,
    /// The thumb's length when the drag began.
    length: Pixels,
    /// How far into the thumb the press landed.
    ///
    /// gpui only offers this number to the closure that builds the drag
    /// preview, so that closure parks it here on the way past. A [`Cell`]
    /// rather than a plain field because the payload is only ever seen through
    /// a shared reference after that.
    grab: Cell<Pixels>,
}

impl DraggedThumb {
    /// The fraction of its range this drag has reached, if it belongs to the
    /// bar `id`.
    pub fn progress(&self, id: &ElementId, position: Point<Pixels>) -> Option<f32> {
        if self.id != *id {
            return None;
        }

        let pointer = self.axis.of(position) - self.axis.of(self.track.origin);
        dragged_to(
            self.axis.along(self.track.size),
            self.length,
            pointer,
            self.grab.get(),
        )
    }
}

/// Whether a surface's bar is showing, and for how much longer.
///
/// Movement is noticed by comparing offsets between renders rather than by
/// hooking every route that scrolls — a wheel, a keyboard, "scroll the active
/// tab into view", a window resize. Anything that moves a surface repaints it,
/// so the comparison catches all of them and nothing has to remember to
/// announce itself. A drag is the one exception, because a pointer held still
/// on the thumb moves nothing and must still keep the bar up; that is what
/// [`ScrollbarState::hold`] is for.
#[derive(Debug, Default)]
pub struct ScrollbarState {
    /// Offset at the last look, or `None` before the first one.
    ///
    /// The first look never counts as movement, so a surface does not flash a
    /// bar at the moment it appears.
    seen: Option<f32>,
    /// Which showing this is. An expiry timer carries the epoch it was armed
    /// at and stands down if a later movement has replaced it, so a bar is
    /// never taken down by a timer belonging to an older burst of scrolling.
    epoch: u64,
    /// Whether a pointer is holding the thumb. No timer may fire while it is.
    held: bool,
    showing: bool,
}

impl ScrollbarState {
    /// A bar that is not showing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes where the surface is scrolled to now.
    ///
    /// Returns the epoch to arm an expiry timer with when the surface moved
    /// since the last look, and `None` when it sat still — or when a drag has
    /// the bar, which arms its own timer when it lets go.
    pub fn moved(&mut self, scrolled: f32) -> Option<u64> {
        let moved = self.seen.is_some_and(|seen| seen != scrolled);
        self.seen = Some(scrolled);
        if !moved || self.held {
            return None;
        }

        self.epoch = self.epoch.wrapping_add(1);
        self.showing = true;
        Some(self.epoch)
    }

    /// Keeps the bar up while a pointer holds the thumb.
    ///
    /// Deliberately arms nothing: a drag would otherwise leave a timer behind
    /// for every pointer move it made.
    pub fn hold(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.held = true;
        self.showing = true;
    }

    /// Lets go of the thumb, returning the epoch to arm the expiry with.
    ///
    /// `None` when nothing was holding it, which is every mouse button release
    /// that had nothing to do with a bar.
    pub fn release(&mut self) -> Option<u64> {
        if !self.held {
            return None;
        }
        self.held = false;
        self.epoch = self.epoch.wrapping_add(1);
        Some(self.epoch)
    }

    /// Whether the bar should be drawn.
    pub fn showing(&self) -> bool {
        self.showing
    }

    /// Takes the bar down, unless a newer movement has since put it up or a
    /// pointer is holding it.
    ///
    /// Returns whether anything changed, which is whether the view needs to be
    /// repainted.
    pub fn hide(&mut self, epoch: u64) -> bool {
        if self.held || self.epoch != epoch || !self.showing {
            return false;
        }
        self.showing = false;
        true
    }
}

/// Arms the timer that takes a bar down again.
///
/// `pick` finds the state inside the view when the timer fires rather than
/// borrowing it now, because by then the surface it belongs to may be gone —
/// a closed session's file listing, say.
pub fn hide_later<V: 'static>(
    epoch: u64,
    cx: &mut Context<V>,
    pick: impl Fn(&mut V) -> Option<&mut ScrollbarState> + 'static,
) {
    cx.spawn(async move |view, cx| {
        cx.background_executor().timer(HIDE_AFTER).await;
        view.update(cx, |view, cx| {
            if pick(view).is_some_and(|state| state.hide(epoch)) {
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

/// One overlay bar: where it is, how long its content is, and how to draw it.
///
/// Built afresh on every render from whatever the surface can report — a
/// [`ScrollHandle`] for a gpui scroll container, plain numbers for anything
/// else — and used both to draw the thumb and to read a drag of it, so the two
/// can never disagree about the geometry.
pub struct Scrollbar {
    id: ElementId,
    axis: ScrollbarAxis,
    track: Bounds<Pixels>,
    visible: f32,
    scrollable: f32,
    scrolled: f32,
    inset: f32,
}

impl Scrollbar {
    /// A bar over a surface measured in whatever unit suits it.
    ///
    /// `track` is the box the thumb rides, in window coordinates — the box the
    /// bar is drawn against, which for a scroll container is the container
    /// itself. See [`thumb`] for what the other three mean.
    pub fn new(
        id: impl Into<ElementId>,
        axis: ScrollbarAxis,
        track: Bounds<Pixels>,
        visible: f32,
        scrollable: f32,
        scrolled: f32,
    ) -> Self {
        Self {
            id: id.into(),
            axis,
            track,
            visible,
            scrollable,
            scrolled,
            inset: INSET,
        }
    }

    /// A bar over a gpui scroll container, measured off its handle.
    ///
    /// The handle reports the bounds and the scrollable extent as of the last
    /// layout pass, so a bar trails a resize by one frame and corrects itself
    /// on the next — which is the frame the resize is drawn in.
    pub fn for_handle(
        id: impl Into<ElementId>,
        axis: ScrollbarAxis,
        handle: &ScrollHandle,
    ) -> Self {
        let track = handle.bounds();
        Self::new(
            id,
            axis,
            track,
            f32::from(axis.along(track.size)),
            f32::from(axis.along(handle.max_offset())),
            scrolled(handle, axis),
        )
    }

    /// Moves the bar further in from the edge it rides.
    ///
    /// For a surface with something else already pinned to that edge — the file
    /// panel's resize grip — which would otherwise take the presses aimed at
    /// the thumb, being drawn on top of it.
    pub fn inset(mut self, inset: f32) -> Self {
        self.inset = inset;
        self
    }

    /// The thumb as it stands, or `None` when there is nothing to scroll.
    pub fn thumb(&self) -> Option<Thumb> {
        thumb(
            self.axis.along(self.track.size),
            self.visible,
            self.scrollable,
            self.scrolled,
        )
    }

    /// The bar as an element, or `None` when there is nothing to scroll.
    ///
    /// Whether it should be on screen at all is the owner's call, made by
    /// building a `Scrollbar` only while its
    /// [`ScrollbarState::showing`] says so.
    ///
    /// Absolutely positioned, so it is placed against its parent's padding box
    /// and takes no part in that parent's layout. The parent has to be the box
    /// the thumb measures — for a scroll container that means a wrapper around
    /// it, not the container itself, whose own children scroll away underneath.
    ///
    /// The thumb occludes, and only the thumb: a press on it belongs to the bar
    /// and must not also reach the tab, row or terminal underneath, while the
    /// track it slides along is not drawn at all and so lets everything
    /// through. Nothing is drawn when the bar is down, which is what makes a
    /// hidden bar untouchable rather than merely invisible.
    ///
    /// The fill is opaque on purpose. A translucent window composes one tint
    /// fill per pixel and no more (see `app_settings::window_tint`), and a bar
    /// over the terminal surface would be a second one.
    pub fn render(&self, theme: &Theme) -> Option<Stateful<Div>> {
        let thumb = self.thumb()?;

        let axis = self.axis;
        let track = self.track;
        let length = thumb.length;
        let bar = div()
            .id(self.id.clone())
            .absolute()
            .occlude()
            .rounded_full()
            .bg(theme.text_muted)
            // An empty preview: the thumb follows the pointer directly, so a
            // ghost trailing it would only be a second thing to watch.
            .on_drag(
                DraggedThumb {
                    id: self.id.clone(),
                    axis,
                    track,
                    length,
                    grab: Cell::new(px(0.)),
                },
                move |dragged, grab, _window, cx| {
                    dragged.grab.set(axis.of(grab));
                    cx.new(|_| gpui::Empty)
                },
            );

        Some(match self.axis {
            ScrollbarAxis::Horizontal => bar
                .left(thumb.start)
                .bottom(px(self.inset))
                .w(thumb.length)
                .h(px(THICKNESS)),
            ScrollbarAxis::Vertical => bar
                .top(thumb.start)
                .right(px(self.inset))
                .h(thumb.length)
                .w(px(THICKNESS)),
        })
    }

    /// How far along its range `event` has dragged this bar, or `None` when the
    /// drag belongs to another bar.
    pub fn dragged(&self, event: &DragMoveEvent<DraggedThumb>, cx: &App) -> Option<f32> {
        event.drag(cx).progress(&self.id, event.event.position)
    }
}

/// How far `handle` is scrolled along `axis`, counting up from the start.
///
/// gpui measures a scroll offset as the displacement of the content, which runs
/// negative as a surface scrolls; this is the same distance the other way
/// round, which is what a bar is positioned by.
pub fn scrolled(handle: &ScrollHandle, axis: ScrollbarAxis) -> f32 {
    f32::from(match axis {
        ScrollbarAxis::Horizontal => -handle.offset().x,
        ScrollbarAxis::Vertical => -handle.offset().y,
    })
}

/// Scrolls `handle` to `progress` of its range, reporting whether it moved.
///
/// Written straight into the offset gpui itself scrolls with, and left for
/// gpui's next layout pass to pin to the scrollable range, exactly as a wheel
/// delta is.
pub fn scroll_to(handle: &ScrollHandle, axis: ScrollbarAxis, progress: f32) -> bool {
    let scrollable = axis.along(handle.max_offset());
    let mut offset = handle.offset();
    match axis {
        ScrollbarAxis::Horizontal => offset.x = -(scrollable * progress),
        ScrollbarAxis::Vertical => offset.y = -(scrollable * progress),
    }

    if offset == handle.offset() {
        return false;
    }
    handle.set_offset(offset);
    true
}

#[cfg(test)]
mod tests {
    use gpui::{point, size};

    use super::*;

    /// A track a hundred long, starting at the window's origin.
    fn track(length: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(length), px(length)))
    }

    /// A surface that fits has nothing to point at.
    #[test]
    fn a_surface_with_nothing_to_scroll_has_no_thumb() {
        assert_eq!(thumb(px(100.), 100., 0., 0.), None);
    }

    /// And neither has one that has not been laid out yet, or one whose
    /// geometry arrived as a nonsense float.
    #[test]
    fn an_unmeasurable_surface_has_no_thumb() {
        assert_eq!(thumb(px(0.), 100., 100., 0.), None);
        assert_eq!(thumb(px(-10.), 100., 100., 0.), None);
        assert_eq!(thumb(px(100.), 0., 100., 0.), None);
        assert_eq!(thumb(px(100.), f32::NAN, 100., 0.), None);
        assert_eq!(thumb(px(100.), 100., f32::INFINITY, 0.), None);
    }

    /// Length is the visible share of the whole, and the ends line up: at rest
    /// the thumb starts at the top, at the end it finishes at the bottom.
    #[test]
    fn the_thumb_spans_the_visible_share_and_reaches_both_ends() {
        let top = thumb(px(200.), 100., 300., 0.).expect("a scrollable surface");
        assert_eq!(top.length, px(50.));
        assert_eq!(top.start, px(0.));

        let bottom = thumb(px(200.), 100., 300., 300.).expect("a scrollable surface");
        assert_eq!(bottom.length, px(50.));
        assert_eq!(bottom.start + bottom.length, px(200.));

        let middle = thumb(px(200.), 100., 300., 150.).expect("a scrollable surface");
        assert_eq!(middle.start, px(75.));
    }

    /// A very long surface still gets a thumb that can be seen and caught, and
    /// it still reaches the far end rather than running off it.
    #[test]
    fn a_long_surface_gets_a_thumb_that_is_still_visible() {
        let short = thumb(px(200.), 10., 100_000., 0.).expect("a scrollable surface");
        assert_eq!(short.length, px(MIN_LENGTH));

        let end = thumb(px(200.), 10., 100_000., 100_000.).expect("a scrollable surface");
        assert_eq!(end.start + end.length, px(200.));
    }

    /// A thumb never outgrows its track, however the ratios come out.
    #[test]
    fn the_thumb_never_outgrows_its_track() {
        let tiny = thumb(px(10.), 100., 1., 0.).expect("a scrollable surface");
        assert_eq!(tiny.length, px(10.));
        assert_eq!(tiny.start, px(0.));
    }

    /// gpui lets an offset run past the end between a wheel and the layout pass
    /// that pins it back. The thumb stays on its track meanwhile.
    #[test]
    fn an_offset_past_either_end_is_pinned_to_the_track() {
        let past = thumb(px(200.), 100., 300., 900.).expect("a scrollable surface");
        assert_eq!(past.start + past.length, px(200.));

        let before = thumb(px(200.), 100., 300., -900.).expect("a scrollable surface");
        assert_eq!(before.start, px(0.));
    }

    /// A drag reads back exactly what drew the thumb: grab it where it sits,
    /// move nowhere, and the surface has not scrolled.
    #[test]
    fn a_drag_that_goes_nowhere_scrolls_nothing() {
        let thumb = thumb(px(200.), 100., 300., 150.).expect("a scrollable surface");

        let progress = dragged_to(px(200.), thumb.length, thumb.start + px(20.), px(20.))
            .expect("a thumb with room to travel");
        assert_eq!(progress, 0.5);
    }

    /// And the point taken hold of stays under the pointer: the same grab a
    /// third of the way down the thumb lands the thumb's start a third short of
    /// the pointer, wherever the pointer goes.
    #[test]
    fn a_drag_keeps_the_grabbed_point_under_the_pointer() {
        let progress =
            dragged_to(px(200.), px(50.), px(100.), px(10.)).expect("a thumb with room to travel");
        assert_eq!(progress, 0.6);

        let start = (px(200.) - px(50.)) * progress;
        assert_eq!(start + px(10.), px(100.));
    }

    /// Dragged past either end, the surface stops at that end rather than
    /// running on or wrapping round.
    #[test]
    fn a_drag_past_either_end_stops_there() {
        assert_eq!(dragged_to(px(200.), px(50.), px(9_000.), px(10.)), Some(1.));
        assert_eq!(
            dragged_to(px(200.), px(50.), px(-9_000.), px(10.)),
            Some(0.)
        );
    }

    /// A thumb that fills its track has nowhere to go, and says so rather than
    /// dividing by zero.
    #[test]
    fn a_thumb_that_fills_its_track_cannot_be_dragged() {
        assert_eq!(dragged_to(px(200.), px(200.), px(50.), px(10.)), None);
        assert_eq!(dragged_to(px(200.), px(300.), px(50.), px(10.)), None);
    }

    /// A drag only answers to the bar it started on. Two bars in one view see
    /// each other's moves, and must ignore them.
    #[test]
    fn a_drag_answers_only_to_its_own_bar() {
        let dragged = DraggedThumb {
            id: "mine".into(),
            axis: ScrollbarAxis::Vertical,
            track: track(200.),
            length: px(50.),
            grab: Cell::new(px(10.)),
        };

        assert_eq!(
            dragged.progress(&"mine".into(), point(px(0.), px(100.))),
            Some(0.6)
        );
        assert_eq!(
            dragged.progress(&"theirs".into(), point(px(0.), px(100.))),
            None
        );
    }

    /// The pointer is read along the bar's own axis, and against the track's
    /// own corner rather than the window's.
    #[test]
    fn a_drag_is_read_along_its_axis_from_the_track_corner() {
        let offset = Bounds::new(point(px(40.), px(80.)), size(px(200.), px(200.)));
        let sideways = DraggedThumb {
            id: "bar".into(),
            axis: ScrollbarAxis::Horizontal,
            track: offset,
            length: px(50.),
            grab: Cell::new(px(10.)),
        };

        // 140 in the window is 100 along a track that starts at 40.
        assert_eq!(
            sideways.progress(&"bar".into(), point(px(140.), px(9_999.))),
            Some(0.6)
        );
    }

    /// The first look is not movement, or every surface would flash a bar at
    /// the moment it is first drawn.
    #[test]
    fn the_first_look_at_a_surface_shows_nothing() {
        let mut state = ScrollbarState::new();

        assert_eq!(state.moved(0.), None);
        assert!(!state.showing());
    }

    /// Movement puts the bar up; sitting still leaves it as it was.
    #[test]
    fn movement_shows_the_bar_and_stillness_leaves_it_alone() {
        let mut state = ScrollbarState::new();
        state.moved(0.);

        let epoch = state.moved(40.).expect("a moved surface");
        assert!(state.showing());

        assert_eq!(state.moved(40.), None);
        assert!(state.showing(), "sitting still took the bar down early");

        assert!(state.hide(epoch));
        assert!(!state.showing());
    }

    /// A timer armed by an earlier burst of scrolling cannot take down the bar
    /// a later one put up.
    #[test]
    fn a_stale_timer_leaves_a_newer_showing_alone() {
        let mut state = ScrollbarState::new();
        state.moved(0.);

        let stale = state.moved(40.).expect("a moved surface");
        let fresh = state.moved(80.).expect("a moved surface");
        assert_ne!(stale, fresh);

        assert!(!state.hide(stale), "a stale timer hid a newer showing");
        assert!(state.showing());

        assert!(state.hide(fresh));
        assert!(!state.showing());
    }

    /// And a timer that fires against a bar which is already down changes
    /// nothing, so it asks for no repaint.
    #[test]
    fn hiding_a_bar_that_is_already_down_changes_nothing() {
        let mut state = ScrollbarState::new();
        state.moved(0.);
        let epoch = state.moved(40.).expect("a moved surface");

        assert!(state.hide(epoch));
        assert!(!state.hide(epoch));
    }

    /// A pointer holding the thumb keeps the bar up however long it is held
    /// still, and no timer armed before or during the hold may take it down.
    #[test]
    fn a_held_thumb_keeps_the_bar_up() {
        let mut state = ScrollbarState::new();
        state.moved(0.);
        let before = state.moved(40.).expect("a moved surface");

        state.hold();
        assert!(state.showing());
        assert!(!state.hide(before), "a timer fired through a held thumb");

        // Movement during the hold arms nothing, so it leaves no timer behind
        // for every pixel the pointer travelled.
        assert_eq!(state.moved(80.), None);
        assert!(state.showing());

        let epoch = state.release().expect("a held thumb");
        assert!(state.showing(), "letting go took the bar down at once");
        assert!(state.hide(epoch));
        assert!(!state.showing());
    }

    /// A release that had no thumb to let go of asks for no timer, which is
    /// every other mouse button release the view sees.
    #[test]
    fn releasing_nothing_arms_nothing() {
        let mut state = ScrollbarState::new();

        assert_eq!(state.release(), None);
    }
}
