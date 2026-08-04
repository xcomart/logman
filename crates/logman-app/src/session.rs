//! One logman session: a transport bound to a terminal emulator.
//!
//! A [`Session`] is a gpui entity. Creating one immediately starts connecting
//! and spawns a pump that drains transport events onto the UI thread, so the
//! whole type is single threaded and never blocks a render.
//!
//! Two transports can drive it: an SSH connection to a remote host, and — on
//! unix only — a login shell on this machine. They are deliberately one type
//! rather than two: every tab, pane and view in the shell is written against
//! `Entity<Session>`, so a second session type would have to be threaded
//! through all of them. What differs between the two lives in the private
//! [`Target`] and [`Transport`] enums instead, and the public surface answers
//! for both.
//!
//! Credentials are kept for reconnection but are deliberately unreachable from
//! the outside: there is no accessor for them, and the hand written
//! [`Debug`](std::fmt::Debug) implementation omits them entirely.

use std::fmt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use gpui::{App, AppContext, Context, Entity, SharedString, Task};
use logman_core::{EffectiveTerminal, SessionOverrides, SessionProfile};
#[cfg(unix)]
use logman_pty::{PtyConfig, PtyEvent, PtySession, login_shell_name};
use logman_ssh::{SshAuth, SshConfig, SshEvent, SshSession};
use logman_term::{TerminalModel, TerminalTheme};

use crate::app_settings;
#[cfg(unix)]
use crate::files::LocalSource;
use crate::files::{FileSource, SftpSource};
use crate::i18n::ts;
use crate::ui::TabStatus;
use crate::verifier::host_key_verifier;

/// Columns a session starts with, until the view reports its real size.
const INITIAL_COLS: u16 = 80;

/// Rows a session starts with, until the view reports its real size.
const INITIAL_ROWS: u16 = 24;

/// Why a local session ended, when the shell simply exited.
///
/// English, like every other `reason` reaching [`SessionStatus`]: those come
/// from the SSH layer verbatim, and the wording around them is what the locale
/// translates.
#[cfg(unix)]
const LOCAL_EXIT_REASON: &str = "the local shell exited";

/// Classification put on a [`SessionStatus::Failed`] raised by the local pty.
///
/// The SSH kinds name the *stage* that failed because a remote connection has
/// several of them; starting a local shell has one, so this names the subsystem
/// and leaves what went wrong entirely to the transport's own message.
#[cfg(unix)]
const LOCAL_FAILURE_KIND: &str = "local shell";

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
        ///
        /// A [`SharedString`] rather than an
        /// [`SshErrorKind`](logman_ssh::SshErrorKind) because a local shell has
        /// no SSH failure to classify; the SSH path fills it in from its own
        /// kind's [`Display`](std::fmt::Display).
        kind: SharedString,
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
    /// The `reason`, `kind` and `message` inside come from the transport and
    /// stay in English; only the wording around them follows the locale.
    pub fn summary(&self) -> SharedString {
        match self {
            Self::Connecting => ts!("session.connecting"),
            Self::Connected => ts!("session.connected"),
            Self::Disconnected { reason } => ts!("session.disconnected", reason = reason),
            Self::Failed { kind, message } => {
                ts!("session.failed", kind = kind, message = message)
            }
        }
    }

    /// Whether the session can currently accept input.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Connecting | Self::Connected)
    }

    /// How a session in this state should be rendered in the tab strip.
    ///
    /// Lives on the status rather than only on [`Session`] so that the mapping
    /// can be asserted without standing an entity — and a gpui app — up first.
    pub fn tab_status(&self) -> TabStatus {
        match self {
            Self::Connecting => TabStatus::Connecting,
            Self::Connected => TabStatus::Connected,
            Self::Disconnected { .. } => TabStatus::Disconnected,
            Self::Failed { .. } => TabStatus::Error,
        }
    }
}

