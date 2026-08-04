// Rust links Windows binaries with the console subsystem by default, which
// flashes a console window before the GUI appears. Release builds use the GUI
// subsystem instead; debug builds keep the console so that env_logger output
// stays visible while developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! logman — a multi-platform GUI SSH terminal.
//!
//! The binary owns the application shell: a tab strip of open [`Session`]s, the
//! terminal surface of the active one, a status bar, and the connection dialog
//! rendered on top of everything else. Session state lives in [`session`], the
//! terminal surface in [`terminal_view`], and every reusable widget in [`ui`].
//!
//! A tab is not one session but a tree of panes ([`pane_tree`]), each showing
//! one session. Most tabs hold a single pane; splitting one is how a tab comes
//! to show several sessions side by side.

mod about_dialog;
mod app_settings;
mod caption;
mod connection;
mod file_panel;
mod files;
mod i18n;
mod icons;
// The pane tree is written as a self-contained data structure with its own
// tests rather than for the call sites the shell currently has, so it offers
// operations nothing reaches yet — editing a payload, listing the pane ids —
// which inside a binary crate read as dead code.
#[allow(dead_code)]
mod pane_tree;
mod session;
mod settings_dialog;
mod terminal_view;
mod theme_editor;
mod theme_store;
// The widget layer is written as a self-contained toolkit rather than for one
// call site, so it deliberately offers variants no current call site uses (the
// light theme, disabled inputs, the danger button). Inside a binary crate those
// read as dead code, hence the module-wide allow.
#[allow(dead_code)]
mod ui;
mod verifier;

// Compiles `locales/*.yml` into the binary and defines the machinery `t!`
// expands to, which is why it has to sit in the crate root. `fallback = "en"`
// is per key, not per locale: a string a translator has not got to yet shows
// in English while the rest of that language stays translated.
rust_i18n::i18n!("locales", fallback = "en");

use gpui::{
    AnyElement, App, Application, Bounds, Context, Div, DragMoveEvent, ElementId, Entity, EntityId,
    FocusHandle, Focusable, KeyBinding, Menu, MenuItem, MouseButton, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowOptions, actions, div, img,
    prelude::*, px, relative, size,
};
use logman_core::{SessionProfile, TitlebarStyle, WindowSettings};
use logman_ssh::SshAuth;

use about_dialog::{AboutDialog, AboutDialogEvent};
use caption::apply_caption_theme;
use connection::{ConnectionDialog, ConnectionDialogEvent};
use file_panel::FilePanel;
use i18n::ts;
use icons::Icons;
use pane_tree::{Axis, PaneId, PaneNode, PaneTree, SplitId};
use session::{Session, SessionStatus};
use settings_dialog::{SettingsDialog, SettingsDialogEvent};
use terminal_view::{PaneFocused, TerminalView};
use ui::{
    Button, ButtonVariant, ContextMenu, DraggedThumb, MenuButton, MenuEntry, Scrollbar,
    ScrollbarAxis, ScrollbarState, TabBar, TabItem, Theme, ThemeRegistry, WindowControlIcons,
    WindowControls, hide_later, scroll_to, scrolled, set_theme, theme, tooltip_label,
};

actions!(
    logman,
    [
        /// Quit the application.
        Quit,
        /// Open the connection dialog with an empty form.
        NewSession,
        /// Close the active pane, and with it the tab once it was the last one.
        CloseSession,
        /// Move keyboard focus to the next pane of the active tab.
        FocusNextPane,
        /// Move keyboard focus to the previous pane of the active tab.
        FocusPrevPane,
        /// Move the active pane out of its tab and into a tab of its own.
        BreakOutPane,
        /// Split the active pane, opening a second connection to the same host
        /// in the new pane to its right.
        DuplicateSplitRight,
        /// Split the active pane, opening a second connection to the same host
        /// in the new pane below it.
        DuplicateSplitBelow,
        /// Show or hide the remote file panel.
        ToggleFilePanel,
        /// Open the settings dialog.
        OpenSettings,
        /// Open the about dialog.
        ShowAbout,
        /// Close the open dialog or dropdown menu, if there is one.
        DismissDialog,
    ]
);

/// Activate the tab at the zero-based index carried by the action.
#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = logman, no_json)]
struct SelectTab(
    /// Zero-based index of the tab to activate.
    usize,
);

/// Key context the workspace-wide shortcuts are scoped to.
const KEY_CONTEXT: &str = "Workspace";

/// Number of tabs reachable through the `Ctrl`/`Cmd` + digit shortcuts.
const QUICK_SELECT_TABS: usize = 9;

/// Height of the toolbar row holding the application menu and the tab strip.
///
/// Must match the height [`TabBar`] gives itself, otherwise the menu button cell
/// and the tab strip would not line up.
const TOOLBAR_HEIGHT: f32 = 36.;

/// Distance from the top left of the window to the top left of the macOS
/// traffic lights, in the custom title bar style.
///
/// The buttons are 14 pt tall, so half the difference to [`TOOLBAR_HEIGHT`]
/// centres them in the toolbar band.
const TRAFFIC_LIGHT_ORIGIN: Point<Pixels> = Point {
    x: px(12.),
    y: px(11.),
};

/// Width kept clear at the left of the toolbar for the macOS traffic lights.
///
/// Three 14 pt buttons, 20 pt apart, starting at [`TRAFFIC_LIGHT_ORIGIN`], plus
/// the same margin again after the last one.
const TRAFFIC_LIGHT_GAP: f32 = 78.;

/// Modifier key named in the shortcut hints of the dropdown menu and the empty
/// state.
///
/// Never translated: it is the name printed on the key. It follows
/// [`bind_shortcuts`] on every platform so the two never drift.
const SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

/// Modifier key named in the shortcut hints of the pane commands.
///
/// Not [`SHORTCUT_MODIFIER`]: the pane shortcuts avoid `Ctrl` off macOS so that
/// the remote shell keeps it. Follows `pane_modifier` in [`bind_shortcuts`], and
/// like the other modifier name it is never translated.
const PANE_SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Alt"
};

/// Chord that shows and hides the remote file panel, as [`bind_shortcuts`]
/// registers it.
///
/// `Cmd+B` on macOS, where the modifier never reaches the shell. Elsewhere the
/// obvious `Ctrl+B` is out: it is tmux's prefix key and readline's
/// *backward-char*, and `Alt+B` — the modifier the pane commands fall back to —
/// is readline's *backward-word*. The shifted chord is free in a way neither of
/// those is, because a terminal cannot encode `Ctrl+Shift+B` distinctly from
/// `Ctrl+B` in the first place: taking it costs the remote shell nothing.
const PANEL_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd-b"
} else {
    "ctrl-shift-b"
};

/// Name of [`PANEL_SHORTCUT`] as the menus print it. Never translated, for the
/// same reason [`SHORTCUT_MODIFIER`] is not.
const PANEL_SHORTCUT_LABEL: &str = if cfg!(target_os = "macos") {
    "Cmd+B"
} else {
    "Ctrl+Shift+B"
};

/// Style group of the toolbar button that shows and hides the remote file
/// panel, so hovering the button recolours the icon inside it.
const PANEL_TOGGLE_GROUP: &str = "toggle-file-panel";

/// Narrowest pane, in terminal columns, a horizontal split may produce.
///
/// A pane below this is unusable — a shell prompt alone is wider — so a split
/// that would create one is refused instead.
const MIN_PANE_COLS: u16 = 20;

/// Shortest pane, in terminal rows, a vertical split may produce.
const MIN_PANE_ROWS: u16 = 6;

/// Smallest share of a split either of its children may be given.
///
/// Both the clamp a divider drag lands on and the renderer's own guard against
/// a stored ratio that would collapse a pane to nothing. A pane dragged to
/// zero would take its divider handle with it and leave no way to drag it back,
/// so the gesture stops short of the edge rather than letting that happen.
const MIN_SPLIT_RATIO: f32 = 0.1;

/// Thickness of the invisible grab area over a split's divider, in pixels.
///
/// The divider itself is drawn by the pane frames on either side of it, which
/// are a hairline each — far too thin to hit with a pointer. The handle is
/// pulled out of the flow with a negative margin of half this on both sides so
/// that widening the grab area moves nothing: it straddles the seam instead of
/// pushing the panes apart.
const SPLIT_HANDLE: f32 = 6.;

/// Element id of the tab strip's overlay scroll indicator.
///
/// Held here rather than inside [`TabBar`] because a drag of the thumb is
/// answered by the workspace, and the id is what tells this bar's drag from any
/// other bar's in the window.
const TAB_SCROLLBAR: &str = "tab-scrollbar";

/// The divider a drag is currently holding.
///
/// gpui delivers drag moves to every ancestor of the element the drag started
/// on, so a handle inside nested splits makes each enclosing split's listener
/// fire too. The id in here is how a listener recognises its own divider; the
/// distinct type is what keeps the gesture apart from the file drops the panel
/// accepts.
struct DraggedSplit {
    /// The split whose ratio the drag is writing.
    split: SplitId,
}

/// One pane: the view showing a session, plus the wiring that keeps the
/// workspace in step with it.
struct PaneLeaf {
    /// The terminal surface; it owns the [`Session`] entity.
    view: Entity<TerminalView>,
    /// Repaints the workspace when the session's title or status changes.
    _observer: Subscription,
    /// Records this pane as the active one when a click focuses its view.
    ///
    /// Driven by [`PaneFocused`] rather than `cx.on_focus`: gpui fires focus
    /// listeners after the frame that carried the click was already drawn, so
    /// a frame-swap driven that way would not show up until the next input
    /// event — the active-pane frame would visibly trail the click.
    _clicked: Subscription,
    /// Backstop for focus arriving by any route other than a click, e.g. a
    /// future programmatic `window.focus`. One frame late by gpui's dispatch
    /// order, which does not matter for paths that repaint anyway.
    _focus: Subscription,
}

/// One tab: a tree of panes, one of which is active.
struct SessionTab {
    /// The panes of this tab. Never empty — the last pane closes the tab.
    panes: PaneTree<PaneLeaf>,
    /// The pane the tab label, the status bar and the shortcuts act on.
    active_pane: PaneId,
}

impl SessionTab {
    /// A tab of a single pane showing `leaf`.
    fn single(leaf: PaneLeaf) -> Self {
        let panes = PaneTree::single(leaf);
        let active_pane = panes.first_leaf().0;
        Self { panes, active_pane }
    }

    /// The active pane, falling back to the first one.
    ///
    /// The fallback only matters if [`SessionTab::active_pane`] ever went stale;
    /// a tab always has a pane to speak for it, so this never fails.
    fn active_pane(&self) -> PaneId {
        if self.panes.contains(self.active_pane) {
            self.active_pane
        } else {
            self.panes.first_leaf().0
        }
    }

