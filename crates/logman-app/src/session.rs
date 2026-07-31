//! One logman session: an SSH transport bound to a terminal emulator.
//!
//! A [`Session`] is a gpui entity. Creating one immediately starts connecting
//! and spawns a pump that drains [`SshEvent`]s onto the UI thread, so the whole
//! type is single threaded and never blocks a render.
//!
//! Credentials are kept for reconnection but are deliberately unreachable from
//! the outside: there is no accessor for them, and the hand written
//! [`Debug`](std::fmt::Debug) implementation omits them entirely.

use std::fmt;

use futures::StreamExt;
use gpui::{App, AppContext, Context, Entity, SharedString, Task};
use logman_core::{EffectiveTerminal, SessionProfile};
use logman_ssh::{SftpClient, SshAuth, SshConfig, SshErrorKind, SshEvent, SshSession};
use logman_term::{TerminalModel, TerminalTheme};

use crate::app_settings;
use crate::i18n::ts;
use crate::ui::TabStatus;
use crate::verifier::host_key_verifier;

/// Columns a session starts with, until the view reports its real size.
const INITIAL_COLS: u16 = 80;

/// Rows a session starts with, until the view reports its real size.
const INITIAL_ROWS: u16 = 24;

/// Where a [`Session`] currently is in its life cycle.
#[derive(Debug, Clone)]
pub enum SessionStatus {
    /// The transport is connecting, verifying the host key or authenticating.
    Connecting,
    /// The remote shell is live.
    Connected,
    /// The session ended without an error.
    Disconnected {
        /// Human-readable explanation of why the session ended.
        reason: String,
    },
    /// The session failed and cannot continue without a reconnect.
    Failed {
        /// Coarse classification of the failure.
        kind: SshErrorKind,
        /// Human-readable explanation, safe to show to the user.
        message: String,
    },
}

impl SessionStatus {
    /// A short label for the status bar and the connection overlay.
    ///
    /// Translated here rather than stored translated, because the status
    /// outlives the language it was reached in: the caller asks for the summary
    /// while rendering, so a language switch shows up on the very next frame.
    /// The `reason`, `kind` and `message` inside come from the SSH layer and
    /// stay in English; only the wording around them follows the locale.
    pub fn summary(&self) -> SharedString {
        match self {
            Self::Connecting => ts!("session.connecting"),
            Self::Connected => ts!("session.connected"),
            Self::Disconnected { reason } => ts!("session.disconnected", reason = reason),
            Self::Failed { kind, message } => {
                ts!("session.failed", kind = kind.to_string(), message = message)
            }
        }
    }

    /// Whether the session can currently accept input.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Connecting | Self::Connected)
    }
}

/// A single SSH session together with the terminal it drives.
pub struct Session {
    /// Profile the session was opened from.
    profile: SessionProfile,
    /// Credentials, retained so that [`Session::reconnect`] can reuse them.
    ///
    /// Never rendered, logged or otherwise exposed.
    auth: SshAuth,
    /// Live transport handle; `None` once the session has ended.
    ssh: Option<SshSession>,
    /// Screen contents and scrollback.
    terminal: TerminalModel,
    /// Current life cycle state.
    status: SessionStatus,
    /// Task draining the SSH event stream; dropping it stops the pump.
    _pump: Option<Task<()>>,
}

impl fmt::Debug for Session {
    /// Written by hand so that [`Session::auth`] can never reach a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("profile", &self.profile.label())
            .field("status", &self.status)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Builds a session for `profile` and starts connecting straight away.
    ///
    /// The terminal is created from the effective settings — the global defaults
    /// with the profile's overrides applied — so the scheme and scrollback depth
    /// are correct from the very first frame.
    pub fn new(profile: SessionProfile, auth: SshAuth, cx: &mut Context<Self>) -> Self {
        let effective = app_settings::current(cx).effective_terminal(&profile.overrides);
        let mut session = Self {
            profile,
            auth,
            ssh: None,
            terminal: TerminalModel::new(
                INITIAL_COLS,
                INITIAL_ROWS,
                effective.scrollback_lines,
                TerminalTheme::by_name_or_default(&effective.scheme),
            ),
            status: SessionStatus::Connecting,
            _pump: None,
        };
        session.start(cx);
        session
    }

    /// The effective terminal settings for this session: the global defaults
    /// with this profile's overrides layered on top.
    ///
    /// Exposed so the view can honor per-session values such as the font size
    /// and the copy-on-select behaviour without re-reading the global settings.
    pub fn effective(&self, cx: &App) -> EffectiveTerminal {
        app_settings::current(cx).effective_terminal(&self.profile.overrides)
    }