/// What a [`Session`] is attached to, and everything needed to attach again.
///
/// Held for the whole life of the session, transport or no transport: a
/// reconnect rebuilds the transport out of exactly this.
enum Target {
    /// A remote host reached over SSH.
    Ssh {
        /// Profile the session was opened from.
        profile: SessionProfile,
        /// Credentials, retained so that [`Session::reconnect`] can reuse them.
        ///
        /// Never rendered, logged or otherwise exposed.
        auth: SshAuth,
    },
    /// The user's login shell on this machine.
    #[cfg(unix)]
    Local {
        /// Name of that shell, resolved once when the session is created.
        ///
        /// Cached rather than looked up per frame: it cannot change under a
        /// running session, and the lookup reads the passwd database.
        shell: SharedString,
        /// Directory the shell is started in; `None` means the app's own.
        ///
        /// Set by [`Session::duplicate`] so that a split of a local pane opens
        /// where the original shell is, and kept so that a restart lands in the
        /// same place the session originally started in.
        cwd: Option<PathBuf>,
    },
}

/// A live transport handle.
///
/// Both variants are fire-and-forget channels into threads owned by the
/// transport crates, so every method here is non-blocking.
enum Transport {
    /// A connected — or still connecting — SSH session.
    Ssh(SshSession),
    /// A local shell on its own pty.
    #[cfg(unix)]
    Local(PtySession),
}

impl Transport {
    /// Sends already encoded bytes to the shell on the other end.
    fn send_input(&self, bytes: Vec<u8>) {
        match self {
            Self::Ssh(ssh) => ssh.send_input(bytes),
            #[cfg(unix)]
            Self::Local(pty) => pty.send_input(bytes),
        }
    }

    /// Tells the shell that the terminal has been resized.
    fn resize(&self, cols: u16, rows: u16) {
        match self {
            Self::Ssh(ssh) => ssh.resize(cols, rows),
            #[cfg(unix)]
            Self::Local(pty) => pty.resize(cols, rows),
        }
    }

    /// Ends the session behind this handle.
    ///
    /// Takes `self` because a closed transport must not be reachable
    /// afterwards; the caller always reaches this through an `Option::take`.
    fn close(self) {
        match self {
            Self::Ssh(ssh) => ssh.disconnect(),
            #[cfg(unix)]
            Self::Local(pty) => pty.shutdown(),
        }
    }
}

/// A single session — remote or local — together with the terminal it drives.
pub struct Session {
    /// What the session connects to, and what a reconnect rebuilds from.
    target: Target,
    /// Per-session settings overrides layered on top of the global ones.
    ///
    /// Copied out of the profile for an SSH session and left at the defaults
    /// for a local one, which is not saved anywhere and so has nothing to
    /// override from.
    overrides: SessionOverrides,
    /// Live transport handle; `None` once the session has ended.
    transport: Option<Transport>,
    /// Screen contents and scrollback.
    terminal: TerminalModel,
    /// Current life cycle state.
    status: SessionStatus,
    /// Task draining the transport's event stream; dropping it stops the pump.
    _pump: Option<Task<()>>,
}