    /// The view of the active pane.
    fn active_view(&self) -> &Entity<TerminalView> {
        let pane = self.active_pane();
        match self.panes.get(pane) {
            Some(leaf) => &leaf.view,
            None => &self.panes.first_leaf().1.view,
        }
    }

    /// Every session in this tab, one per pane.
    fn sessions(&self, cx: &App) -> Vec<Entity<Session>> {
        self.panes
            .leaves()
            .into_iter()
            .map(|(_, leaf)| leaf.view.read(cx).session().clone())
            .collect()
    }

    /// The pane rendering `view`, if any.
    ///
    /// Panes are found by view rather than by id because a focus event only
    /// says which surface was focused, and a pane keeps its view across merges
    /// and break-outs.
    fn pane_of(&self, view: EntityId) -> Option<PaneId> {
        self.panes
            .leaves()
            .into_iter()
            .find(|(_, leaf)| leaf.view.entity_id() == view)
            .map(|(id, _)| id)
    }
}

/// The root view: tab strip, terminal surface, status bar and dialog.
struct Workspace {
    /// Focus target while no session is open, so the shortcuts stay live.
    focus_handle: FocusHandle,
    /// Open sessions, in tab order.
    tabs: Vec<SessionTab>,
    /// Index of the active tab; meaningless while [`Workspace::tabs`] is empty.
    active: usize,
    /// Horizontal scroll of the tab strip, used to reveal the active tab.
    tab_scroll: ScrollHandle,
    /// Whether the tab strip's overlay scroll indicator is on screen.
    tab_scrollbar: ScrollbarState,
    /// The connection dialog, rendered only while it reports itself open.
    dialog: Entity<ConnectionDialog>,
    /// The settings dialog, rendered only while it reports itself open.
    settings: Entity<SettingsDialog>,
    /// The about dialog, rendered only while it reports itself open.
    about: Entity<AboutDialog>,
    /// The remote file panel, shown to the left of the panes.
    ///
    /// One panel for the whole window rather than one per session: it keeps the
    /// browsing state of every session itself and shows whichever one the active
    /// pane belongs to.
    panel: Entity<FilePanel>,
    /// Whether the remote file panel is showing.
    ///
    /// Session state only. Persisting it would mean a settings key, and the
    /// panel is cheap enough to reopen that the key would earn its keep only
    /// once there is more to remember about it than one flag.
    panel_open: bool,
    /// Whether the application dropdown menu is showing.
    menu_open: bool,
    /// Whether the tab strip's dropdown tab list is showing.
    tab_menu_open: bool,
    /// The tab a right-click opened a context menu for, and where the pointer
    /// was when it did. `None` while no tab menu is showing.
    tab_context: Option<(usize, Point<Pixels>)>,
    /// Title bar style currently *on the window*.
    ///
    /// Starts as the style the window was created with and is re-set whenever
    /// the setting is applied, in the same breath as the window is told to
    /// switch. Not read from the settings directly: the toolbar has to branch on
    /// what the window actually carries, and only this field follows the
    /// platform call rather than the stored preference.
    titlebar: TitlebarStyle,
    /// Keeps the connection dialog subscription alive.
    _dialog_events: Subscription,
    /// Keeps the settings dialog subscription alive.
    _settings_events: Subscription,
    /// Keeps the about dialog subscription alive.
    _about_events: Subscription,
    /// Disconnects every session before the process exits.
    _quit: Subscription,
}

