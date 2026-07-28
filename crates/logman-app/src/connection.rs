//! Connection dialog: saved profiles and the form used to open a session.
//!
//! The dialog is the only place in the application that touches
//! [`ProfileStore`] and [`SecretStore`]: by the time it emits
//! [`ConnectionDialogEvent::Connect`] the profile is on disk, the secret is in
//! the OS keychain (when the user asked for that), and the credentials have
//! been resolved into a ready-to-use [`SshAuth`].
//!
//! # Handling of secrets
//!
//! Passwords and key passphrases live only in the masked [`TextInput`]s and in
//! the [`SshAuth`] handed to the caller. They are never logged, never rendered
//! unmasked, and never included in a status message. [`ConnectionDialog`]
//! deliberately does not implement `Debug` so that a stray `{:?}` cannot leak
//! them either.

use std::path::PathBuf;
use std::sync::Once;

use gpui::{
    App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    KeyBinding, KeyDownEvent, MouseButton, PathPromptOptions, Render, SharedString, Window,
    actions, div, prelude::*, px,
};
use logman_core::{AuthMethod, ProfileStore, SecretStore, SessionProfile};
use logman_ssh::SshAuth;
use uuid::Uuid;

use crate::ui::{Button, ButtonVariant, Checkbox, Segmented, TextInput, form_row, modal, theme};

/// Port pre-filled into the form and used when the port field is left empty.
const DEFAULT_PORT: u16 = 22;

/// Widest port number that still fits in a `u16`, in digits.
const MAX_PORT_DIGITS: usize = 5;

/// Width of the dialog panel.
///
/// Wide enough that the longest control label — the "Remember passphrase in the
/// system keychain" checkbox, plus its focus ring — still fits on one line.
const DIALOG_WIDTH: f32 = 724.;

/// Width of the saved-profile column.
const LIST_WIDTH: f32 = 260.;

/// Height at which the saved-profile column starts scrolling.
const LIST_MAX_HEIGHT: f32 = 300.;

/// Segments of the authentication picker, in [`AuthKind`] order.
const AUTH_OPTIONS: [(&str, &str); 3] = [
    ("password", "Password"),
    ("key", "Private key"),
    ("agent", "Agent"),
];

/// Shown instead of the profile list on a first run.
const EMPTY_LIST_HINT: &str = "No saved profiles yet. Fill in the form and connect \u{2014} the \
                               connection is saved automatically.";

/// Explanation shown while the unsupported agent method is selected.
const AGENT_UNSUPPORTED: &str = "SSH agent authentication is not supported yet.";

/// Key context the dialog's own shortcuts are scoped to.
///
/// `Tab` **must** stay scoped to this context. The terminal forwards `Tab` to
/// the remote shell for completion, so a binding registered against the global
/// (`None`) context would silently break it.
const KEY_CONTEXT: &str = "ConnectionDialog";

/// Guards the one-time registration of the dialog's key bindings.
static BIND_KEYS: Once = Once::new();

actions!(
    logman_connection,
    [
        /// Move focus to the next control in the dialog.
        FocusNext,
        /// Move focus to the previous control in the dialog.
        FocusPrev,
    ]
);

/// Tab order of the form, in visual order.
///
/// Indices are spaced so that the controls which only exist in one
/// authentication mode can be numbered without renumbering their neighbours;
/// a control that is not rendered is never painted and therefore never enters
/// the tab ring at all, so the gaps are harmless.
mod tab {
    /// Connection name.
    pub const NAME: isize = 10;
    /// Host name or address.
    pub const HOST: isize = 20;
    /// TCP port.
    pub const PORT: isize = 30;
    /// Remote login name.
    pub const USERNAME: isize = 40;
    /// Authentication method picker.
    pub const AUTH: isize = 50;
    /// Password, or the key path in private key mode.
    pub const SECRET_OR_KEY: isize = 60;
    /// The key file browser button.
    pub const BROWSE: isize = 65;
    /// Private key passphrase.
    pub const PASSPHRASE: isize = 70;
    /// "Remember ... in the system keychain".
    pub const REMEMBER: isize = 80;
    /// Cancel.
    pub const CANCEL: isize = 90;
    /// Connect.
    pub const CONNECT: isize = 100;
}

