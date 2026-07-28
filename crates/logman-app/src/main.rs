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

mod about_dialog;
mod app_settings;
mod connection;
mod session;
mod settings_dialog;
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
    KeyBinding, Menu, MenuItem, SharedString, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use logman_core::{SessionProfile, UiTheme, WindowSettings};
use logman_ssh::SshAuth;

use about_dialog::{AboutDialog, AboutDialogEvent};
use connection::{ConnectionDialog, ConnectionDialogEvent};
use session::Session;
use settings_dialog::{SettingsDialog, SettingsDialogEvent};
use terminal_view::TerminalView;
use ui::{Button, ButtonVariant, MenuButton, MenuEntry, TabBar, TabItem, Theme, set_theme, theme};

actions!(
    logman,
    [
        /// Quit the application.
        Quit,
        /// Open the connection dialog with an empty form.
        NewSession,
        /// Close the active session tab.
        CloseSession,
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

/// Modifier key named in the dropdown menu's shortcut hints.
///
/// The dropdown is only drawn where gpui builds no native menu bar, but the
/// hints follow [`bind_shortcuts`] on every platform so the two never drift.
const SHORTCUT_MODIFIER: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

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
    /// The settings dialog, rendered only while it reports itself open.
    settings: Entity<SettingsDialog>,
    /// The about dialog, rendered only while it reports itself open.
    about: Entity<AboutDialog>,
    /// Whether the application dropdown menu is showing.
    menu_open: bool,
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

        let settings = cx.new(SettingsDialog::new);
        let settings_events = cx.subscribe_in(
            &settings,
            window,
            |this, dialog, event, window, cx| match event {
                // The dialog has already replaced and persisted the settings
                // global by the time it emits this; the shell re-applies the
                // parts that touch live windows and sessions.
                SettingsDialogEvent::Applied => {
                    let settings = app_settings::current(cx);
                    apply_ui_theme(settings.ui_theme, cx);
                    cx.refresh_windows();
                    window.set_background_appearance(window_appearance(&settings.window));
                    for tab in &this.tabs {
                        tab.session(cx)
                            .update(cx, |session, cx| session.apply_settings(cx));
                    }
                }
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
            settings,
            about,
            menu_open: false,
            _dialog_events: dialog_events,
            _settings_events: settings_events,
            _about_events: about_events,
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

    /// Closes every dialog and the dropdown menu.
    ///
    /// Every `open_*` method starts here, which is what keeps the three modals
    /// mutually exclusive: only one of them can ever be on screen, and opening
    /// one always puts the menu away.
    fn close_overlays(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
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

    /// Shows or hides the application dropdown menu.
    fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.menu_open == open {
            return;
        }
        self.menu_open = open;
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
        // The menu paints above everything else, so it is dismissed first.
        if self.menu_open {
            self.set_menu_open(false, cx);
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
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let menu = (!cfg!(target_os = "macos")).then(|| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(TOOLBAR_HEIGHT))
                .px(px(4.))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .child(self.render_app_menu(cx))
        });

        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .w_full()
            .children(menu)
            .child(div().flex_1().min_w_0().child(self.render_tab_bar(cx)))
            .into_any_element()
    }

    /// Builds the dropdown menu shown on the platforms without a native one.
    ///
    /// Every row dispatches the action its keyboard shortcut dispatches, so the
    /// menu adds a way in rather than a second implementation.
    fn render_app_menu(&self, cx: &mut Context<Self>) -> MenuButton {
        let this = cx.entity();
        let entries = vec![
            MenuEntry::new("New session")
                .shortcut(format!("{SHORTCUT_MODIFIER}+T"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(NewSession), cx)),
            MenuEntry::new("Settings\u{2026}")
                .shortcut(format!("{SHORTCUT_MODIFIER}+,"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            MenuEntry::separator(),
            MenuEntry::new("About logman")
                .on_activate(|window, cx| window.dispatch_action(Box::new(ShowAbout), cx)),
            MenuEntry::separator(),
            MenuEntry::new("Quit")
                .shortcut(format!("{SHORTCUT_MODIFIER}+Q"))
                .on_activate(|window, cx| window.dispatch_action(Box::new(Quit), cx)),
        ];

        MenuButton::new("app-menu")
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
        let toolbar = self.render_toolbar(cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
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
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::show_about_action))
            .on_action(cx.listener(Self::select_tab_action))
            .on_action(cx.listener(Self::dismiss_dialog_action))
            .child(toolbar)
            .child(body)
            .child(status_bar)
            .children(dialog)
            .children(settings)
            .children(about)
    }
}

/// Installs the widget theme matching the configured UI theme.
fn apply_ui_theme(ui_theme: UiTheme, cx: &mut App) {
    let theme = match ui_theme {
        UiTheme::Light => Theme::light(),
        UiTheme::Dark => Theme::dark(),
    };
    set_theme(theme, cx);
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
fn app_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "logman".into(),
            items: vec![
                MenuItem::action("About logman", ShowAbout),
                MenuItem::separator(),
                MenuItem::action("Settings\u{2026}", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit logman", Quit),
            ],
        },
        Menu {
            name: "Session".into(),
            items: vec![
                MenuItem::action("New Session", NewSession),
                MenuItem::action("Close Session", CloseSession),
            ],
        },
    ]
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
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, Some(KEY_CONTEXT)),
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

        // Load settings before the widget layer installs its default theme, then
        // override that theme to match what the user configured.
        app_settings::init(cx);
        ui::init(cx);
        TerminalView::init(cx);
        bind_shortcuts(cx);
        cx.set_menus(app_menus());

        let settings = app_settings::current(cx);
        apply_ui_theme(settings.ui_theme, cx);

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
                // Wayland compositors and X11 docks match this against
                // logman.desktop to pick up the application icon.
                app_id: Some("logman".into()),
                // A translucent or blurred window needs the platform surface to
                // permit alpha; the terminal view then tints its background.
                window_background: window_appearance(&settings.window),
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