impl Workspace {
    /// Builds an empty workspace and wires up the connection dialog.
    ///
    /// `titlebar` is the style the window was opened with; from then on the
    /// field tracks whatever the applied settings switched the window to.
    fn new(titlebar: TitlebarStyle, window: &Window, cx: &mut Context<Self>) -> Self {
        let dialog = cx.new(ConnectionDialog::new);
        let dialog_events =
            cx.subscribe_in(
                &dialog,
                window,
                |this, dialog, event, window, cx| match event {
                    ConnectionDialogEvent::Connect { profile, auth } => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.open_session(profile.clone(), auth.clone(), window, cx);
                    }
                    #[cfg(unix)]
                    ConnectionDialogEvent::ConnectLocal => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.open_local_session(window, cx);
                    }
                    ConnectionDialogEvent::Dismissed => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.focus_active(window, cx);
                    }
                },
            );

        let settings = cx.new(SettingsDialog::new);
        let settings_events = cx.subscribe_in(
            &settings,
            window,
            |this, dialog, event, window, cx| match event {
                // The dialog has already replaced and persisted the settings
                // global by the time it emits this; the shell re-applies the
                // parts that touch live windows and sessions.
                SettingsDialogEvent::Applied => {
                    this.apply_settings(window, cx);
                    // The dialog closes itself after applying; without a refocus
                    // the window focus dangles on its unrendered controls and
                    // macOS disables every menu item validated through it.
                    this.focus_active(window, cx);
                }
                // The same work, minus the refocus: the dialog is still open and
                // the user is still typing in it, so taking the focus back to
                // the terminal here would pull it out from under them.
                SettingsDialogEvent::ThemesChanged => this.apply_settings(window, cx),
                SettingsDialogEvent::Dismissed => {
                    dialog.update(cx, |dialog, cx| dialog.close(cx));
                    this.focus_active(window, cx);
                }
            },
        );

        let about = cx.new(AboutDialog::new);
        let about_events =
            cx.subscribe_in(
                &about,
                window,
                |this, dialog, event, window, cx| match event {
                    AboutDialogEvent::Dismissed => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.focus_active(window, cx);
                    }
                },
            );

        let quit = cx.on_app_quit(|this, cx| {
            for session in this.sessions(cx) {
                session.update(cx, |session, cx| session.disconnect(cx));
            }
            async {}
        });

        let panel = cx.new(FilePanel::new);

        Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            tab_scroll: ScrollHandle::new(),
            tab_scrollbar: ScrollbarState::new(),
            dialog,
            settings,
            about,
            panel,
            panel_open: true,
            menu_open: false,
            tab_menu_open: false,
            tab_context: None,
            titlebar,
            _dialog_events: dialog_events,
            _settings_events: settings_events,
            _about_events: about_events,
            _quit: quit,
        }
    }

    /// Every session the workspace holds, across all tabs and panes.
    fn sessions(&self, cx: &App) -> Vec<Entity<Session>> {
        self.tabs.iter().flat_map(|tab| tab.sessions(cx)).collect()
    }

    /// Re-applies the current settings to the window and every open session.
    ///
    /// Shared by the two things that can make the settings mean something new:
    /// saving them, and changing a theme or scheme file the settings point at.
    /// Deliberately does *not* move the focus — where the focus belongs after
    /// this depends on whether the dialog closed, which only the caller knows.
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        // Before the repaint below, so the next frame is already drawn in the
        // newly chosen language.
        i18n::apply(settings.language.as_deref());
        // The native macOS menu bar is built once and owned by the platform, so
        // unlike the in-app menu it does not follow a repaint; it has to be
        // handed over again.
        cx.set_menus(app_menus());
        apply_ui_theme(&settings.ui_theme, cx);
        // Ahead of the repaint, so the toolbar's next frame already knows
        // whether it has to stand in for a title bar; and ahead of the two
        // calls below, which leave the accent policy and the caption colors on
        // the window, so a caption that comes back here comes back already
        // themed.
        //
        // The field follows the call rather than the stored setting: everything
        // that branches on it is asking what the window carries, not what was
        // last saved.
        if settings.window.titlebar != self.titlebar {
            self.titlebar = settings.window.titlebar;
            let custom = self.titlebar == TitlebarStyle::Custom;
            window.set_titlebar_transparent(custom, custom.then_some(TRAFFIC_LIGHT_ORIGIN));
            // The Linux counterpart of the call above, which only the Windows
            // and macOS backends implement: swap the compositor's frame for
            // client-side decorations (or back) on the live window.
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            window.request_decorations(if custom {
                gpui::WindowDecorations::Client
            } else {
                gpui::WindowDecorations::Server
            });
        }
        cx.refresh_windows();
        window.set_background_appearance(window_appearance(&settings.window));
        // After the background appearance, never before: on Windows that call
        // re-arms the accent policy that would otherwise repaint the caption
        // out from under us.
        apply_caption_theme(window, &theme(cx));
        // Every pane of every tab, not just the visible one: a background tab's
        // terminal has to come back in the newly chosen scheme too.
        for session in self.sessions(cx) {
            session.update(cx, |session, cx| session.apply_settings(cx));
        }
    }

    /// Opens a session for `profile` and makes its tab active.
    fn open_session(
        &mut self,
        profile: SessionProfile,
        auth: SshAuth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::info!("opening a session to {}", profile.label());
        let session = cx.new(|cx| Session::new(profile, auth, cx));
        self.adopt_session(session, window, cx);
    }

    /// Opens a shell on this machine and makes its tab active.
    ///
    /// Takes nothing, because a local session is configured by nothing: the
    /// shell is the user's login shell and everything else comes from the
    /// global terminal settings.
    #[cfg(unix)]
    fn open_local_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = cx.new(Session::new_local);
        log::info!(
            "opening a local session running {}",
            session.read(cx).label()
        );
        self.adopt_session(session, window, cx);
    }

    /// Gives a freshly built session a view, a pane and a tab of its own, and
    /// activates that tab.
    ///
    /// Everything past the constructor is identical for a remote and a local
    /// session, which is the whole point of them being one type.
    fn adopt_session(
        &mut self,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| TerminalView::new(session.clone(), window, cx));
        let leaf = self.new_pane(view, session, window, cx);

        self.tabs.push(SessionTab::single(leaf));
        self.active = self.tabs.len() - 1;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Wires a freshly created terminal view up as a pane.
    fn new_pane(
        &mut self,
        view: Entity<TerminalView>,
        session: Entity<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneLeaf {
        // Repaints on any session change; on a disconnect it also retires the
        // pane. `observe_in` rather than `observe` because closing a pane moves
        // focus, and focus needs the window.
        let observer = cx.observe_in(&session, window, |this, session, window, cx| {
            if matches!(
                session.read(cx).status(),
                SessionStatus::Disconnected { .. }
            ) {
                this.close_pane_for_session(session.entity_id(), window, cx);
            }
            cx.notify();
        });
        let handle = view.read(cx).focus_handle(cx);
        let id = view.entity_id();
        let clicked = cx.subscribe(&view, |this, view, _: &PaneFocused, cx| {
            this.on_pane_focused(view.entity_id(), cx);
        });
        let focus = cx.on_focus(&handle, window, move |this, _window, cx| {
            this.on_pane_focused(id, cx);
        });

        PaneLeaf {
            view,
            _observer: observer,
            _clicked: clicked,
            _focus: focus,
        }
    }

    /// Records the pane rendering `view` as the active one of its tab.
    ///
    /// This is what makes a click inside a pane — [`TerminalView`] focuses
    /// itself on mouse down — move the active-pane marker, the status bar and
    /// the tab label onto that pane.
    fn on_pane_focused(&mut self, view: EntityId, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            let Some(pane) = tab.pane_of(view) else {
                continue;
            };
            if tab.active_pane != pane {
                tab.active_pane = pane;
                cx.notify();
            }
            return;
        }
    }

    /// Activates the tab at `index`, if it exists.
    ///
    /// Selecting the tab that is already active is not a no-op: it scrolls the
    /// strip back to it, which is the point of picking it from the tab list.
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Disconnects and removes the tab at `index`, panes and all.
    ///
    /// This is the tab strip's close button: a tab that was split closes as a
    /// unit. Closing one pane at a time is [`Workspace::close_active_pane`].
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        let tab = self.tabs.remove(index);
        for session in tab.sessions(cx) {
            self.forget_panel_session(session.entity_id(), cx);
            session.update(cx, |session, cx| session.disconnect(cx));
        }

        // Removing a tab in front of the active one shifts it down a slot.
        if index < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Disconnects and removes the active pane of the active tab.
    ///
    /// The pane's sibling grows into the space it leaves. On the last pane of a
    /// tab there is no sibling to grow, so the tab goes with it.
    fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        self.remove_pane(self.active, tab.active_pane(), window, cx);
    }

    /// Retires the pane of a session whose connection has ended.
    ///
    /// This is the automatic arm of the close policy, driven by the session
    /// observer in [`Self::new_pane`]:
    ///
    /// * `Disconnected` — the remote shell exited or the server hung up — the
    ///   pane closes by itself; its sibling grows, and the tab goes once its
    ///   last pane does. When the last tab goes, the workspace shows the start
    ///   screen again rather than quitting.
    /// * `Failed` never lands here: a session that could not connect keeps its
    ///   pane, so the error and its Reconnect button stay readable.
    ///
    /// A session that is no longer in any tab — the manual close paths remove
    /// the pane *before* disconnecting it — is a no-op, which is also what
    /// makes the observer re-entrancy safe.
    fn close_pane_for_session(
        &mut self,
        session: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.tabs.iter().enumerate().find_map(|(index, tab)| {
            tab.panes.leaves().into_iter().find_map(|(pane, leaf)| {
                (leaf.view.read(cx).session().entity_id() == session).then_some((index, pane))
            })
        });
        let Some((index, pane)) = found else {
            return;
        };
        self.remove_pane(index, pane, window, cx);
    }

    /// Disconnects and removes one pane of the tab at `index`.
    ///
    /// The pane's sibling grows into the space it leaves. On the last pane of a
    /// tab there is no sibling to grow, so the tab goes with it. Focus only
    /// moves when the removed pane sat in the active tab; a background tab
    /// shrinking must not steal the keyboard.
    fn remove_pane(
        &mut self,
        index: usize,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.panes.leaf_count() < 2 {
            self.close_tab(index, window, cx);
            return;
        }

        // Read before the removal, while the neighbour is still in the tree.
        let successor = tab.panes.next_leaf(pane);

        let tab = &mut self.tabs[index];
        let Some(leaf) = tab.panes.remove(pane) else {
            return;
        };
        // The removed pane may not have been the active one — an idle split
        // closing in the background — in which case the active pane stands.
        if !tab.panes.contains(tab.active_pane) {
            tab.active_pane = successor
                .filter(|id| tab.panes.contains(*id))
                .unwrap_or_else(|| tab.panes.first_leaf().0);
        }

        // Dropping the leaf takes its subscriptions and its view with it, so the
        // session has to be told to hang up first. Hanging up twice — the
        // automatic path arrives here already disconnected — is a no-op.
        let session = leaf.view.read(cx).session().clone();
        self.forget_panel_session(session.entity_id(), cx);
        session.update(cx, |session, cx| session.disconnect(cx));

        if index == self.active {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Turns the tab at `source` into a split of the active tab.
    ///
    /// The source tab leaves the strip and its panes — the whole subtree, if it
    /// was itself split — appear next to the active pane, along `axis`. Focus
    /// follows the panes that moved.
    ///
    /// Splitting is always "merge another open tab in", so it needs a target the
    /// user picks: [`Workspace::render_tab_context`] is the only way in, and
    /// there is no shortcut for it.
    pub(crate) fn merge_tab_into_active(
        &mut self,
        source: usize,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source >= self.tabs.len() || source == self.active {
            return;
        }
        if !self.can_split_active(axis, cx) {
            // Only reachable from a stale menu: the rows offering a split are
            // left out while the pane is this small.
            log::info!("refusing to merge tab {source}: the active pane is too small to split");
            return;
        }

        let target_pane = self.tabs[self.active].active_pane();
        let incoming = self.tabs.remove(source);
        // Removing a tab in front of the active one shifts it down a slot.
        if source < self.active {
            self.active -= 1;
        }

        let follow = incoming.active_pane();
        let tab = &mut self.tabs[self.active];
        if !tab.panes.merge_subtree(target_pane, axis, incoming.panes) {
            // `target_pane` came from this very tab a moment ago, so this is
            // unreachable; logged rather than ignored because reaching it would
            // mean a pane has been dropped on the floor.
            log::error!("the pane to split has vanished; the merge was dropped");
            return;
        }
        tab.active_pane = follow;

        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Splits the active pane along `axis` and opens a second connection to the
    /// same host in the new half.
    ///
    /// The other half of splitting, and the one that needs no target: everything
    /// it has to know — the profile and the credentials — is already in the pane
    /// the user is looking at, which is why this one *can* have a shortcut where
    /// [`Workspace::merge_tab_into_active`] cannot.
    ///
    /// The new session is independent from the moment it is created: its own
    /// transport, its own shell, its own scrollback. Nothing about the state of
    /// the original matters, so a pane whose connection failed can still be
    /// split — that is how the user retries without losing the error on screen.
    pub(crate) fn duplicate_split(
        &mut self,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if !self.can_split_active(axis, cx) {
            // Reachable from the keyboard at any size, unlike the menu rows,
            // which are left out while the pane is this small.
            log::info!("refusing to split: the active pane is too small");
            return;
        }

        let target_pane = tab.active_pane();
        let session = tab.active_view().read(cx).session().clone();
        log::info!("opening a second session to {}", session.read(cx).title());

        let session = session.update(cx, |session, cx| session.duplicate(cx));
        let view = cx.new(|cx| TerminalView::new(session.clone(), window, cx));
        let leaf = self.new_pane(view, session, window, cx);

        let tab = &mut self.tabs[self.active];
        let Some(pane) = tab.panes.split(target_pane, axis, leaf) else {
            // `target_pane` came out of this very tab a moment ago, so this is
            // unreachable; logged rather than ignored because reaching it would
            // mean a live session has been dropped on the floor.
            log::error!("the pane to split has vanished; the new session was dropped");
            return;
        };
        tab.active_pane = pane;

        self.focus_active(window, cx);
        cx.notify();
    }

    /// Moves the active pane into a tab of its own, right after the current one.
    ///
    /// The session keeps running throughout: the pane, its view and its
    /// subscriptions move over unchanged. A no-op on an unsplit tab, which is
    /// already exactly this.
    pub(crate) fn break_out_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.panes.leaf_count() < 2 {
            return;
        }

        let pane = tab.active_pane();
        let successor = tab.panes.next_leaf(pane);

        let tab = &mut self.tabs[self.active];
        let Some(leaf) = tab.panes.remove(pane) else {
            return;
        };
        tab.active_pane = successor
            .filter(|id| tab.panes.contains(*id))
            .unwrap_or_else(|| tab.panes.first_leaf().0);

        let index = self.active + 1;
        self.tabs.insert(index, SessionTab::single(leaf));
        self.active = index;
        self.reveal_active_tab();
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Moves focus to the next pane of the active tab, wrapping around.
    pub(crate) fn focus_next_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_pane(true, window, cx);
    }

    /// Moves focus to the previous pane of the active tab, wrapping around.
    pub(crate) fn focus_prev_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_pane(false, window, cx);
    }

    /// Steps the active pane one place through the active tab's focus cycle.
    fn cycle_pane(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab.panes.leaf_count() < 2 {
            return;
        }

        let from = tab.active_pane();
        let next = if forward {
            tab.panes.next_leaf(from)
        } else {
            tab.panes.prev_leaf(from)
        };
        let Some(next) = next else {
            return;
        };

        tab.active_pane = next;
        // Focusing the pane's grid also runs `on_pane_focused`, which is
        // harmless: it finds the pane already marked active.
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Whether the active pane is big enough to be split along `axis`.
    ///
    /// The two halves inherit roughly half of the pane's current grid each, so
    /// the check is on the live column or row count rather than on pixels: a
    /// pane that would come out narrower than [`MIN_PANE_COLS`] or shorter than
    /// [`MIN_PANE_ROWS`] is not worth having.
    ///
    /// Silent, because the tab context menu asks this on every frame it is open
    /// to decide which rows to show; the refusal is logged where it happens.
    fn can_split_active(&self, axis: Axis, cx: &App) -> bool {
        let Some(tab) = self.tabs.get(self.active) else {
            return false;
        };
        let (cols, rows) = tab
            .active_view()
            .read(cx)
            .session()
            .read(cx)
            .terminal()
            .size();
        match axis {
            Axis::Horizontal => cols / 2 >= MIN_PANE_COLS,
            Axis::Vertical => rows / 2 >= MIN_PANE_ROWS,
        }
    }

    /// Scrolls the tab strip so that the active tab is on screen.
    ///
    /// The strip applies this during its next prepaint, so callers have to ask
    /// for a repaint as well.
    fn reveal_active_tab(&self) {
        if !self.tabs.is_empty() {
            self.tab_scroll.scroll_to_item(self.active);
        }
    }

    /// Moves keyboard focus onto the active pane's terminal, or onto the
    /// workspace itself when no session is open.
    ///
    /// Without this the shortcuts stop working after the last tab is closed,
    /// because their key context only exists while something inside the
    /// workspace is focused.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(tab) => {
                let handle = tab.active_view().read(cx).focus_handle(cx);
                window.focus(&handle);
            }
            None => window.focus(&self.focus_handle),
        }
    }

    /// Whether one of the modal dialogs is on screen.
    ///
    /// A modal takes the window over, so anything the strip would otherwise open
    /// on top of it has to stand down.
    fn dialog_open(&self, cx: &App) -> bool {
        self.dialog.read(cx).is_open()
            || self.settings.read(cx).is_open()
            || self.about.read(cx).is_open()
    }

    /// Closes every dialog and the dropdown menu.
    ///
    /// Every `open_*` method starts here, which is what keeps the three modals
    /// mutually exclusive: only one of them can ever be on screen, and opening
    /// one always puts the menu away.
    fn close_overlays(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.tab_menu_open = false;
        self.tab_context = None;
        if self.dialog.read(cx).is_open() {
            self.dialog.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.settings.read(cx).is_open() {
            self.settings.update(cx, |dialog, cx| dialog.close(cx));
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
        }
    }

    /// Shows the connection dialog with an empty form.
    fn open_dialog(&mut self, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        self.dialog.update(cx, |dialog, cx| dialog.open_new(cx));
        cx.notify();
    }

    /// Shows the connection dialog pre-filled from a saved profile.
    ///
    /// A profile the user already finished configuring — a remembered password,
    /// or a key that needs no passphrase — carries everything the transport
    /// needs, so presenting the dialog again would be one form to dismiss
    /// before a session the user has already asked for. Those profiles connect
    /// on the click.
    ///
    /// The dialog still opens, pre-filled, whenever anything would have to be
    /// typed or corrected: a password that was never remembered, an encrypted
    /// key with no stored passphrase, a key file that has gone missing, or the
    /// agent method, which the transport does not implement.
    ///
    /// Deciding that reads the OS keychain, and possibly the key file,
    /// synchronously on the UI thread — the same work the dialog's Connect
    /// button does, one click earlier.
    fn open_profile(&mut self, profile: &SessionProfile, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        let id = profile.id;
        self.dialog
            .update(cx, |dialog, cx| dialog.open_profile(id, cx));
        cx.notify();
    }

    /// Shows the settings dialog.
    fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        self.settings.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Shows the about dialog.
    fn open_about(&mut self, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        self.about.update(cx, |dialog, cx| dialog.open(cx));
        cx.notify();
    }

    /// Shows or hides the file panel.
    ///
    /// One command whichever session is active: a remote one browses the server
    /// over SFTP and a local one browses this computer, so there is always a
    /// filesystem behind the panel and never a reason to refuse to open it.
    fn toggle_file_panel(&mut self, cx: &mut Context<Self>) {
        self.panel_open = !self.panel_open;
        cx.notify();
    }

    /// Tells the file panel which session it is looking at.
    ///
    /// Called from the render pass rather than from each of the eight places
    /// that can change the active pane, so there is no site left to forget. The
    /// panel compares the session against the one it already holds and returns
    /// without repainting when they match, which is every frame but the ones
    /// that actually switch.
    fn sync_file_panel(&self, cx: &mut Context<Self>) {
        let session = self
            .tabs
            .get(self.active)
            .map(|tab| tab.active_view().read(cx).session().clone());
        self.panel
            .update(cx, |panel, cx| panel.set_session(session, cx));
    }

    /// Drops a closed session's browsing state from the file panel.
    fn forget_panel_session(&self, session: EntityId, cx: &mut Context<Self>) {
        self.panel
            .update(cx, |panel, cx| panel.forget_session(session, cx));
    }

    /// Shows or hides the application dropdown menu.
    fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open == open {
            return;
        }
        self.menu_open = open;
        cx.notify();
    }

    /// Shows or hides the tab strip's dropdown tab list.
    fn set_tab_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.tab_menu_open == open {
            return;
        }
        self.tab_menu_open = open;
        cx.notify();
    }

    /// Opens the context menu of the tab at `index`, with its corner at `at`.
    ///
    /// The right-click that gets here does not change the active tab, so `index`
    /// and [`Workspace::active`] are independent — which is what the menu's
    /// commands are built around.
    fn open_tab_context(&mut self, index: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || self.dialog_open(cx) {
            return;
        }
        // Not `close_overlays`: a modal dialog outranks the strip — the guard
        // above leaves it alone — while the two dropdowns are simply mutually
        // exclusive with this menu.
        self.menu_open = false;
        self.tab_menu_open = false;
        self.tab_context = Some((index, at));
        cx.notify();
    }

    /// Puts the tab context menu away, if one is open.
    fn close_tab_context(&mut self, cx: &mut Context<Self>) {
        if self.tab_context.take().is_some() {
            cx.notify();
        }
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + <kbd>T</kbd>.
    fn new_session_action(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(cx);
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + <kbd>W</kbd>.
    ///
    /// Closes the active pane rather than the whole tab, the way a split editor
    /// or terminal does: on an unsplit tab the two are the same thing, and on a
    /// split one closing every pane in turn ends up closing the tab.
    fn close_session_action(
        &mut self,
        _: &CloseSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_active_pane(window, cx);
    }

    /// Handles the pane focus shortcut for the next pane.
    fn focus_next_pane_action(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_next_pane(window, cx);
    }

    /// Handles the pane focus shortcut for the previous pane.
    fn focus_prev_pane_action(
        &mut self,
        _: &FocusPrevPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_prev_pane(window, cx);
    }

    /// Handles the shortcut that pulls the active pane out into its own tab.
    fn break_out_pane_action(
        &mut self,
        _: &BreakOutPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.break_out_active_pane(window, cx);
    }

    /// Handles the shortcut that splits the active pane to the right.
    fn duplicate_split_right_action(
        &mut self,
        _: &DuplicateSplitRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate_split(Axis::Horizontal, window, cx);
    }

    /// Handles the shortcut that splits the active pane downwards.
    fn duplicate_split_below_action(
        &mut self,
        _: &DuplicateSplitBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate_split(Axis::Vertical, window, cx);
    }

    /// Handles the shortcut that shows and hides the remote file panel.
    fn toggle_file_panel_action(
        &mut self,
        _: &ToggleFilePanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_file_panel(cx);
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + <kbd>,</kbd>.
    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(cx);
    }

    /// Handles the "About logman" menu item.
    fn show_about_action(&mut self, _: &ShowAbout, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_about(cx);
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + a digit.
    fn select_tab_action(
        &mut self,
        action: &SelectTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_tab(action.0, window, cx);
    }

    /// Handles <kbd>Esc</kbd>: closes whichever overlay is open, or lets the key
    /// through to the terminal when none is.
    fn dismiss_dialog_action(
        &mut self,
        _: &DismissDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The dropdown menus paint above everything else, so they are dismissed
        // first.
        if self.tab_context.is_some() {
            self.close_tab_context(cx);
            return;
        }
        if self.menu_open {
            self.set_menu_open(false, cx);
            return;
        }
        if self.tab_menu_open {
            self.set_tab_menu_open(false, cx);
            return;
        }
        if self.about.read(cx).is_open() {
            self.about.update(cx, |dialog, cx| dialog.close(cx));
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        if self.dialog.read(cx).is_open() {
            // Route through `dismiss` rather than closing directly: the dialog
            // also binds `Escape` internally, and going through one path keeps
            // `Dismissed` firing exactly once no matter which handler wins the
            // dispatch. Closing and restoring focus is then the subscription's
            // job.
            self.dialog.update(cx, |dialog, cx| dialog.dismiss(cx));
            cx.notify();
            return;
        }
        if self.settings.read(cx).is_open() {
            self.settings.update(cx, |dialog, cx| dialog.close(cx));
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        cx.propagate();
    }

    /// Renders the toolbar: the application menu button and the tab strip.
    ///
    /// The button is left out on macOS, where [`app_menus`] puts the same
    /// commands in the system menu bar.
    ///
    /// In the custom title bar style this row *is* the title bar. It then marks
    /// itself as the window's drag area, takes over writing the application's
    /// name at its left end, and — off macOS, which keeps its native traffic
    /// lights — grows a set of caption buttons at its right end. Every
    /// *control* inside it occludes, so the drag area only ever answers for the
    /// gaps between them; see [`ui::window_controls`]. The name is not a
    /// control and deliberately does not.
    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let custom = draws_own_titlebar(self.titlebar, window);
        let menu = (!cfg!(target_os = "macos")).then(|| self.render_app_menu(cx));
        // Nothing to browse without a session, so the toggle goes with the panel
        // it would open. A session of either kind has a filesystem behind it —
        // the server's, or this computer's — so nothing finer is asked here.
        let toggle = (!self.tabs.is_empty()).then(|| {
            let open = self.panel_open;
            let hover = theme.surface_hover;
            // The open state is already carried by the accent colour, so only
            // the closed button brightens on hover. The icon is tinted by its
            // own `text_color` rather than the button's, so the hover shade has
            // to reach it through the group.
            let hover_text = if open { theme.accent } else { theme.text };
            div()
                .id("toggle-file-panel")
                // The row behind it may be a window drag area; see
                // [`ui::window_controls`].
                .occlude()
                .group(PANEL_TOGGLE_GROUP)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_md()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(|workspace, _, _window, cx| {
                    workspace.toggle_file_panel(cx);
                }))
                // The shortcut rides along, the way the dropdown row for the
                // same command carries it: this button is the only place the
                // binding is discoverable on macOS, where there is no in-app
                // menu to read it off.
                .tooltip(tooltip_label(ts!(
                    "files.tip_toggle",
                    shortcut = PANEL_SHORTCUT_LABEL
                )))
                .child(
                    icons::icon(
                        icons::PANEL,
                        px(16.),
                        if open { theme.accent } else { theme.text_muted },
                    )
                    .group_hover(PANEL_TOGGLE_GROUP, move |style| {
                        style.text_color(hover_text)
                    }),
                )
        });

        // One cell for the leading controls, so the menu button and the panel
        // toggle share the toolbar's fill and bottom hairline with the strip.
        let leading = (menu.is_some() || toggle.is_some()).then(|| {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(4.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .children(menu)
                .children(toggle)
        });

        // Room for the traffic lights AppKit still draws over the transparent
        // title bar. Painted like the leading cell rather than left empty, so
        // the band reads as one strip. Fullscreen hides the buttons, and the
        // gap goes with them.
        let traffic_lights =
            (custom && cfg!(target_os = "macos") && !window.is_fullscreen()).then(|| {
                div()
                    .flex_none()
                    .w(px(TRAFFIC_LIGHT_GAP))
                    .h(px(TOOLBAR_HEIGHT))
                    .bg(theme.surface)
                    .border_b_1()
                    .border_color(theme.border)
            });

        // The application's own name, which only the custom style has to write:
        // a system title bar already carries it, and drawing it twice would put
        // it in two places at once.
        //
        // Windows and the GTK/KDE captions set an application icon beside the
        // title and macOS does not, so the icon follows that split. It is a
        // colour image, so `img` rather than `svg`: the latter keeps only an
        // icon's coverage and would paint the mark as one flat tint.
        //
        // Nothing here is interactive, and — unlike every control in this row —
        // nothing here occludes either. The name and the mark are part of the
        // *empty* title bar as far as the window is concerned, so a press on
        // them has to reach the drag area underneath and move the window.
        let title = custom.then(|| {
            // `Resource::Embedded` spelled out rather than left to `img`'s
            // `From<&str>`: that conversion asks the http `Uri` parser first,
            // and a bare file name parses as a valid relative URI — the icon
            // would be "fetched" instead of read from the asset source, and
            // the fetch can only fail.
            let icon = (!cfg!(target_os = "macos")).then(|| {
                img(gpui::ImageSource::Resource(gpui::Resource::Embedded(
                    icons::APP_ICON.into(),
                )))
                .size(px(16.))
                .flex_none()
            });
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(TOOLBAR_HEIGHT))
                .px(px(10.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                // A shade quieter than a tab title, which is the one label in
                // this row that has to be read.
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .children(icon)
                .child("logman")
        });

        // The caption buttons the other two platforms have to draw themselves.
        let controls = (custom && !cfg!(target_os = "macos")).then(|| {
            WindowControls::new(
                "window-controls",
                WindowControlIcons {
                    minimize: icons::WINDOW_MINIMIZE.into(),
                    maximize: icons::WINDOW_MAXIMIZE.into(),
                    restore: icons::WINDOW_RESTORE.into(),
                    close: icons::WINDOW_CLOSE.into(),
                },
            )
        });

        div()
            .id("toolbar")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .w_full()
            .h(px(TOOLBAR_HEIGHT))
            .when(custom, |this| {
                // Occluding is load-bearing, not just hygiene: the workspace
                // root tracks focus, and gpui's focus transfer marks every
                // mouse down over it `default_prevented` — which the Windows
                // backend reads as "the app took this press", swallowing the
                // `HTCAPTION` down that would have started the system drag.
                // Cutting the root's hitbox out from under the strip keeps the
                // press unclaimed, and spares the terminal a focus loss for a
                // click that was aimed at the window, not the app.
                titlebar_gestures(this.occlude().window_control_area(WindowControlArea::Drag))
            })
            .children(traffic_lights)
            .children(title)
            .children(leading)
            .child(div().flex_1().min_w_0().child(self.render_tab_bar(cx)))
            .children(controls)
            .into_any_element()
    }

    /// Builds the dropdown menu shown on the platforms without a native one.
    ///
    /// Every row dispatches the action its keyboard shortcut dispatches, so the
    /// menu adds a way in rather than a second implementation.
    ///
    /// Splitting with a second connection is here, and so is breaking a pane
    /// out; merging a tab in is not, and cannot be: a merge needs a *source*
    /// tab, which a menu of static commands has no way to name. That one half of
    /// splitting lives in the tab context menu alone — see
    /// [`Workspace::render_tab_context`] — and the same asymmetry shapes
    /// [`app_menus`].
    fn render_app_menu(&self, cx: &mut Context<Self>) -> MenuButton {
        let this = cx.entity();
        let entries = vec![
            MenuEntry::new(ts!("menu.new_session"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewSession), cx)),
            MenuEntry::new(ts!("menu.duplicate_right"))
                .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+D"))
                .on_activate(|window, cx| {
                    window.dispatch_action(Box::new(DuplicateSplitRight), cx)
                }),
            MenuEntry::new(ts!("menu.duplicate_below"))
                .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+S"))
                .on_activate(|window, cx| {
                    window.dispatch_action(Box::new(DuplicateSplitBelow), cx)
                }),
            MenuEntry::new(ts!("menu.break_out_pane"))
                .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+B"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(BreakOutPane), cx)),
            MenuEntry::new(ts!("files.toggle"))
                .shortcut(PANEL_SHORTCUT_LABEL)
                .on_activate(|window, cx| window.dispatch_action(Box::new(ToggleFilePanel), cx)),
            MenuEntry::new(ts!("menu.settings"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+,"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            MenuEntry::separator(),
            MenuEntry::new(ts!("menu.about"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(ShowAbout), cx)),
            MenuEntry::separator(),
            MenuEntry::new(ts!("menu.quit"))
                .shortcut(format!("{SHORTCUT_MODIFIER}+Q"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(Quit), cx)),
        ];

        MenuButton::new("app-menu")
            .tooltip(ts!("menu.tip_menu"))
            .open(self.menu_open)
            .entries(entries)
            .on_open_change(move |open, _window, cx| {
                this.update(cx, |workspace, cx| workspace.set_menu_open(open, cx));
            })
    }

    /// Renders the tab strip.
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let this = cx.entity();
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                // A split tab is labelled after its active pane, so the strip
                // says what the user is looking at rather than what the tab
                // happened to be opened as.
                let session = tab.active_view().read(cx).session().read(cx);
                TabItem::new(("session-tab", index), session.title()).status(session.tab_status())
            })
            .collect();

        TabBar::new("session-tabs")
            .tabs(tabs)
            .active(self.active)
            .scroll_handle(&self.tab_scroll)
            .scrollbar(self.tab_scrollbar())
            .menu_icon(icons::TAB_LIST)
            .new_icon(icons::NEW_TAB)
            // The close button reuses the tab menu's own row: it is the same
            // command, worded the same way, and neither takes an ellipsis.
            .tooltips(
                ts!("tab.tip_list"),
                ts!("tab.tip_new", shortcut = format!("{SHORTCUT_MODIFIER}+T")),
                ts!("tab.close"),
            )
            .menu_open(self.tab_menu_open)
            .on_menu_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |workspace, cx| workspace.set_tab_menu_open(open, cx));
                }
            })
            .on_select({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| workspace.select_tab(index, window, cx));
                }
            })
            .on_close({
                let this = this.clone();
                move |index, window, cx| {
                    this.update(cx, |workspace, cx| workspace.close_tab(index, window, cx));
                }
            })
            .on_context_menu({
                let this = this.clone();
                move |index, at, _window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.open_tab_context(index, at, cx)
                    });
                }
            })
            .on_new(move |_window, cx| {
                this.update(cx, |workspace, cx| workspace.open_dialog(cx));
            })
    }

    /// Renders the context menu of a right-clicked tab, if one is open.
    ///
    /// The commands depend on which tab was clicked, because both of them are
    /// about the active tab:
    ///
    /// * on another tab, the menu merges *that* tab into the active one as a
    ///   split — the only way to bring an existing session in, and the reason
    ///   that half of splitting has no shortcut;
    /// * on the active tab, it splits the active pane off into a second
    ///   connection to the same host, and offers the reverse of a merge: moving
    ///   the active pane back out into a tab of its own, which needs the tab to
    ///   actually be split.
    ///
    /// A row whose command would be refused is left out rather than shown doing
    /// nothing, so the menu can come down to nothing but "close this tab".
    fn render_tab_context(&self, cx: &mut Context<Self>) -> Option<ContextMenu> {
        let (index, position) = self.tab_context?;
        // The strip and the stored index are a frame apart: a tab can be gone by
        // now — closed from the menu itself, or by the session that owned it.
        let tab = self.tabs.get(index)?;
        let this = cx.entity();

        let mut entries = Vec::new();
        if index == self.active {
            // A split that would leave an unusably small pane is refused, so the
            // row asking for it is left out rather than offered and ignored.
            if self.can_split_active(Axis::Horizontal, cx) {
                entries.push(
                    MenuEntry::new(ts!("tab.duplicate_right"))
                        .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+D"))
                        .on_activate(|window, cx| {
                            window.dispatch_action(Box::new(DuplicateSplitRight), cx)
                        }),
                );
            }
            if self.can_split_active(Axis::Vertical, cx) {
                entries.push(
                    MenuEntry::new(ts!("tab.duplicate_below"))
                        .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+S"))
                        .on_activate(|window, cx| {
                            window.dispatch_action(Box::new(DuplicateSplitBelow), cx)
                        }),
                );
            }
            if !entries.is_empty() {
                entries.push(MenuEntry::separator());
            }
            if tab.panes.leaf_count() > 1 {
                entries.push(
                    MenuEntry::new(ts!("menu.break_out_pane"))
                        .shortcut(format!("{PANE_SHORTCUT_MODIFIER}+Shift+B"))
                        .on_activate(|window, cx| {
                            window.dispatch_action(Box::new(BreakOutPane), cx)
                        }),
                );
                entries.push(MenuEntry::separator());
            }
        } else {
            // A split that would leave an unusably small pane is refused, so the
            // row asking for it is left out rather than offered and ignored.
            for (label, axis) in [
                (ts!("tab.split_right"), Axis::Horizontal),
                (ts!("tab.split_below"), Axis::Vertical),
            ] {
                if !self.can_split_active(axis, cx) {
                    continue;
                }
                let this = this.clone();
                entries.push(MenuEntry::new(label).on_activate(move |window, cx| {
                    this.update(cx, |workspace, cx| {
                        workspace.merge_tab_into_active(index, axis, window, cx);
                    });
                }));
            }
            if !entries.is_empty() {
                entries.push(MenuEntry::separator());
            }
        }
        entries.push(MenuEntry::new(ts!("tab.close")).on_activate({
            let this = this.clone();
            move |window, cx| {
                this.update(cx, |workspace, cx| workspace.close_tab(index, window, cx));
            }
        }));

        Some(
            ContextMenu::new("tab-context")
                .position(position)
                .entries(entries)
                .on_dismiss(move |_window, cx| {
                    this.update(cx, |workspace, cx| workspace.close_tab_context(cx));
                }),
        )
    }

    /// Renders the panes of the active tab, or the empty state.
    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(tab) = self.tabs.get(self.active) else {
            return self.render_empty_state(cx);
        };

        let theme = theme(cx);
        // A lone terminal with nothing beside it is drawn exactly as it was
        // before panes existed: no frame, no divider, the terminal filling the
        // body. Once it is split, or once the file panel is open next to it,
        // there is a second thing that can hold the keyboard and the frame has
        // to be there to say which one does.
        let frame = tab.panes.leaf_count() > 1 || self.panel_open;
        // Asked of the focus tree at render time for the same reason the panel
        // asks it — see `FilePanel::render`. Only one of the two frames wears
        // the accent, so the active pane gives its own up while the panel has
        // the keyboard.
        let panel_focused =
            self.panel_open && self.panel.focus_handle(cx).contains_focused(window, cx);
        let active = tab.active_pane();
        let root = tab.panes.root();
        let panel = self.panel_open.then(|| self.panel.clone());

        div()
            .flex()
            .flex_row()
            .flex_grow()
            .min_w_0()
            .min_h_0()
            .children(panel)
            .child(div().flex().flex_1().min_w_0().min_h_0().child(render_pane(
                root,
                active,
                frame,
                panel_focused,
                &theme,
                cx,
            )))
            .into_any_element()
    }

    /// Moves the divider of `split` to wherever the pointer has dragged it.
    ///
    /// The share is measured against the split's own box rather than tracked as
    /// a delta, so the divider sits under the pointer however far the gesture
    /// wandered — including outside the window, which a delta would have to
    /// keep integrating. `MIN_SPLIT_RATIO` stops it short of either edge: a
    /// pane squeezed to nothing would take this handle with it and leave no way
    /// to drag it back.
    fn drag_split(
        &mut self,
        split: SplitId,
        axis: Axis,
        event: &DragMoveEvent<DraggedSplit>,
        cx: &mut Context<Self>,
    ) {
        // Enclosing splits see the same moves, so a listener has to check that
        // the divider being dragged is the one it renders.
        if event.drag(cx).split != split {
            return;
        }

        let bounds = event.bounds;
        let position = event.event.position;
        let share = match axis {
            Axis::Horizontal => (position.x - bounds.left()) / bounds.size.width,
            Axis::Vertical => (position.y - bounds.top()) / bounds.size.height,
        };
        // Zero-sized bounds cannot happen in a laid-out frame, but the division
        // above says otherwise; a `NaN` would poison the stored ratio for good.
        if !share.is_finite() {
            return;
        }

        // Looked up now rather than captured at render time: the active tab can
        // change between the frame that drew the handle and this event.
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab
            .panes
            .set_ratio(split, share.clamp(MIN_SPLIT_RATIO, 1. - MIN_SPLIT_RATIO))
        {
            cx.notify();
        }
    }

    /// The tab strip's overlay scroll indicator, as it stands.
    ///
    /// Rebuilt on demand rather than kept, because everything it is made of —
    /// the strip's box, how far it overflows, where it sits — is measured
    /// afresh by gpui on every layout pass.
    fn tab_scrollbar(&self) -> Scrollbar {
        Scrollbar::for_handle(TAB_SCROLLBAR, ScrollbarAxis::Horizontal, &self.tab_scroll)
            .fade(self.tab_scrollbar.fade())
    }

    /// Puts the strip's bar up whenever the strip has moved, and starts the
    /// clock that takes it down again.
    ///
    /// Called from `render` because that is where every way of scrolling the
    /// strip meets: a wheel over the tabs, and the jump that brings a newly
    /// activated tab back into view.
    fn watch_tab_scroll(&mut self, cx: &mut Context<Self>) {
        let scrolled = scrolled(&self.tab_scroll, ScrollbarAxis::Horizontal);
        if let Some(epoch) = self.tab_scrollbar.moved(scrolled) {
            hide_later(epoch, cx, |workspace| Some(&mut workspace.tab_scrollbar));
        }
    }

    /// Scrolls the tab strip to wherever its thumb has been dragged.
    ///
    /// Every element listening for this drag type hears every such drag, so the
    /// bar checks that the one being dragged is its own before answering.
    fn drag_tab_scrollbar(&mut self, event: &DragMoveEvent<DraggedThumb>, cx: &mut Context<Self>) {
        let Some(progress) = self.tab_scrollbar().dragged(event, cx) else {
            return;
        };

        // Held even when the pointer moved along the other axis and the strip
        // has not budged: the bar has to stay up for as long as it is being
        // held, and a still pointer moves nothing to notice.
        self.tab_scrollbar.hold();
        scroll_to(&self.tab_scroll, ScrollbarAxis::Horizontal, progress);
        cx.notify();
    }

    /// Lets go of the strip's thumb, and starts the clock on the bar again.
    ///
    /// Every mouse release in the window arrives here; all but the one ending a
    /// drag of this bar find nothing to let go of.
    fn release_tab_scrollbar(&mut self, cx: &mut Context<Self>) {
        if let Some(epoch) = self.tab_scrollbar.release() {
            hide_later(epoch, cx, |workspace| Some(&mut workspace.tab_scrollbar));
            cx.notify();
        }
    }

    /// Renders the placeholder shown while no session is open.
    fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let this = cx.entity();
        let profiles = self.dialog.read(cx).profiles();

        let saved = (!profiles.is_empty()).then(|| {
            let rows = profiles.into_iter().enumerate().map(|(index, profile)| {
                let this = this.clone();
                let id = ElementId::from(("saved-profile", index));
                let label = format!("{}  ·  {}", profile.name, profile.label());
                Button::new(id, label)
                    .variant(ButtonVariant::Ghost)
                    .full_width(true)
                    .on_click(move |_, _window, cx| {
                        this.update(cx, |workspace, cx| workspace.open_profile(&profile, cx));
                    })
            });

            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .w(px(320.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(ts!("empty.saved_profiles")),
                )
                .children(rows)
        });

        let local = self.render_empty_local(cx);
        let shortcut = ts!("empty.hint", shortcut = format!("{SHORTCUT_MODIFIER}+T"));

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(px(14.))
            // The only fill covering the body while no session is open, so this
            // is where the window opacity lands on the empty state.
            .bg(app_settings::window_tint(theme.background, cx))
            .child(
                div()
                    .text_size(px(30.))
                    .text_color(theme.text)
                    .child("logman"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(shortcut),
            )
            .child(
                div().w(px(320.)).child(
                    Button::new("empty-new-session", ts!("menu.new_session"))
                        .full_width(true)
                        .on_click({
                            let this = this.clone();
                            move |_, _window, cx| {
                                this.update(cx, |workspace, cx| workspace.open_dialog(cx));
                            }
                        }),
                ),
            )
            .children(local)
            .children(saved)
            .into_any_element()
    }

    /// The empty state's local terminal button, or nothing at all on a platform
    /// without one.
    ///
    /// Sits between the button that opens the connection dialog and the saved
    /// profiles, and unlike either of them it opens a session outright rather
    /// than a dialog: a local shell has no host, no credentials and nothing to
    /// save, so there is nothing for a dialog to ask. The shell's name rides
    /// along after a separator, exactly as a profile row carries its
    /// `user@host`, so the button says which shell the press will start.
    fn render_empty_local(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        #[cfg(not(unix))]
        {
            let _ = cx;
            None
        }

        #[cfg(unix)]
        {
            let this = cx.entity();
            let label = format!(
                "{}  ·  {}",
                ts!("connection.local.name"),
                logman_pty::login_shell_name()
            );
            Some(
                div()
                    .w(px(320.))
                    .child(
                        Button::new("empty-local-session", label)
                            .variant(ButtonVariant::Secondary)
                            .full_width(true)
                            .on_click(move |_, window, cx| {
                                this.update(cx, |workspace, cx| {
                                    workspace.open_local_session(window, cx)
                                });
                            }),
                    )
                    .into_any_element(),
            )
        }
    }

    /// Renders the bottom status bar.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let (target, status, grid): (SharedString, SharedString, SharedString) =
            match self.tabs.get(self.active) {
                // The active pane, not the tab: on a split tab the bar reports
                // the session the keyboard is aimed at.
                Some(tab) => {
                    let session = tab.active_view().read(cx).session().read(cx);
                    let (cols, rows) = session.terminal().size();
                    (
                        session.label(),
                        session.status().summary(),
                        format!("{cols}x{rows}").into(),
                    )
                }
                None => (
                    ts!("statusbar.no_session"),
                    ts!("statusbar.idle"),
                    // A dash standing in for the grid size: punctuation, not a
                    // word, so it is the same in every language.
                    SharedString::new_static("-"),
                ),
            };

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .h(px(24.))
            .px(px(10.))
            // The bar is inert, so a press on it must not move the keyboard.
            // Without this the workspace root's `track_focus` would claim the
            // click, and the accent frame would jump to the active pane even
            // though no pane received focus.
            .on_any_mouse_down(|_, window, _cx| window.prevent_default())
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(11.))
            .text_color(theme.text_muted)
            .child(div().flex_none().whitespace_nowrap().child(target))
            // The status summary carries the failure reason, which can be far
            // wider than the window; letting it shrink and ellipsize keeps the
            // grid size pinned to the right edge instead of pushing it out.
            .child(div().flex_1().min_w_0().truncate().child(status))
            .child(div().flex_none().whitespace_nowrap().child(grid))
            .into_any_element()
    }
}