/// Emitted by [`ConnectionDialog`] when the user acts on it.
pub enum ConnectionDialogEvent {
    /// Open a session. The dialog has already persisted the profile and any
    /// secret the user asked to remember.
    Connect {
        /// Profile describing the target host.
        profile: SessionProfile,
        /// Credentials resolved from the form and the OS keychain.
        auth: SshAuth,
    },
    /// The dialog was dismissed without connecting.
    Dismissed,
}

/// Authentication method offered by the form.
///
/// Mirrors [`AuthMethod`] but is ordered, because the segmented control
/// addresses its options by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    /// Password authentication.
    Password,
    /// Public key authentication with a key file on disk.
    PrivateKey,
    /// Delegate to a running SSH agent. Not implemented by `logman-ssh` yet.
    Agent,
}

impl AuthKind {
    /// Index of this method in [`AUTH_OPTIONS`].
    fn index(self) -> usize {
        match self {
            Self::Password => 0,
            Self::PrivateKey => 1,
            Self::Agent => 2,
        }
    }

    /// The method at `index` in [`AUTH_OPTIONS`], defaulting to
    /// [`AuthKind::Password`].
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::PrivateKey,
            2 => Self::Agent,
            _ => Self::Password,
        }
    }
}

/// Severity of the message strip at the bottom of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLevel {
    /// Neutral guidance, e.g. "a saved secret will be used".
    Info,
    /// Something went wrong but the connection can still proceed.
    Warning,
    /// The action could not be completed.
    Error,
}

impl StatusLevel {
    /// Color of the message text under the active theme.
    fn color(self, theme: &crate::ui::Theme) -> Hsla {
        match self {
            Self::Info => theme.text_muted,
            Self::Warning => theme.accent,
            Self::Error => theme.danger,
        }
    }
}

/// A message rendered inside the dialog.
struct DialogStatus {
    /// How loudly to render it.
    level: StatusLevel,
    /// Text shown to the user. Never contains a secret.
    message: SharedString,
}

/// Field that should receive keyboard focus the next time the dialog renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusTarget {
    /// The host field, for a brand new connection.
    Host,
    /// The password or passphrase field, for a profile that is already filled in.
    Secret,
}

/// Modal dialog for picking a saved profile or entering a new connection.
///
/// The dialog is an entity: create it once with [`ConnectionDialog::new`], keep
/// the handle, subscribe to [`ConnectionDialogEvent`], and render it as the last
/// child of a `relative()` root element so the backdrop covers the window.
///
/// It renders nothing at all while [`ConnectionDialog::is_open`] is `false`, so
/// it is safe to render unconditionally.
pub struct ConnectionDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// Saved profiles, reloaded from disk every time the dialog opens.
    store: ProfileStore,
    /// Identifier of the profile the form was filled from, if any. Kept so that
    /// connecting updates the existing profile instead of duplicating it.
    editing: Option<Uuid>,
    /// Authentication method currently selected in the form.
    auth_kind: AuthKind,
    /// Whether the secret should be written to the OS keychain.
    save_secret: bool,
    /// Message strip shown under the form.
    status: Option<DialogStatus>,
    /// Focus of the dialog root; also the anchor for the `Escape` handler.
    focus_handle: FocusHandle,
    /// Field to focus on the next render, set when the dialog opens.
    pending_focus: Option<FocusTarget>,
    /// Display name of the connection.
    name_input: Entity<TextInput>,
    /// Host name or address.
    host_input: Entity<TextInput>,
    /// TCP port; kept digits-only by an observer installed in [`Self::new`].
    port_input: Entity<TextInput>,
    /// Remote login name.
    username_input: Entity<TextInput>,
    /// Password, masked.
    password_input: Entity<TextInput>,
    /// Path of the private key file.
    key_path_input: Entity<TextInput>,
    /// Private key passphrase, masked.
    passphrase_input: Entity<TextInput>,
}