impl fmt::Debug for Session {
    /// Written by hand so that the credentials in [`Target::Ssh`] can never
    /// reach a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("target", &self.label())
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
        let overrides = profile.overrides.clone();
        let mut session = Self::build(Target::Ssh { profile, auth }, overrides, cx);
        session.start(cx);
        session
    }

    /// Builds a session running the user's login shell on this machine, and
    /// starts it straight away.
    ///
    /// There is nothing to configure: the shell is whatever `$SHELL` — or the
    /// passwd entry — says, and everything else comes from the global terminal
    /// settings, since a local session is not saved and so carries no overrides
    /// of its own.
    #[cfg(unix)]
    pub fn new_local(cx: &mut Context<Self>) -> Self {
        Self::new_local_in(None, cx)
    }

    /// [`Session::new_local`], starting the shell in `cwd`.
    #[cfg(unix)]
    fn new_local_in(cwd: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let target = Target::Local {
            shell: SharedString::from(login_shell_name()),
            cwd,
        };
        let mut session = Self::build(target, SessionOverrides::default(), cx);
        session.start(cx);
        session
    }

    /// The common part of both constructors: a session with a terminal built
    /// from the effective settings, but no transport yet.
    fn build(target: Target, overrides: SessionOverrides, cx: &mut Context<Self>) -> Self {
        let effective = app_settings::current(cx).effective_terminal(&overrides);
        Self {
            target,
            overrides,
            transport: None,
            terminal: TerminalModel::new(
                INITIAL_COLS,
                INITIAL_ROWS,
                effective.scrollback_lines,
                TerminalTheme::by_name_or_default(&effective.scheme),
            ),
            status: SessionStatus::Connecting,
            _pump: None,
        }
    }

    /// The effective terminal settings for this session: the global defaults
    /// with this session's overrides layered on top.
    ///
    /// Exposed so the view can honor per-session values such as the font size
    /// and the copy-on-select behaviour without re-reading the global settings.
    pub fn effective(&self, cx: &App) -> EffectiveTerminal {
        app_settings::current(cx).effective_terminal(&self.overrides)
    }

    /// Re-reads the settings and applies the ones that can change on a live
    /// session.
    ///
    /// Only the color scheme takes effect immediately. The scrollback depth is
    /// fixed when the terminal model is built — changing it would rebuild the
    /// grid and clear the screen — and the `TERM` value has already been
    /// negotiated with the pty, so both are picked up only on the next
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

    /// What this session is attached to, in one line: `user@host` for an SSH
    /// session, the name of the shell for a local one.
    ///
    /// This is what the status bar and the connection overlay identify a
    /// session by, so that neither has to know which transport it is looking at.
    pub fn label(&self) -> SharedString {
        match &self.target {
            Target::Ssh { profile, .. } => SharedString::from(profile.label()),
            #[cfg(unix)]
            Target::Local { shell, .. } => shell.clone(),
        }
    }

    /// Whether this session runs a shell on this machine rather than a remote
    /// one.
    ///
    /// The views use it to word themselves for what the user is actually
    /// looking at — there is no host to connect to and nothing to reconnect to.
    pub fn is_local(&self) -> bool {
        match &self.target {
            Target::Ssh { .. } => false,
            #[cfg(unix)]
            Target::Local { .. } => true,
        }
    }

    /// The title to show in the tab: the `OSC 0` / `OSC 2` title when the shell
    /// set one, the profile name — or, locally, the shell's name — otherwise.
    pub fn title(&self) -> SharedString {
        match self.terminal.title() {
            Some(title) if !title.trim().is_empty() => SharedString::from(title.to_owned()),
            _ => match &self.target {
                Target::Ssh { profile, .. } => SharedString::from(profile.name.clone()),
                #[cfg(unix)]
                Target::Local { shell, .. } => shell.clone(),
            },
        }
    }

    /// The working directory of the shell, if it announced one.
    ///
    /// Fed by the `OSC 7` / `OSC 1337` sequences a configured prompt emits, so
    /// it stays `None` for shells that do not report their directory. The value
    /// survives a disconnect - the last known directory is still the one the
    /// session ended in - and is cleared by [`Session::reconnect`], because the
    /// new shell has not reported anything yet.
    pub fn cwd(&self) -> Option<&str> {
        self.terminal.cwd()
    }

    /// The filesystem this session can browse, or `None` while it is not
    /// carrying a live shell to browse one over.
    ///
    /// An SSH session browses the server over SFTP; a local one browses this
    /// computer. Which of the two the caller gets is the only thing that differs
    /// — both are [`FileSource`]s, and the file panel above is written against
    /// the trait and never asks.
    ///
    /// Both arms are gated on [`SessionStatus::Connected`], and for the same
    /// reason rather than for two: **the panel shows what the session is
    /// looking at, and until a shell is running there is nothing it is looking
    /// at.** Remotely that is also a practical matter — during `Connecting` the
    /// handle is already there and the SFTP service would queue requests behind
    /// the authentication, leaving the panel on a pending listing with nothing
    /// to show for it. Locally the filesystem would answer perfectly well a
    /// moment early, and it is still the wrong answer: a pane that has not
    /// started its shell yet is drawn as *starting*, and a file panel listing a
    /// directory beside it would say the session was further along than it is.
    /// Once a session ends, both sources are gone with the transport.
    ///
    /// Cheap on both sides: the SFTP source only clones a request channel — the
    /// channel itself is opened lazily on the first request and then reused —
    /// and the local one only clones the executor handle it does its blocking
    /// work on.
    pub fn files(&self, cx: &App) -> Option<Arc<dyn FileSource>> {
        // Read only by the local arm below, which does not exist off unix: a
        // build with no pty has no local session to browse from.
        #[cfg(not(unix))]
        let _ = cx;
        match (&self.status, &self.transport) {
            (SessionStatus::Connected, Some(Transport::Ssh(ssh))) => {
                Some(Arc::new(SftpSource::new(ssh.sftp())))
            }
            // The pty handle itself is not needed: the shell's filesystem is
            // this process's filesystem, reachable without going through it.
            // What the session decides is *whether* there is one to browse.
            #[cfg(unix)]
            (SessionStatus::Connected, Some(Transport::Local(_))) => {
                Some(Arc::new(LocalSource::new(cx.background_executor().clone())))
            }
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

    /// Sends already encoded key or paste bytes to the shell.
    ///
    /// Typing always snaps the viewport back to the bottom of the scrollback,
    /// which is what every other terminal does.
    pub fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        self.terminal.scroll_to_bottom();
        if let Some(transport) = &self.transport {
            transport.send_input(bytes);
        }
        cx.notify();
    }

    /// Resizes the terminal and tells the pty about it.
    ///
    /// A resize to the current size is ignored, so callers may invoke this on
    /// every layout pass without flooding the transport with window change
    /// requests.
    pub fn resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if self.terminal.size() == (cols, rows) {
            return;
        }

        self.terminal.resize(cols, rows);
        if let Some(transport) = &self.transport {
            transport.resize(cols, rows);
        }
        cx.notify();
    }

    /// Ends the session. Safe to call on an already closed session.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(transport) = self.transport.take() {
            transport.close();
        }
        self._pump = None;
        if self.status.is_live() {
            self.status = SessionStatus::Disconnected {
                reason: "closed by the user".to_owned(),
            };
        }
        cx.notify();
    }

    /// Reopens the session: reconnects to the same host with the same
    /// credentials, or — locally — starts the login shell again.
    ///
    /// The terminal is reset first so that the new shell starts on a clean
    /// screen rather than below the output of the previous one.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(transport) = self.transport.take() {
            transport.close();
        }
        self.terminal.reset();
        self.start(cx);
    }

    /// Opens a second, independent session onto the same target as this one.
    ///
    /// Same principle as [`Session::reconnect`]: the credentials already in
    /// memory are reused, so a duplicate of an SSH session never asks for a
    /// password again. It lives here rather than in the caller for the reason
    /// given at the top of this module — the credentials have no accessor, so
    /// the only place that can hand them to a new session is inside this type.
    ///
    /// A duplicate of a local session starts in the directory the original
    /// shell is in, when it reports one that still exists; splitting a terminal
    /// and landing in the same place is what every other terminal does.
    ///
    /// The returned session is its own entity with its own transport, terminal
    /// and life cycle; nothing about it stays tied to this one. The current
    /// status is irrelevant, exactly as it is for a reconnect: duplicating a
    /// failed or disconnected session is how the user retries in a second pane
    /// while keeping the first one's error on screen.
    pub fn duplicate(&self, cx: &mut Context<Self>) -> Entity<Self> {
        match &self.target {
            Target::Ssh { profile, auth } => {
                let (profile, auth) = (profile.clone(), auth.clone());
                cx.new(|cx| Self::new(profile, auth, cx))
            }
            #[cfg(unix)]
            Target::Local { .. } => {
                let cwd = self.local_start_dir();
                cx.new(|cx| Self::new_local_in(cwd, cx))
            }
        }
    }

    /// How the session should be rendered in the tab strip.
    pub fn tab_status(&self) -> TabStatus {
        self.status.tab_status()
    }

    /// The directory a duplicate of this local session should start in.
    ///
    /// Only a directory that exists right now is worth passing on: the shell's
    /// report comes from an `OSC 7` sequence, which can name a directory that
    /// has since been removed, or — if the prompt is misconfigured — something
    /// that is not a path at all. A pty that cannot enter its working directory
    /// fails to start, so anything doubtful falls back to `None` and lets the
    /// new shell open wherever the application itself is.
    #[cfg(unix)]
    fn local_start_dir(&self) -> Option<PathBuf> {
        let cwd = self.cwd()?;
        let path = Path::new(cwd);
        // Relative is a sign the report was not a path at all: `OSC 7` carries
        // an absolute one, and resolving it against *our* directory would send
        // the new shell somewhere the user never was.
        if !path.is_absolute() || !path.is_dir() {
            return None;
        }
        Some(path.to_path_buf())
    }

    /// Opens the transport and spawns the event pump.
    ///
    /// Settings are read here rather than only in the constructor so that a
    /// reconnect naturally picks up a scheme, `TERM` or timeout changed since the
    /// session was first opened.
    fn start(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        let effective = settings.effective_terminal(&self.overrides);
        // Re-applied here, not just on construction, so a reconnect adopts a
        // scheme the user changed while the session was live.
        self.terminal
            .set_theme(TerminalTheme::by_name_or_default(&effective.scheme));

        let (cols, rows) = self.terminal.size();
        // The transport and its pump are built first and installed afterwards:
        // each arm reads out of `self.target`, which rules out touching the
        // fields below in the same breath.
        let (transport, pump) = match &self.target {
            Target::Ssh { profile, auth } => {
                let mut config = SshConfig::new(
                    profile.host.clone(),
                    profile.port,
                    profile.username.clone(),
                    auth.clone(),
                );
                config.cols = cols;
                config.rows = rows;
                config.term = effective.term;
                config.keepalive_secs = settings.connection.keepalive_secs;
                config.connect_timeout_secs = settings.connection.connect_timeout_secs;

                let (ssh, mut events) = SshSession::connect(config, host_key_verifier());
                let pump = cx.spawn(async move |this, cx| {
                    while let Some(event) = events.next().await {
                        let delivered =
                            this.update(cx, |session, cx| session.on_ssh_event(event, cx));
                        if delivered.is_err() {
                            break;
                        }
                    }
                });
                (Transport::Ssh(ssh), pump)
            }
            #[cfg(unix)]
            Target::Local { cwd, .. } => {
                let mut config = PtyConfig::new(cols, rows);
                config.term = effective.term;
                config.cwd = cwd.clone();

                let (pty, mut events) = PtySession::spawn(config);
                let pump = cx.spawn(async move |this, cx| {
                    while let Some(event) = events.next().await {
                        let delivered =
                            this.update(cx, |session, cx| session.on_pty_event(event, cx));
                        if delivered.is_err() {
                            break;
                        }
                    }
                });
                (Transport::Local(pty), pump)
            }
        };

        self.transport = Some(transport);
        self.status = SessionStatus::Connecting;
        self._pump = Some(pump);
        cx.notify();
    }

    /// Applies one SSH transport event to the session state.
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
                    self.label()
                );
            }
            SshEvent::Ready => self.on_transport_ready(),
            SshEvent::Data(bytes) | SshEvent::ExtendedData(bytes) => self.on_output(&bytes),
            SshEvent::ExitStatus(code) => {
                log::debug!("{}: remote shell exited with {code}", self.label());
            }
            SshEvent::Disconnected { reason } => {
                self.transport = None;
                self.status = SessionStatus::Disconnected { reason };
            }
            SshEvent::Error(kind, message) => {
                self.transport = None;
                self.status = SessionStatus::Failed {
                    kind: SharedString::from(kind.to_string()),
                    message,
                };
            }
        }
        cx.notify();
    }

    /// Applies one local pty event to the session state.
    ///
    /// A shell that exits is a plain disconnect rather than a failure — the
    /// user typed `exit` — and the shell that could not be started at all is
    /// the only thing the pty layer reports as an error.
    #[cfg(unix)]
    fn on_pty_event(&mut self, event: PtyEvent, cx: &mut Context<Self>) {
        match event {
            PtyEvent::Ready => self.on_transport_ready(),
            PtyEvent::Data(bytes) => self.on_output(&bytes),
            PtyEvent::Exited => {
                self.transport = None;
                self.status = SessionStatus::Disconnected {
                    reason: LOCAL_EXIT_REASON.to_owned(),
                };
            }
            PtyEvent::Error(message) => {
                self.transport = None;
                self.status = SessionStatus::Failed {
                    kind: SharedString::new_static(LOCAL_FAILURE_KIND),
                    message,
                };
            }
        }
        cx.notify();
    }

    /// The transport reports a shell on the other end.
    ///
    /// The size is pushed again because the terminal has almost certainly been
    /// laid out — and resized — while the transport was still coming up, and
    /// that resize reached a transport that had no pty yet.
    fn on_transport_ready(&mut self) {
        self.status = SessionStatus::Connected;
        let (cols, rows) = self.terminal.size();
        if let Some(transport) = &self.transport {
            transport.resize(cols, rows);
        }
    }

    /// Feeds one chunk of shell output to the emulator.
    ///
    /// A directory change needs no extra notification: both callers end in a
    /// `cx.notify` on every chunk of output anyway, so observers see the new
    /// [`Session::cwd`] on the next frame.
    fn on_output(&mut self, bytes: &[u8]) {
        let cwd_changed = self.terminal.feed(bytes);
        if cwd_changed {
            log::debug!("{}: cwd is now {:?}", self.label(), self.terminal.cwd());
        }
        self.flush_terminal_replies();
    }

    /// Writes any answer the terminal produced back to the shell.
    ///
    /// Requests such as a Device Status Report (`CSI 6 n`) or a Device
    /// Attributes query (`CSI c`) block programs like vim and tmux until the
    /// reply arrives, so this must run after every [`TerminalModel::feed`] —
    /// on a local pty just as much as on an SSH channel.
    fn flush_terminal_replies(&mut self) {
        let reply = self.terminal.take_pty_output();
        if reply.is_empty() {
            return;
        }
        if let Some(transport) = &self.transport {
            transport.send_input(reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_starting_or_running_session_is_live() {
        assert!(SessionStatus::Connecting.is_live());
        assert!(SessionStatus::Connected.is_live());
        assert!(
            !SessionStatus::Disconnected {
                reason: "closed by the user".to_owned()
            }
            .is_live()
        );
        assert!(
            !SessionStatus::Failed {
                kind: "authentication failed".into(),
                message: "wrong password".to_owned()
            }
            .is_live()
        );
    }

    #[test]
    fn every_status_maps_to_its_own_tab_marker() {
        assert_eq!(
            SessionStatus::Connecting.tab_status(),
            TabStatus::Connecting
        );
        assert_eq!(SessionStatus::Connected.tab_status(), TabStatus::Connected);
        assert_eq!(
            SessionStatus::Disconnected {
                reason: "the local shell exited".to_owned()
            }
            .tab_status(),
            TabStatus::Disconnected
        );
        assert_eq!(
            SessionStatus::Failed {
                kind: "local shell".into(),
                message: "could not start the local shell".to_owned()
            }
            .tab_status(),
            TabStatus::Error
        );
    }

    #[test]
    fn a_summary_carries_the_transports_own_words() {
        // The wording around them follows the locale, so only the parts that
        // come from the transport verbatim can be asserted on.
        let disconnected = SessionStatus::Disconnected {
            reason: "the local shell exited".to_owned(),
        };
        assert!(
            disconnected.summary().contains("the local shell exited"),
            "{}",
            disconnected.summary()
        );

        let failed = SessionStatus::Failed {
            kind: "local shell".into(),
            message: "could not start the local shell: No such file".to_owned(),
        };
        let summary = failed.summary();
        assert!(summary.contains("local shell"), "{summary}");
        assert!(summary.contains("No such file"), "{summary}");
    }

    #[test]
    fn a_status_with_nothing_to_quote_still_summarises() {
        // Neither of these interpolates anything, so an empty answer would mean
        // the key went missing rather than that there was nothing to say.
        assert!(!SessionStatus::Connecting.summary().is_empty());
        assert!(!SessionStatus::Connected.summary().is_empty());
    }
}