/// Renders one node of a pane tree.
///
/// A split becomes a flex box in the direction of its axis, with each child
/// sized by `flex_basis`; the `min_w_0` / `min_h_0` on the box *and* on both
/// children is what lets those bases actually divide the space, instead of the
/// terminals inside insisting on their measured width. The pty follows on its
/// own: [`TerminalView`]'s element recomputes the grid from whatever bounds it
/// is given and only pushes a resize when the cell count changed.
///
/// A leaf renders the terminal view itself. When `frame` is set — a split tab,
/// or a single pane with the file panel beside it — every leaf is framed with a
/// hairline, accent coloured on the active one. The frames double as the
/// divider between neighbours, which is why there is no separate divider
/// element — a third hairline squeezed between two of them would only thicken
/// the seam. Every pane is framed, not just the active one, so that moving
/// focus recolours the frame without shifting the layout by a pixel. It is a
/// border rather than a fill because a translucent window allows only one
/// tinted fill per pixel and the terminal surface already owns it.
///
/// `panel_focused` demotes the active leaf back to the plain border colour: the
/// file panel wears the accent frame while it holds the keyboard, and two
/// accent frames at once would say the keystroke is going to both places.
///
/// A split also lays an invisible handle over its divider, last so that it wins
/// the hit test against the panes it straddles, and positioned absolutely so
/// that it can straddle them at all: an in-flow handle would have to be given
/// room, which is exactly what the hairline seam is meant not to need.
fn render_pane(
    node: &PaneNode<PaneLeaf>,
    active: PaneId,
    frame: bool,
    panel_focused: bool,
    theme: &Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    match node {
        PaneNode::Leaf { id, payload } => {
            let border = if *id == active && !panel_focused {
                theme.accent
            } else {
                theme.border
            };
            div()
                .id(("pane", id.as_u64()))
                .flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .when(frame, |pane| pane.border_1().border_color(border))
                .child(payload.view.clone())
                .into_any_element()
        }
        PaneNode::Split {
            id,
            axis,
            ratio,
            first,
            second,
        } => {
            let id = *id;
            let axis = *axis;
            let ratio = ratio.clamp(MIN_SPLIT_RATIO, 1. - MIN_SPLIT_RATIO);
            // Both children are rendered up front because each one needs `cx`
            // for the handles further down the tree, and a closure holding it
            // could not then be called twice.
            let first = render_pane(first, active, frame, panel_focused, theme, cx);
            let second = render_pane(second, active, frame, panel_focused, theme, cx);
            let half = |share: f32, child: AnyElement| {
                div()
                    .flex()
                    .flex_basis(relative(share))
                    .min_w_0()
                    .min_h_0()
                    .child(child)
            };
            // Centred on the seam by pulling it back half its own thickness,
            // so the grab area is symmetric about the line the user sees.
            let offset = px(-SPLIT_HANDLE / 2.);
            let handle = div()
                .id(("split-handle", id.as_u64()))
                .absolute()
                // A plain hitbox does not stop events reaching what is under
                // it, and under this one are two terminals that would take the
                // press as the start of a text selection.
                .occlude()
                .map(|handle| match axis {
                    Axis::Horizontal => handle
                        .top_0()
                        .bottom_0()
                        .left(relative(ratio))
                        .ml(offset)
                        .w(px(SPLIT_HANDLE))
                        .cursor_ew_resize(),
                    Axis::Vertical => handle
                        .left_0()
                        .right_0()
                        .top(relative(ratio))
                        .mt(offset)
                        .h(px(SPLIT_HANDLE))
                        .cursor_ns_resize(),
                })
                // An empty preview: the divider follows the pointer directly,
                // so a ghost trailing it would only be a second thing to watch.
                .on_drag(DraggedSplit { split: id }, |_, _, _, cx| {
                    cx.new(|_| gpui::Empty)
                });

            div()
                .flex()
                .map(|container| match axis {
                    Axis::Horizontal => container.flex_row(),
                    Axis::Vertical => container.flex_col(),
                })
                .size_full()
                .min_w_0()
                .min_h_0()
                // Listening here rather than on the handle because the handle
                // moves out from under the pointer as the drag goes on, while
                // this box stays put and is what the new ratio is measured
                // against.
                .on_drag_move::<DraggedSplit>(cx.listener(
                    move |workspace, event: &DragMoveEvent<DraggedSplit>, _window, cx| {
                        workspace.drag_split(id, axis, event, cx);
                    },
                ))
                .child(half(ratio, first))
                .child(half(1. - ratio, second))
                .child(handle)
                .into_any_element()
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        // Before anything is built, so the panel is already pointed at the
        // active pane's session by the time it renders itself as a child.
        self.sync_file_panel(cx);
        self.watch_tab_scroll(cx);
        let toolbar = self.render_toolbar(window, cx);
        let body = self.render_body(window, cx);
        let status_bar = self.render_status_bar(cx);
        let tab_context = self.render_tab_context(cx);
        let dialog = self
            .dialog
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.dialog.clone()));
        let settings = self
            .settings
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.settings.clone()));
        let about = self
            .about
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.about.clone()));

        // With client-side decorations the compositor stops drawing the drop
        // shadow along with the frame, so the window has to bring its own:
        // the surface grows a transparent band all round, the content is
        // inset by it, and the shadow is painted into it. The inset call
        // keeps `_GTK_FRAME_EXTENTS` in step so the compositor treats the
        // content edge, not the surface edge, as the window.
        let tiling = client_tiling(window);
        if tiling.is_some() {
            window.set_client_inset(px(SHADOW_BAND));
        } else {
            // Clears the extents a client-side frame may have left behind
            // when the setting switches back to the system title bar on a
            // live window; a no-op under decorations that never set any.
            window.set_client_inset(px(0.));
        }

        // No background fill here on purpose. The three bands below — toolbar,
        // body and status bar — cover the window between them, and each paints
        // its own. A fill at this level would sit *under* the translucent
        // terminal and empty-state fills and compose back to opaque, which is
        // exactly what made `window.background_opacity` and `background_blur`
        // look like they did nothing.
        let content = div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            .text_size(px(13.))
            // The tab strip's overlay bar is answered from here rather than
            // from the strip: gpui hands a drag move to every listener of that
            // type wherever it sits, and the root is the one element that is
            // always mounted while a drag of it is in flight.
            .on_drag_move::<DraggedThumb>(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedThumb>, _window, cx| {
                    workspace.drag_tab_scrollbar(event, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_tab_scrollbar(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _: &MouseUpEvent, _window, cx| {
                    workspace.release_tab_scrollbar(cx);
                }),
            )
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::close_session_action))
            .on_action(cx.listener(Self::focus_next_pane_action))
            .on_action(cx.listener(Self::focus_prev_pane_action))
            .on_action(cx.listener(Self::break_out_pane_action))
            .on_action(cx.listener(Self::duplicate_split_right_action))
            .on_action(cx.listener(Self::duplicate_split_below_action))
            .on_action(cx.listener(Self::toggle_file_panel_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::show_about_action))
            .on_action(cx.listener(Self::select_tab_action))
            .on_action(cx.listener(Self::dismiss_dialog_action))
            .child(toolbar)
            .child(body)
            .child(status_bar)
            // Deferred inside, so it paints above the three bands whatever its
            // place in this list.
            .children(tab_context)
            .children(dialog)
            .children(settings)
            .children(about);

        let Some(tiling) = tiling else {
            // A server-decorated window: the compositor frames and shadows
            // it, and the content is the whole surface.
            return content.into_any_element();
        };

        div()
            .size_full()
            .relative()
            .bg(gpui::transparent_black())
            .when(!tiling.top, |outer| outer.pt(px(SHADOW_BAND)))
            .when(!tiling.bottom, |outer| outer.pb(px(SHADOW_BAND)))
            .when(!tiling.left, |outer| outer.pl(px(SHADOW_BAND)))
            .when(!tiling.right, |outer| outer.pr(px(SHADOW_BAND)))
            .child(
                content
                    // A hairline where the frame's own outline used to be,
                    // per untiled edge; a tiled edge meets the neighbour
                    // flush, the way the compositor would have drawn it.
                    .border_color(theme.border)
                    .when(!tiling.top, |content| content.border_t_1())
                    .when(!tiling.bottom, |content| content.border_b_1())
                    .when(!tiling.left, |content| content.border_l_1())
                    .when(!tiling.right, |content| content.border_r_1())
                    .when(!tiling.is_tiled(), |content| {
                        content.shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(0., 0., 0., 0.35),
                            blur_radius: px(SHADOW_BAND / 2.),
                            spread_radius: px(0.),
                            offset: gpui::point(px(0.), px(2.)),
                        }])
                    }),
            )
            // Last on purpose: the window border outranks whatever it
            // crosses, dialogs included, the way a compositor frame would.
            .children(render_resize_edges(tiling))
            .into_any_element()
    }
}