impl ConnectionDialog {
    /// Build the dialog, loading saved profiles from disk.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();

        // Scoped to the dialog's key context on purpose: a global `tab` binding
        // would stop the terminal from sending `\t` to the remote shell.
        BIND_KEYS.call_once(|| {
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrev, Some(KEY_CONTEXT)),
            ]);
        });

        // Every field submits the whole form, so `Enter` connects from anywhere.
        let field = {
            let weak = weak.clone();
            move |cx: &mut Context<Self>,
                  placeholder: &'static str,
                  masked: bool,
                  tab_index: isize| {
                let weak = weak.clone();
                cx.new(move |cx| {
                    TextInput::new(cx)
                        .placeholder(placeholder)
                        .masked(masked)
                        .tab_index(tab_index)
                        .on_submit(move |_, _window, cx| {
                            // `on_submit` fires from inside the TextInput's own
                            // `update`, which means gpui has leased that entity
                            // out of the entity map. Submitting reads every
                            // field back — including the one that fired — and a
                            // `read` of a leased entity is a hard panic. Defer
                            // to the end of the effect cycle, by which point the
                            // lease has been returned.
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                weak.update(cx, |this, cx| this.submit(cx)).ok();
                            });
                        })
                })
            }
        };

        let name_input = field(cx, "web-01", false, tab::NAME);
        let host_input = field(cx, "web-01.example.com", false, tab::HOST);
        let port_input = field(cx, "22", false, tab::PORT);
        let username_input = field(cx, "alice", false, tab::USERNAME);
        let password_input = field(cx, "password", true, tab::SECRET_OR_KEY);
        let key_path_input = field(cx, "~/.ssh/id_ed25519", false, tab::SECRET_OR_KEY);
        let passphrase_input = field(cx, "optional", true, tab::PASSPHRASE);

        port_input.update(cx, |input, cx| {
            input.set_content(DEFAULT_PORT.to_string(), cx);
        });

        // The text field has no input filter, so the port is sanitised after the
        // fact. Rewriting only when the text actually changes stops the observer
        // from re-triggering itself.
        cx.observe(&port_input, |_this, input, cx| {
            let content = input.read(cx).content().to_owned();
            let digits: String = content
                .chars()
                .filter(char::is_ascii_digit)
                .take(MAX_PORT_DIGITS)
                .collect();
            if digits != content {
                input.update(cx, |input, cx| input.set_content(digits, cx));
            }
        })
        .detach();

        let store = ProfileStore::load().unwrap_or_else(|err| {
            log::warn!("starting with an empty profile store: {err:#}");
            ProfileStore::default()
        });

        Self {
            open: false,
            store,
            editing: None,
            auth_kind: AuthKind::Password,
            save_secret: false,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: None,
            name_input,
            host_input,
            port_input,
            username_input,
            password_input,
            key_path_input,
            passphrase_input,
        }
    }

    /// Show the dialog with an empty form.
    pub fn open_new(&mut self, cx: &mut Context<Self>) {
        self.reload_store();
        self.reset_form(cx);
        self.open = true;
        self.pending_focus = Some(FocusTarget::Host);
        cx.notify();
    }

    /// Show the dialog pre-filled from the saved profile `id`.
    ///
    /// An unknown `id` opens the empty form rather than failing.
    pub fn open_profile(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.reload_store();
        self.reset_form(cx);
        self.open = true;

        match self.store.get(id).cloned() {
            Some(profile) => {
                let has_secret = profile.save_secret;
                let agent = matches!(profile.auth, AuthMethod::Agent);
                self.fill_form(&profile, cx);
                self.pending_focus = Some(if agent {
                    FocusTarget::Host
                } else {
                    FocusTarget::Secret
                });
                if agent {
                    self.set_status(StatusLevel::Warning, AGENT_UNSUPPORTED);
                } else if has_secret {
                    self.set_status(
                        StatusLevel::Info,
                        "A secret saved in the system keychain will be used unless you type a \
                         new one.",
                    );
                }
            }
            None => {
                log::warn!("connection dialog asked to open unknown profile {id}");
                self.pending_focus = Some(FocusTarget::Host);
            }
        }

        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hide the dialog without connecting.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_focus = None;
        // A closed dialog has nothing to report; leaving the last message behind
        // would let it reappear for a moment the next time the dialog opens.
        self.status = None;
        // Never keep a secret in memory longer than the dialog is on screen.
        self.password_input.update(cx, |input, cx| input.clear(cx));
        self.passphrase_input
            .update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    /// Saved profiles, in stored order.
    pub fn profiles(&self) -> Vec<SessionProfile> {
        self.store.profiles().to_vec()
    }

    /// Re-read the profile store so external edits are picked up when the dialog
    /// opens. A failure leaves the previously loaded profiles in place.
    fn reload_store(&mut self) {
        match ProfileStore::load() {
            Ok(store) => self.store = store,
            Err(err) => log::warn!("keeping the previously loaded profiles: {err:#}"),
        }
    }

    /// Clear every field and drop any selection.
    fn reset_form(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.auth_kind = AuthKind::Password;
        self.save_secret = false;
        self.status = None;

        self.name_input.update(cx, |input, cx| input.clear(cx));
        self.host_input.update(cx, |input, cx| input.clear(cx));
        self.username_input.update(cx, |input, cx| input.clear(cx));
        self.password_input.update(cx, |input, cx| input.clear(cx));
        self.key_path_input.update(cx, |input, cx| input.clear(cx));
        self.passphrase_input
            .update(cx, |input, cx| input.clear(cx));
        self.port_input.update(cx, |input, cx| {
            input.set_content(DEFAULT_PORT.to_string(), cx);
        });
    }

    /// Copy `profile` into the form and remember that it is being edited.
    ///
    /// Secrets are never copied back into the form: an empty password field
    /// means "reuse whatever the keychain holds".
    fn fill_form(&mut self, profile: &SessionProfile, cx: &mut Context<Self>) {
        self.name_input
            .update(cx, |input, cx| input.set_content(profile.name.clone(), cx));
        self.host_input
            .update(cx, |input, cx| input.set_content(profile.host.clone(), cx));
        self.port_input.update(cx, |input, cx| {
            input.set_content(profile.port.to_string(), cx)
        });
        self.username_input.update(cx, |input, cx| {
            input.set_content(profile.username.clone(), cx)
        });
        self.password_input.update(cx, |input, cx| input.clear(cx));
        self.passphrase_input
            .update(cx, |input, cx| input.clear(cx));

        match &profile.auth {
            AuthMethod::Password => {
                self.auth_kind = AuthKind::Password;
                self.key_path_input.update(cx, |input, cx| input.clear(cx));
            }
            AuthMethod::PublicKey { key_path } => {
                self.auth_kind = AuthKind::PrivateKey;
                let path = key_path.display().to_string();
                self.key_path_input
                    .update(cx, |input, cx| input.set_content(path, cx));
            }
            AuthMethod::Agent => {
                self.auth_kind = AuthKind::Agent;
                self.key_path_input.update(cx, |input, cx| input.clear(cx));
            }
        }

        self.save_secret = profile.save_secret;
        self.editing = Some(profile.id);
    }

    /// Replace the message strip.
    fn set_status(&mut self, level: StatusLevel, message: impl Into<SharedString>) {
        self.status = Some(DialogStatus {
            level,
            message: message.into(),
        });
    }

    /// Load the profile `id` into the form.
    fn select_profile(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(profile) = self.store.get(id).cloned() else {
            return;
        };
        let has_secret = profile.save_secret;
        let agent = matches!(profile.auth, AuthMethod::Agent);
        self.fill_form(&profile, cx);
        self.status = None;
        if agent {
            self.set_status(StatusLevel::Warning, AGENT_UNSUPPORTED);
        } else if has_secret {
            self.set_status(
                StatusLevel::Info,
                "A secret saved in the system keychain will be used unless you type a new one.",
            );
        }
        cx.notify();
    }

    /// Forget the profile `id`, together with any secret stored for it.
    ///
    /// Deleting the secret alongside the profile is what keeps the keychain from
    /// accumulating entries nothing refers to any more.
    fn delete_profile(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.store.remove(id).is_none() {
            return;
        }

        let mut problems = Vec::new();
        if let Err(err) = self.store.save() {
            problems.push(format!("the profile list could not be written ({err:#})"));
        }
        if let Err(err) = SecretStore::delete(id) {
            problems.push(format!("its keychain entry could not be removed ({err:#})"));
        }

        if self.editing == Some(id) {
            self.reset_form(cx);
        }

        if problems.is_empty() {
            self.status = None;
        } else {
            self.set_status(
                StatusLevel::Error,
                format!("Profile deleted, but {}.", problems.join(" and ")),
            );
        }
        cx.notify();
    }

    /// Switch the authentication method, discarding the secret typed for the
    /// previous one so it cannot be sent to the wrong place.
    fn set_auth_kind(&mut self, kind: AuthKind, cx: &mut Context<Self>) {
        if self.auth_kind == kind {
            return;
        }
        self.auth_kind = kind;
        self.password_input.update(cx, |input, cx| input.clear(cx));
        self.passphrase_input
            .update(cx, |input, cx| input.clear(cx));
        self.status = match kind {
            AuthKind::Agent => Some(DialogStatus {
                level: StatusLevel::Warning,
                message: AGENT_UNSUPPORTED.into(),
            }),
            _ => None,
        };
        cx.notify();
    }

    /// Set the private key path, e.g. from the platform file picker.
    fn set_key_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let text = path.display().to_string();
        self.key_path_input
            .update(cx, |input, cx| input.set_content(text, cx));
        cx.notify();
    }

    /// Trimmed content of `input`.
    fn text(input: &Entity<TextInput>, cx: &App) -> String {
        input.read(cx).content().trim().to_owned()
    }

    /// The port typed into the form, or `None` when it is out of range.
    ///
    /// An empty field means [`DEFAULT_PORT`].
    fn port(&self, cx: &App) -> Option<u16> {
        let raw = Self::text(&self.port_input, cx);
        if raw.is_empty() {
            return Some(DEFAULT_PORT);
        }
        raw.parse::<u16>().ok().filter(|port| *port != 0)
    }

    /// Whether the form holds enough information to open a session.
    fn can_connect(&self, cx: &App) -> bool {
        if self.auth_kind == AuthKind::Agent {
            return false;
        }
        if Self::text(&self.host_input, cx).is_empty()
            || Self::text(&self.username_input, cx).is_empty()
        {
            return false;
        }
        if self.auth_kind == AuthKind::PrivateKey && Self::text(&self.key_path_input, cx).is_empty()
        {
            return false;
        }
        self.port(cx).is_some()
    }

    /// `Enter` in any field: connect when the form is complete, explain why not
    /// otherwise.
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.can_connect(cx) {
            self.connect(cx);
        } else {
            self.explain_incomplete(cx);
        }
    }

    /// Fill the message strip with the reason [`Self::can_connect`] said no.
    fn explain_incomplete(&mut self, cx: &mut Context<Self>) {
        let reason = if self.auth_kind == AuthKind::Agent {
            AGENT_UNSUPPORTED
        } else if Self::text(&self.host_input, cx).is_empty() {
            "Enter the host to connect to."
        } else if Self::text(&self.username_input, cx).is_empty() {
            "Enter the user to log in as."
        } else if self.auth_kind == AuthKind::PrivateKey
            && Self::text(&self.key_path_input, cx).is_empty()
        {
            "Choose the private key file to authenticate with."
        } else {
            "Enter a port between 1 and 65535."
        };
        self.set_status(StatusLevel::Error, reason);
        cx.notify();
    }

    /// Persist the form, resolve the credentials and emit
    /// [`ConnectionDialogEvent::Connect`].
    ///
    /// Storage problems never block the connection: they are reported in the
    /// message strip and the dialog stays open so the user can read them, while
    /// the session opens behind it. A clean run closes the dialog.
    fn connect(&mut self, cx: &mut Context<Self>) {
        if !self.can_connect(cx) {
            self.explain_incomplete(cx);
            return;
        }

        let auth_kind = self.auth_kind;
        let host = Self::text(&self.host_input, cx);
        let username = Self::text(&self.username_input, cx);
        let key_path = PathBuf::from(Self::text(&self.key_path_input, cx));
        let Some(port) = self.port(cx) else {
            self.explain_incomplete(cx);
            return;
        };

        let name = {
            let typed = Self::text(&self.name_input, cx);
            if typed.is_empty() {
                host.clone()
            } else {
                typed
            }
        };

        let auth_method = match auth_kind {
            AuthKind::Password => AuthMethod::Password,
            AuthKind::PrivateKey => AuthMethod::PublicKey {
                key_path: key_path.clone(),
            },
            // `can_connect` already rejected the agent method.
            AuthKind::Agent => return,
        };

        let mut profile = match self.editing.and_then(|id| self.store.get(id).cloned()) {
            Some(mut existing) => {
                existing.name = name;
                existing.host = host;
                existing.port = port;
                existing.username = username;
                existing.auth = auth_method;
                existing
            }
            None => SessionProfile::new(name, host, port, username, auth_method),
        };
        profile.save_secret = self.save_secret;

        let mut problems: Vec<String> = Vec::new();

        // The secret typed into the form wins; an empty field falls back to the
        // keychain, which is how a saved profile connects without retyping.
        let typed = match auth_kind {
            AuthKind::Password => self.password_input.read(cx).content().to_owned(),
            AuthKind::PrivateKey => self.passphrase_input.read(cx).content().to_owned(),
            AuthKind::Agent => String::new(),
        };
        let secret = if !typed.is_empty() {
            typed
        } else if self.editing.is_some() {
            match SecretStore::get(profile.id) {
                Ok(stored) => stored.unwrap_or_default(),
                Err(err) => {
                    problems.push(format!("the saved secret could not be read ({err:#})"));
                    String::new()
                }
            }
        } else {
            String::new()
        };

        self.store.upsert(profile.clone());
        if let Err(err) = self.store.save() {
            problems.push(format!("the profile could not be saved ({err:#})"));
        }

        if profile.save_secret {
            if secret.is_empty() {
                problems.push("there was no secret to put in the keychain".to_owned());
            } else if let Err(err) = SecretStore::set(profile.id, &secret) {
                problems.push(format!("the secret could not be saved ({err:#})"));
            }
        } else if let Err(err) = SecretStore::delete(profile.id) {
            problems.push(format!(
                "a previously saved secret could not be removed ({err:#})"
            ));
        }

        let auth = match auth_kind {
            AuthKind::Password => SshAuth::Password(secret),
            AuthKind::PrivateKey => SshAuth::PrivateKeyFile {
                path: key_path,
                passphrase: (!secret.is_empty()).then_some(secret),
            },
            AuthKind::Agent => return,
        };

        self.editing = Some(profile.id);
        cx.emit(ConnectionDialogEvent::Connect { profile, auth });

        if problems.is_empty() {
            self.close(cx);
        } else {
            self.set_status(
                StatusLevel::Warning,
                format!("Connecting, but {}.", problems.join(", and ")),
            );
            cx.notify();
        }
    }

    /// Close the dialog and report that nothing was connected.
    ///
    /// This is the single dismissal path: `Escape`, the backdrop and the Cancel
    /// button all route through here, so [`ConnectionDialogEvent::Dismissed`] is
    /// emitted exactly once however the user backs out.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ConnectionDialogEvent::Dismissed);
        self.close(cx);
    }

    /// `Tab`: move focus to the next control.
    ///
    /// gpui's tab ring wraps on its own — [`Window::focus_next`] falls back to
    /// the first stop once it runs off the end — so there is nothing to add here.
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, _cx: &mut Context<Self>) {
        window.focus_next();
    }

    /// `Shift+Tab`: move focus to the previous control, wrapping to the last.
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, _cx: &mut Context<Self>) {
        window.focus_prev();
    }

    /// `Escape` dismisses the dialog from anywhere inside it.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.open && event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.dismiss(cx);
        }
    }

    /// The saved-profile column.
    fn render_profile_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme(cx);
        let this = cx.entity();
        let selected = self.editing;

        let rows = self
            .store
            .profiles()
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let id = profile.id;
                let is_selected = selected == Some(id);
                let group = SharedString::from(format!("logman-profile-{index}"));

                div()
                    .id(ElementId::from(("connection-profile", index)))
                    .group(group.clone())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if is_selected {
                        theme.surface_active
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|style| {
                        style.bg(if is_selected {
                            theme.surface_active
                        } else {
                            theme.surface_hover
                        })
                    })
                    .on_click({
                        let this = this.clone();
                        move |event, _window, cx| {
                            let double = event.click_count() >= 2;
                            this.update(cx, |dialog, cx| {
                                dialog.select_profile(id, cx);
                                if double {
                                    dialog.connect(cx);
                                }
                            });
                        }
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow()
                            .min_w_0()
                            .gap(px(1.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(theme.text)
                                    .child(SharedString::from(profile.name.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(theme.text_muted)
                                    .child(SharedString::from(profile.label())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .gap(px(2.))
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .child(row_action(
                                ElementId::from(("connection-profile-edit", index)),
                                "Edit",
                                theme.text_muted,
                                theme.surface_hover,
                                {
                                    let this = this.clone();
                                    move |cx| {
                                        this.update(cx, |dialog, cx| {
                                            dialog.select_profile(id, cx);
                                            dialog.pending_focus = Some(FocusTarget::Host);
                                        });
                                    }
                                },
                            ))
                            .child(row_action(
                                ElementId::from(("connection-profile-delete", index)),
                                "Delete",
                                theme.danger,
                                theme.surface_hover,
                                {
                                    let this = this.clone();
                                    move |cx| {
                                        this.update(cx, |dialog, cx| {
                                            dialog.delete_profile(id, cx);
                                        });
                                    }
                                },
                            )),
                    )
            })
            .collect::<Vec<_>>();

        let empty = rows.is_empty();

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .w(px(LIST_WIDTH))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child("Saved profiles"),
            )
            .child(
                div()
                    .id("connection-profile-list")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .p(px(4.))
                    .max_h(px(LIST_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p(px(8.))
                                .text_size(px(12.))
                                .text_color(theme.text_muted)
                                .child(EMPTY_LIST_HINT),
                        )
                    })
                    .children(rows),
            )
    }

    /// The connection form.
    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme(cx);
        let this = cx.entity();
        let auth_kind = self.auth_kind;

        let auth_control = Segmented::new("connection-auth")
            .options(AUTH_OPTIONS)
            .selected(auth_kind.index())
            .tab_index(tab::AUTH)
            .on_select({
                let this = this.clone();
                move |index, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_auth_kind(AuthKind::from_index(index), cx);
                    });
                }
            });

        let key_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .w_full()
            .child(
                div()
                    .flex_grow()
                    .min_w_0()
                    .child(self.key_path_input.clone()),
            )
            .child(
                Button::new("connection-browse", "Browse\u{2026}")
                    .variant(ButtonVariant::Secondary)
                    .tab_index(tab::BROWSE)
                    .on_click({
                        let this = this.clone();
                        move |_, _window, cx| browse_for_key(this.clone(), cx)
                    }),
            );

        let secret_label = match auth_kind {
            AuthKind::PrivateKey => "Remember passphrase in the system keychain",
            _ => "Remember password in the system keychain",
        };

        let remember = Checkbox::new("connection-remember", secret_label)
            .checked(self.save_secret)
            .tab_index(tab::REMEMBER)
            .on_toggle({
                let this = this.clone();
                move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.save_secret = checked;
                        cx.notify();
                    });
                }
            });

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_w_0()
            .gap(px(10.))
            .child(form_row("Name", self.name_input.clone()))
            .child(form_row("Host", self.host_input.clone()))
            .child(form_row("Port", self.port_input.clone()))
            .child(form_row("Username", self.username_input.clone()))
            .child(form_row("Authentication", auth_control))
            .when(auth_kind == AuthKind::Password, |this| {
                this.child(form_row("Password", self.password_input.clone()))
            })
            .when(auth_kind == AuthKind::PrivateKey, |this| {
                this.child(form_row("Key file", key_row))
                    .child(form_row("Passphrase", self.passphrase_input.clone()))
            })
            .when(auth_kind == AuthKind::Agent, |this| {
                this.child(form_row(
                    "",
                    div()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(AGENT_UNSUPPORTED),
                ))
            })
            .when(auth_kind != AuthKind::Agent, |this| {
                this.child(form_row("", remember))
            })
    }

    /// The message strip and the action buttons.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme(cx);
        let this = cx.entity();
        let connectable = self.can_connect(cx);

        let status = self.status.as_ref().map(|status| {
            div()
                .text_size(px(12.))
                .text_color(status.level.color(&theme))
                .child(status.message.clone())
        });

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().h(px(1.)).w_full().flex_none().bg(theme.border))
            .children(status)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("connection-cancel", "Cancel")
                            .variant(ButtonVariant::Secondary)
                            .tab_index(tab::CANCEL)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.dismiss(cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("connection-connect", "Connect")
                            .variant(ButtonVariant::Primary)
                            .disabled(!connectable)
                            .tab_index(tab::CONNECT)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.connect(cx));
                                }
                            }),
                    ),
            )
    }

    /// Move focus into the field recorded by the last `open_*` call.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_focus.take() else {
            return;
        };
        let input = match (target, self.auth_kind) {
            (FocusTarget::Secret, AuthKind::Password) => &self.password_input,
            (FocusTarget::Secret, AuthKind::PrivateKey) => &self.passphrase_input,
            _ => &self.host_input,
        };
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }
}

