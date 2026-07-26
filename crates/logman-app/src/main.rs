//! logman — a multi-platform GUI SSH terminal.
//!
//! The binary owns the application shell: a tab strip of open [`Session`]s, the
//! terminal surface of the active one, a status bar, and the connection dialog
//! rendered on top of everything else. Session state lives in [`session`], the
//! terminal surface in [`terminal_view`], and every reusable widget in [`ui`].

mod connection;
mod session;
mod terminal_view;
// The widget layer is written as a self-contained toolkit rather than for one
// call site, so it deliberately offers variants no current call site uses (the
// light theme, disabled inputs, the danger button). Inside a binary crate those
// read as dead code, hence the module-wide allow.
#[allow(dead_code)]
mod ui;
mod verifier;

use gpui::{
    AnyElement, App, Application, Bounds, Context, ElementId, Entity, FocusHandle, Focusable,
    KeyBinding, SharedString, Subscription, TitlebarOptions, Window, WindowBounds, WindowOptions,
    actions, div, prelude::*, px, size,
};
use logman_core::SessionProfile;
use logman_ssh::SshAuth;

use connection::{ConnectionDialog, ConnectionDialogEvent};
use session::Session;
use terminal_view::TerminalView;
use ui::{Button, ButtonVariant, TabBar, TabItem, theme};