/// Installs the widget theme the configured id names.
///
/// An id nothing answers to — a theme file the user has since deleted — falls
/// back to the default theme rather than failing; see
/// [`ThemeRegistry::resolve`].
fn apply_ui_theme(id: &str, cx: &mut App) {
    let theme = ThemeRegistry::resolve(id, cx);
    set_theme(theme, cx);
}

/// Whether the toolbar has to stand in for the window's title bar.
///
/// On Windows and macOS the style applied to the window settles it: a
/// transparent title bar leaves no platform caption, so the toolbar is all
/// there is.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn draws_own_titlebar(style: TitlebarStyle, _window: &Window) -> bool {
    style == TitlebarStyle::Custom
}

/// Whether the toolbar has to stand in for the window's title bar.
///
/// Linux is not the configured style alone. The custom style makes the window
/// ask for client-side decorations, but the ask can be declined — gpui falls
/// back to server decorations when no compositor is running — so what the
/// window actually ended up with is what decides here. Deciding from the
/// style alone would draw a second caption under the compositor's own.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn draws_own_titlebar(style: TitlebarStyle, window: &Window) -> bool {
    style == TitlebarStyle::Custom
        && matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        )
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// Windows needs none of them. The row reports itself as
/// [`WindowControlArea::Drag`], the hit test turns that into `HTCAPTION`, and
/// the window procedure then does the dragging, the aero-snap gestures and the
/// double-click to maximise on its own — before the app is ever told a button
/// went down.
#[cfg(target_os = "windows")]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    row
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// AppKit still drags the window for the strip its own title bar would have
/// covered, so only the double-click is left to answer — and it has to go
/// through [`Window::titlebar_double_click`], which follows whatever the user
/// picked in System Settings (zoom, minimise, or nothing at all).
#[cfg(target_os = "macos")]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    row.on_click(|event, window, _cx| {
        if event.standard_click() && event.click_count() == 2 {
            window.titlebar_double_click();
        }
    })
}