impl EventEmitter<ConnectionDialogEvent> for ConnectionDialog {}

impl Focusable for ConnectionDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectionDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("connection-dialog");
        }

        self.apply_pending_focus(window, cx);

        let title = if self.editing.is_some() {
            "Connect"
        } else {
            "New connection"
        };

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(16.))
                    .child(self.render_profile_list(cx))
                    .child(self.render_form(cx)),
            )
            .child(self.render_footer(cx));

        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.dismiss(cx));
            }
        };

        // The wrapper exists only to own the focus handle and the `Escape`
        // binding. It has to span its parent, because an absolutely positioned
        // element is laid out against its direct parent: a shrink-to-fit
        // wrapper would collapse to zero height and drag the modal off-screen
        // with it.
        div()
            .id("connection-dialog")
            .key_context(KEY_CONTEXT)
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                "connection-modal",
                title,
                px(DIALOG_WIDTH),
                body,
                on_dismiss,
            ))
    }
}

/// A compact text button used inside the profile rows.
///
/// The mouse-down handler stops propagation so that clicking an action does not
/// also select the row it lives in.
fn row_action(
    id: ElementId,
    label: &'static str,
    color: Hsla,
    hover: Hsla,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(18.))
        .px(px(6.))
        .rounded_sm()
        .whitespace_nowrap()
        .text_size(px(11.))
        .text_color(color)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _window, cx| on_click(cx))
        .child(label)
}

/// Ask the platform for a private key file and write the choice into `dialog`.
///
/// The picker is asynchronous, so the result arrives on a spawned task; a
/// cancelled dialog simply leaves the field untouched.
fn browse_for_key(dialog: Entity<ConnectionDialog>, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Select".into()),
    });

    cx.spawn(async move |cx| {
        let selection = match paths.await {
            Ok(Ok(Some(paths))) => paths.into_iter().next(),
            Ok(Ok(None)) => None,
            Ok(Err(err)) => {
                log::warn!("the file picker could not be opened: {err:#}");
                None
            }
            Err(_) => None,
        };
        let Some(path) = selection else {
            return;
        };
        dialog
            .update(cx, |dialog, cx| dialog.set_key_path(path, cx))
            .ok();
    })
    .detach();
}