actions!(
    logman,
    [
        /// Quit the application.
        Quit,
        /// Open the connection dialog with an empty form.
        NewSession,
        /// Close the active session tab.
        CloseSession,
        /// Close the connection dialog, if it is open.
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

/// One open session together with the view rendering it.
struct SessionTab {
    /// The terminal surface; it owns the [`Session`] entity.
    view: Entity<TerminalView>,
    /// Repaints the workspace when the session's title or status changes.
    _observer: Subscription,
}

impl SessionTab {
    /// The session rendered by this tab.
    fn session(&self, cx: &App) -> Entity<Session> {
        self.view.read(cx).session().clone()
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
    /// The connection dialog, rendered only while it reports itself open.
    dialog: Entity<ConnectionDialog>,
    /// Keeps the dialog subscription alive.
    _dialog_events: Subscription,
    /// Disconnects every session before the process exits.
    _quit: Subscription,
}

impl Workspace {
    /// Builds an empty workspace and wires up the connection dialog.
    fn new(window: &Window, cx: &mut Context<Self>) -> Self {
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
                    ConnectionDialogEvent::Dismissed => {
                        dialog.update(cx, |dialog, cx| dialog.close(cx));
                        this.focus_active(window, cx);
                    }
                },
            );

        let quit = cx.on_app_quit(|this, cx| {
            for tab in &this.tabs {
                tab.session(cx)
                    .update(cx, |session, cx| session.disconnect(cx));
            }
            async {}
        });

        Self {
            focus_handle: cx.focus_handle(),
            tabs: Vec::new(),
            active: 0,
            dialog,
            _dialog_events: dialog_events,
            _quit: quit,
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
        let view = cx.new(|cx| TerminalView::new(session.clone(), window, cx));
        let observer = cx.observe(&session, |_, _, cx| cx.notify());

        self.tabs.push(SessionTab {
            view,
            _observer: observer,
        });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Activates the tab at `index`, if it exists.
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.active = index;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Disconnects and removes the tab at `index`.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        let tab = self.tabs.remove(index);
        tab.session(cx)
            .update(cx, |session, cx| session.disconnect(cx));

        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Moves keyboard focus onto the active terminal, or onto the workspace
    /// itself when no session is open.
    ///
    /// Without this the shortcuts stop working after the last tab is closed,
    /// because their key context only exists while something inside the
    /// workspace is focused.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(tab) => {
                let handle = tab.view.read(cx).focus_handle(cx);
                window.focus(&handle);
            }
            None => window.focus(&self.focus_handle),
        }
    }

    /// Shows the connection dialog with an empty form.
    fn open_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog.update(cx, |dialog, cx| dialog.open_new(cx));
        cx.notify();
    }

    /// Shows the connection dialog pre-filled from a saved profile.
    fn open_profile(&mut self, profile: &SessionProfile, cx: &mut Context<Self>) {
        let id = profile.id;
        self.dialog
            .update(cx, |dialog, cx| dialog.open_profile(id, cx));
        cx.notify();
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + <kbd>T</kbd>.
    fn new_session_action(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(cx);
    }

    /// Handles <kbd>Ctrl</kbd>/<kbd>Cmd</kbd> + <kbd>W</kbd>.
    fn close_session_action(
        &mut self,
        _: &CloseSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        self.close_tab(active, window, cx);
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

    /// Handles <kbd>Esc</kbd>: closes the dialog, or lets the key through to
    /// the terminal when no dialog is open.
    fn dismiss_dialog_action(
        &mut self,
        _: &DismissDialog,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.dialog.read(cx).is_open() {
            cx.propagate();
            return;
        }
        // Route through `dismiss` rather than closing directly: the dialog also
        // binds `Escape` internally, and going through one path keeps
        // `Dismissed` firing exactly once no matter which handler wins the
        // dispatch. Closing and restoring focus is then the subscription's job.
        self.dialog.update(cx, |dialog, cx| dialog.dismiss(cx));
        cx.notify();
    }

    /// Renders the tab strip.
    fn render_tab_bar(&self, cx: &mut Context<Self>) -> TabBar {
        let this = cx.entity();
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let session = tab.view.read(cx).session().read(cx);
                TabItem::new(("session-tab", index), session.title()).status(session.tab_status())
            })
            .collect();

        TabBar::new("session-tabs")
            .tabs(tabs)
            .active(self.active)
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
            .on_new(move |_window, cx| {
                this.update(cx, |workspace, cx| workspace.open_dialog(cx));
            })
    }

    /// Renders the active terminal, or the empty state.
    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.tabs.get(self.active) {
            Some(tab) => div()
                .flex()
                .flex_grow()
                .min_h_0()
                .child(tab.view.clone())
                .into_any_element(),
            None => self.render_empty_state(cx),
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
                        .child("Saved profiles"),
                )
                .children(rows)
        });

        let shortcut = if cfg!(target_os = "macos") {
            "Press Cmd+T to connect to a host."
        } else {
            "Press Ctrl+T to connect to a host."
        };

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(px(14.))
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
                    Button::new("empty-new-session", "New session")
                        .full_width(true)
                        .on_click({
                            let this = this.clone();
                            move |_, _window, cx| {
                                this.update(cx, |workspace, cx| workspace.open_dialog(cx));
                            }
                        }),
                ),
            )
            .children(saved)
            .into_any_element()
    }

    /// Renders the bottom status bar.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let (target, status, grid) = match self.tabs.get(self.active) {
            Some(tab) => {
                let session = tab.view.read(cx).session().read(cx);
                let (cols, rows) = session.terminal().size();
                (
                    session.profile().label(),
                    session.status().summary(),
                    format!("{cols}x{rows}"),
                )
            }
            None => ("no session".to_owned(), "idle".to_owned(), "-".to_owned()),
        };

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .h(px(24.))
            .px(px(10.))
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(11.))
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(SharedString::from(target)),
            )
            // The status summary carries the failure reason, which can be far
            // wider than the window; letting it shrink and ellipsize keeps the
            // grid size pinned to the right edge instead of pushing it out.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(status)),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child(SharedString::from(grid)),
            )
            .into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        let tab_bar = self.render_tab_bar(cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let dialog = self
            .dialog
            .read(cx)
            .is_open()
            .then(|| div().absolute().inset_0().child(self.dialog.clone()));

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .text_size(px(13.))
            .on_action(cx.listener(Self::new_session_action))
            .on_action(cx.listener(Self::close_session_action))
            .on_action(cx.listener(Self::select_tab_action))
            .on_action(cx.listener(Self::dismiss_dialog_action))
            .child(tab_bar)
            .child(body)
            .child(status_bar)
            .children(dialog)
    }
}

/// Registers every shortcut the workspace listens for.
fn bind_shortcuts(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    let mut bindings = vec![
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-t"), NewSession, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-w"), CloseSession, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", DismissDialog, Some(KEY_CONTEXT)),
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

    Application::new().run(|cx: &mut App| {
        if let Err(error) = logman_core::init_secrets() {
            log::warn!("the OS keychain is unavailable: {error}");
        }

        ui::init(cx);
        TerminalView::init(cx);
        bind_shortcuts(cx);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(700.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("logman".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                window.focus(&workspace.read(cx).focus_handle);
                workspace
            },
        )
        .expect("failed to open the logman window");

        cx.activate(true);
    });
}