/// Wires the gestures a system title bar answers to onto the custom one.
///
/// Everything is the app's here: the compositor is told to take over the move,
/// and the window menu and the zoom have to be asked for explicitly. Only
/// meaningful once the window carries client-side decorations, which is why
/// the caller gates them on [`Window::window_decorations`].
///
/// The move starts on the press rather than the click because the compositor
/// takes the pointer with it, so a release would never arrive.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn titlebar_gestures(row: Stateful<Div>) -> Stateful<Div> {
    use gpui::MouseButton;

    row.on_click(|event, window, _cx| {
        if event.standard_click() && event.click_count() == 2 {
            window.zoom_window();
        }
    })
    .on_mouse_down(MouseButton::Left, |_, window, _cx| {
        window.start_window_move();
    })
    .on_mouse_down(MouseButton::Right, |event, window, _cx| {
        window.show_window_menu(event.position);
    })
}

/// Width of the transparent band around a self-decorated window.
///
/// The band carries the drop shadow the compositor no longer draws once the
/// window asks for client-side decorations, and doubles as the resize grip.
/// It is part of the window's surface but not of the window as the user
/// understands it: [`Window::set_client_inset`] publishes the visible bounds
/// through `_GTK_FRAME_EXTENTS`, so the compositor snaps, maximises and
/// stacks by the visible edge, exactly as it does for GTK's frames.
const SHADOW_BAND: f32 = 12.;