    /// Re-reads the settings and applies the ones that can change on a live
    /// session.
    ///
    /// Only the color scheme takes effect immediately. The scrollback depth is
    /// fixed when the terminal model is built — changing it would rebuild the
    /// grid and clear the screen — and the `TERM` value has already been
    /// negotiated with the remote pty, so both are picked up only on the next
    /// reconnect instead.
    pub fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let effective = self.effective(cx);
        self.terminal
            .set_theme(TerminalTheme::by_name_or_default(&effective.scheme));
        cx.notify();
    }

    /// The current life cycle state.
    pub fn status(&self) -> &SessionStatus {
        &self.status
    }

    /// The profile the session was opened from.
    pub fn profile(&self) -> &SessionProfile {
        &self.profile
    }

    /// The title to show in the tab: the `OSC 0` / `OSC 2` title when the
    /// remote shell set one, the profile name otherwise.
    pub fn title(&self) -> SharedString {
        match self.terminal.title() {
            Some(title) if !title.trim().is_empty() => SharedString::from(title.to_owned()),
            _ => SharedString::from(self.profile.name.clone()),
        }
    }

    /// The working directory of the remote shell, if it announced one.
    ///
    /// Fed by the `OSC 7` / `OSC 1337` sequences a configured prompt emits, so
    /// it stays `None` for shells that do not report their directory. The value
    /// survives a disconnect - the last known directory is still the one the
    /// session ended in - and is cleared by [`Session::reconnect`], because the
    /// new shell has not reported anything yet.
    pub fn cwd(&self) -> Option<&str> {
        self.terminal.cwd()
    }

    /// An SFTP client riding on this session's transport, or `None` while the
    /// session is not carrying a live shell.
    ///
    /// Gated on [`SessionStatus::Connected`] rather than merely on the transport
    /// existing: during `Connecting` the handle is already there and the SFTP
    /// service would happily queue requests behind the authentication, so a file
    /// panel would sit on a pending listing with nothing to show for it. Once
    /// the session ends the client is gone and every call would answer
    /// [`SftpError::Disconnected`](logman_ssh::SftpError::Disconnected) anyway.
    ///
    /// Cheap: the returned client only clones the request channel, and the SFTP
    /// channel itself is opened lazily on the first request and then reused.
    pub fn sftp(&self) -> Option<SftpClient> {
        match self.status {
            SessionStatus::Connected => self.ssh.as_ref().map(SshSession::sftp),
            _ => None,
        }
    }

    /// The terminal model, for rendering.
    pub fn terminal(&self) -> &TerminalModel {
        &self.terminal
    }

    /// The terminal model, for scrolling and other view driven mutations.
    pub fn terminal_mut(&mut self) -> &mut TerminalModel {
        &mut self.terminal
    }

    /// Sends already encoded key or paste bytes to the remote shell.
    ///
    /// Typing always snaps the viewport back to the bottom of the scrollback,
    /// which is what every other terminal does.
    pub fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        self.terminal.scroll_to_bottom();
        if let Some(ssh) = &self.ssh {
            ssh.send_input(bytes);
        }
        cx.notify();
    }

    /// Resizes the terminal and tells the remote pty about it.
    ///
    /// A resize to the current size is ignored, so callers may invoke this on
    /// every layout pass without flooding the connection with window change
    /// requests.
    pub fn resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.terminal.size() == (cols, rows) {
            return;
        }

        self.terminal.resize(cols, rows);
        if let Some(ssh) = &self.ssh {
            ssh.resize(cols, rows);
        }
        cx.notify();
    }

    /// Ends the session. Safe to call on an already closed session.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(ssh) = self.ssh.take() {
            ssh.disconnect();
        }
        self._pump = None;
        if self.status.is_live() {
            self.status = SessionStatus::Disconnected {
                reason: "closed by the user".to_owned(),
            };
        }
        cx.notify();
    }

    /// Reopens the session with the same profile and credentials.
    ///
    /// The terminal is reset first so that the new shell starts on a clean
    /// screen rather than below the output of the previous one.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(ssh) = self.ssh.take() {
            ssh.disconnect();
        }
        self.terminal.reset();
        self.start(cx);
    }

    /// Opens a second, independent session to the same host as this one.
    ///
    /// Same principle as [`Session::reconnect`]: the credentials already in
    /// memory are reused, so a duplicate never asks for a password again. It
    /// lives here rather than in the caller for the reason given at the top of
    /// this module — [`Session::auth`] has no accessor, so the only place that
    /// can hand it to a new session is inside this type.
    ///
    /// The returned session is its own entity with its own transport, terminal
    /// and life cycle; nothing about it stays tied to this one. The current
    /// status is irrelevant, exactly as it is for a reconnect: duplicating a
    /// failed or disconnected session is how the user retries in a second pane
    /// while keeping the first one's error on screen.
    pub fn duplicate(&self, cx: &mut Context<Self>) -> Entity<Self> {
        cx.new(|cx| Self::new(self.profile.clone(), self.auth.clone(), cx))
    }

    /// How the session should be rendered in the tab strip.
    pub fn tab_status(&self) -> TabStatus {
        match self.status {
            SessionStatus::Connecting => TabStatus::Connecting,
            SessionStatus::Connected => TabStatus::Connected,
            SessionStatus::Disconnected { .. } => TabStatus::Disconnected,
            SessionStatus::Failed { .. } => TabStatus::Error,
        }
    }

    /// Opens the transport and spawns the event pump.
    ///
    /// Settings are read here rather than only in [`Session::new`] so that a
    /// reconnect naturally picks up a scheme, `TERM` or timeout changed since the
    /// session was first opened.
    fn start(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        let effective = settings.effective_terminal(&self.profile.overrides);
        // Re-applied here, not just in `new`, so a reconnect adopts a scheme the
        // user changed while the session was live.
        self.terminal
            .set_theme(TerminalTheme::by_name_or_default(&effective.scheme));

        let (cols, rows) = self.terminal.size();
        let mut config = SshConfig::new(
            self.profile.host.clone(),
            self.profile.port,
            self.profile.username.clone(),
            self.auth.clone(),
        );
        config.cols = cols;
        config.rows = rows;
        config.term = effective.term;
        config.keepalive_secs = settings.connection.keepalive_secs;
        config.connect_timeout_secs = settings.connection.connect_timeout_secs;

        let (ssh, mut events) = SshSession::connect(config, host_key_verifier());
        self.ssh = Some(ssh);
        self.status = SessionStatus::Connecting;
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let delivered = this.update(cx, |session, cx| session.on_ssh_event(event, cx));
                if delivered.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Applies one transport event to the session state.
    fn on_ssh_event(&mut self, event: SshEvent, cx: &mut Context<Self>) {
        match event {
            SshEvent::Connecting => self.status = SessionStatus::Connecting,
            SshEvent::HostKey {
                algorithm,
                fingerprint,
                accepted,
            } => {
                log::debug!(
                    "{}: host key {algorithm} {fingerprint} accepted={accepted}",
                    self.profile.label()
                );
            }
            SshEvent::Ready => {
                self.status = SessionStatus::Connected;
                let (cols, rows) = self.terminal.size();
                if let Some(ssh) = &self.ssh {
                    ssh.resize(cols, rows);
                }
            }
            SshEvent::Data(bytes) | SshEvent::ExtendedData(bytes) => {
                // A directory change needs no extra notification: this arm ends
                // in the `cx.notify` below on every chunk of output anyway, so
                // observers see the new `Session::cwd` on the next frame.
                let cwd_changed = self.terminal.feed(&bytes);
                if cwd_changed {
                    log::debug!(
                        "{}: remote cwd is now {:?}",
                        self.profile.label(),
                        self.terminal.cwd()
                    );
                }
                self.flush_terminal_replies();
            }
            SshEvent::ExitStatus(code) => {
                log::debug!("{}: remote shell exited with {code}", self.profile.label());
            }
            SshEvent::Disconnected { reason } => {
                self.ssh = None;
                self.status = SessionStatus::Disconnected { reason };
            }
            SshEvent::Error(kind, message) => {
                self.ssh = None;
                self.status = SessionStatus::Failed { kind, message };
            }
        }
        cx.notify();
    }

    /// Writes any answer the terminal produced back to the remote side.
    ///
    /// Requests such as a Device Status Report (`CSI 6 n`) or a Device
    /// Attributes query (`CSI c`) block programs like vim and tmux until the
    /// reply arrives, so this must run after every [`TerminalModel::feed`].
    fn flush_terminal_replies(&mut self) {
        let reply = self.terminal.take_pty_output();
        if reply.is_empty() {
            return;
        }
        if let Some(ssh) = &self.ssh {
            ssh.send_input(reply);
        }
    }
}