/// Edge length of the corner squares, where the resize goes diagonal.
const RESIZE_CORNER: f32 = 24.;

/// The tiling state of a window that draws its own frame, `None` under a
/// server-side one.
///
/// Always `None` here: Windows keeps resizing and framing the window through
/// the caption hit test even under a custom title bar, and AppKit never gives
/// the frame up at all — neither window ever carries the shadow band.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn client_tiling(_window: &Window) -> Option<gpui::Tiling> {
    None
}

/// The tiling state of a window that draws its own frame, `None` under a
/// server-side one.
///
/// `Some` exactly when the compositor granted client-side decorations, with
/// the edges that currently touch a screen or neighbour edge marked tiled —
/// those edges get no band, no shadow and no resize grip. Fullscreen counts
/// as tiled all round.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn client_tiling(window: &Window) -> Option<gpui::Tiling> {
    match window.window_decorations() {
        gpui::Decorations::Client { tiling } => Some(tiling),
        gpui::Decorations::Server => None,
    }
}

/// The resize handles the compositor's frame would have provided.
///
/// Asking for client-side decorations takes the frame away, resize borders
/// included, so the shadow band has to start the resize itself — the
/// compositor takes over once told, exactly as it does for the title-bar
/// drag. The strips cover the band, the corner squares reach past it into
/// the window, and every tiled edge goes without: a maximised or snapped
/// window has no border to drag there.
fn render_resize_edges(tiling: gpui::Tiling) -> Vec<AnyElement> {
    use gpui::{CursorStyle, ResizeEdge};

    let strip = px(SHADOW_BAND);
    let corner = px(RESIZE_CORNER);
    // A strip stops short of a corner square only where that square exists;
    // against a tiled perpendicular edge it runs to the end of the band.
    let inset = |tiled: bool| if tiled { px(0.) } else { corner };
    let handle = |id: &'static str, cursor: CursorStyle, edge: ResizeEdge| {
        div()
            .id(id)
            .occlude()
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, _cx| {
                window.start_window_resize(edge);
            })
    };

    let mut handles: Vec<AnyElement> = Vec::new();
    if !tiling.top {
        handles.push(
            handle("resize-top", CursorStyle::ResizeUpDown, ResizeEdge::Top)
                .top_0()
                .left(inset(tiling.left))
                .right(inset(tiling.right))
                .h(strip)
                .into_any_element(),
        );
    }
    if !tiling.bottom {
        handles.push(
            handle(
                "resize-bottom",
                CursorStyle::ResizeUpDown,
                ResizeEdge::Bottom,
            )
            .bottom_0()
            .left(inset(tiling.left))
            .right(inset(tiling.right))
            .h(strip)
            .into_any_element(),
        );
    }
    if !tiling.left {
        handles.push(
            handle(
                "resize-left",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Left,
            )
            .left_0()
            .top(inset(tiling.top))
            .bottom(inset(tiling.bottom))
            .w(strip)
            .into_any_element(),
        );
    }
    if !tiling.right {
        handles.push(
            handle(
                "resize-right",
                CursorStyle::ResizeLeftRight,
                ResizeEdge::Right,
            )
            .right_0()
            .top(inset(tiling.top))
            .bottom(inset(tiling.bottom))
            .w(strip)
            .into_any_element(),
        );
    }
    if !tiling.top && !tiling.left {
        handles.push(
            handle(
                "resize-top-left",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::TopLeft,
            )
            .top_0()
            .left_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.top && !tiling.right {
        handles.push(
            handle(
                "resize-top-right",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::TopRight,
            )
            .top_0()
            .right_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.bottom && !tiling.left {
        handles.push(
            handle(
                "resize-bottom-left",
                CursorStyle::ResizeUpRightDownLeft,
                ResizeEdge::BottomLeft,
            )
            .bottom_0()
            .left_0()
            .size(corner)
            .into_any_element(),
        );
    }
    if !tiling.bottom && !tiling.right {
        handles.push(
            handle(
                "resize-bottom-right",
                CursorStyle::ResizeUpLeftDownRight,
                ResizeEdge::BottomRight,
            )
            .bottom_0()
            .right_0()
            .size(corner)
            .into_any_element(),
        );
    }
    handles
}

/// Maps the window settings onto a gpui background appearance.
///
/// Blur wins when requested; failing that, any opacity below fully opaque asks
/// for a plain transparent window; otherwise the window stays opaque.
fn window_appearance(window: &WindowSettings) -> WindowBackgroundAppearance {
    if window.background_blur {
        WindowBackgroundAppearance::Blurred
    } else if window.background_opacity < 1.0 {
        WindowBackgroundAppearance::Transparent
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// The application menu bar, in macOS layout.
///
/// gpui only turns this into a real menu bar on macOS — the Windows and Linux
/// backends store it and draw nothing — so the other platforms get the same
/// commands from the in-app dropdown built by
/// [`Workspace::render_app_menu`]. Every item dispatches an action that is also
/// bound to a shortcut in [`bind_shortcuts`], which is what lets the macOS
/// backend label the items with their key equivalents; register the bindings
/// first so the keymap it reads is already populated.
///
/// About, Settings and Quit live in the application menu because that is where
/// macOS users look for them.
///
/// The item labels are translated, but the application menu's own name is the
/// "logman" wordmark and stays as it is. Rebuilt and re-installed whenever the
/// language changes, because gpui takes the menu bar by value.
fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "logman".into(),
            items: vec![
                MenuItem::action(ts!("menu.about"), ShowAbout),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.settings"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(ts!("menu.mac.quit"), Quit),
            ],
        },
        Menu {
            name: ts!("menu.session"),
            items: vec![
                MenuItem::action(ts!("menu.mac.new_session"), NewSession),
                MenuItem::action(ts!("menu.mac.close_session"), CloseSession),
                // Only half of splitting is here, for the reason given on
                // [`Workspace::render_app_menu`]: a merge has to name a source
                // tab, so it belongs to the tab context menu alone.
                MenuItem::action(ts!("menu.mac.duplicate_right"), DuplicateSplitRight),
                MenuItem::action(ts!("menu.mac.duplicate_below"), DuplicateSplitBelow),
                MenuItem::action(ts!("menu.mac.break_out_pane"), BreakOutPane),
                MenuItem::separator(),
                MenuItem::action(ts!("files.mac.toggle"), ToggleFilePanel),
            ],
        },
    ]
}

/// Registers every shortcut the workspace listens for.
///
/// A binding here beats the terminal: gpui matches key bindings along the whole
/// dispatch path before it delivers the key event itself, so every chord bound
/// in this function is taken away from the remote shell. That is what decides
/// the pane modifier below.
fn bind_shortcuts(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    // Pane navigation follows iTerm2 on macOS, where `cmd` never reaches the
    // shell. Elsewhere the same chords would swallow `Ctrl+[` — which every
    // remote shell reads as ESC — and `Ctrl+]`, so those platforms use `alt`
    // instead, the modifier Windows Terminal also keeps for pane navigation.
    // The bracket keys stay unshifted on purpose: both macOS and Windows report
    // a shifted bracket as `}` with the shift flag already consumed, so a
    // `shift-]` binding would never match. Hence a letter for the break-out.
    let pane_modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "alt"
    };

    let mut bindings = vec![
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-t"), NewSession, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-w"), CloseSession, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", DismissDialog, Some(KEY_CONTEXT)),
        KeyBinding::new(
            &format!("{pane_modifier}-]"),
            FocusNextPane,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{pane_modifier}-["),
            FocusPrevPane,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{pane_modifier}-shift-b"),
            BreakOutPane,
            Some(KEY_CONTEXT),
        ),
        // Shifted for the same reason the break-out is: off macOS the pane
        // modifier is `alt`, and bare `Alt+D` is readline's *kill-word*, which
        // a user typing in the pane being split would miss immediately. The
        // shifted chord is free in a way the bare one is not — a terminal
        // cannot encode `Alt+Shift+D` distinctly from `Alt+D` — so taking it
        // costs the remote shell nothing. `Alt+S` is shifted to match, since
        // the two split directions have to read as one pair of commands.
        KeyBinding::new(
            &format!("{pane_modifier}-shift-d"),
            DuplicateSplitRight,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{pane_modifier}-shift-s"),
            DuplicateSplitBelow,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(PANEL_SHORTCUT, ToggleFilePanel, Some(KEY_CONTEXT)),
    ];
    for index in 0..QUICK_SELECT_TABS {
        bindings.push(KeyBinding::new(
            &format!("{modifier}-{}", index + 1),
            SelectTab(index),
            Some(KEY_CONTEXT),
        ));
    }

    cx.bind_keys(bindings);
}

fn main() {
    env_logger::init();

    // The icon set has to be installed before the app runs: `svg()` resolves
    // every path through this source, and the default one answers `None`.
    Application::new().with_assets(Icons).run(|cx: &mut App| {
        if let Err(error) = logman_core::init_secrets() {
            log::warn!("the OS keychain is unavailable: {error}");
        }

        // Load settings before the widget layer installs its default theme, then
        // override that theme to match what the user configured.
        app_settings::init(cx);
        let settings = app_settings::current(cx);
        // Ahead of everything that renders a string — the menu bar included —
        // so nothing is ever built in the wrong language and then corrected.
        i18n::apply(settings.language.as_deref());

        ui::init(cx);
        TerminalView::init(cx);
        bind_shortcuts(cx);
        cx.set_menus(app_menus());

        // Before the theme is applied: the id in the settings may well name one
        // of the user's own themes, and the same goes for the scheme every
        // session is about to be opened with.
        theme_store::reload(cx);
        apply_ui_theme(&settings.ui_theme, cx);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        // Read once, here: `appears_transparent` is what strips the platform
        // caption, and both Windows and macOS decide that when the window is
        // created. Changing the setting later cannot reach an open window,
        // which is why the settings dialog says a restart is needed.
        let titlebar = settings.window.titlebar;
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("logman".into()),
                    appears_transparent: titlebar == TitlebarStyle::Custom,
                    // Ignored unless the caption is transparent; it moves the
                    // traffic lights AppKit keeps drawing into the toolbar
                    // band the app puts in the caption's place.
                    traffic_light_position: (titlebar == TitlebarStyle::Custom)
                        .then_some(TRAFFIC_LIGHT_ORIGIN),
                }),
                // Only the Linux backends read this. `appears_transparent`
                // above means nothing to X11 and Wayland: the caption stays
                // the compositor's until the window asks for client-side
                // decorations outright. gpui falls back to server decorations
                // on its own when no compositor is present, and
                // [`draws_own_titlebar`] follows what the window actually got.
                window_decorations: (titlebar == TitlebarStyle::Custom)
                    .then_some(gpui::WindowDecorations::Client),
                // Wayland compositors and X11 docks match this against
                // com.aihouse.logman.desktop to pick up the application icon.
                app_id: Some("com.aihouse.logman".into()),
                // A translucent or blurred window needs the platform surface to
                // permit alpha; the terminal view then tints its background.
                window_background: window_appearance(&settings.window),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(titlebar, window, cx));
                window.focus(&workspace.read(cx).focus_handle);
                apply_caption_theme(window, &theme(cx));
                workspace
            },
        )
        .expect("failed to open the logman window");

        cx.activate(true);
    });
}
