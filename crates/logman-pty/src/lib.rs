//! Local pty transport for logman.
//!
//! A shell on this machine is reached the same way a remote one is:
//! [`PtySession::spawn`] hands back a handle plus a stream of [`PtyEvent`]s,
//! and every blocking operation lives on threads this crate owns, so a GUI
//! thread can hold the handle and never wait on it.
//!
//! ```no_run
//! # #[cfg(unix)]
//! # fn demo() {
//! use logman_pty::{PtyConfig, PtyEvent, PtySession};
//!
//! let (session, mut events) = PtySession::spawn(PtyConfig::new(80, 24));
//!
//! session.send_input(b"uptime\n".to_vec());
//! while let Ok(event) = events.try_recv() {
//!     if let PtyEvent::Data(bytes) = event {
//!         print!("{}", String::from_utf8_lossy(&bytes));
//!     }
//! }
//! session.shutdown();
//! # }
//! ```
//!
//! The pty comes from `alacritty_terminal::tty` rather than from a second
//! implementation of the same thing: that module already handles `openpty`,
//! `setsid`, handing the slave over as the controlling terminal, and the macOS
//! detour through `/usr/bin/login` that makes the shell a genuine login
//! session. It ships unconditionally with the terminal emulator this workspace
//! already uses, so reusing it costs nothing.
//!
//! This crate is unix-only. On other platforms it deliberately compiles to
//! nothing at all instead of being left out of the workspace, so that a
//! Windows build stays green while the application gates its local-shell
//! feature on `cfg(unix)`; a ConPTY backend would be a separate piece of work.

#![warn(missing_docs)]

#[cfg(unix)]
mod config;
#[cfg(unix)]
mod event;
#[cfg(unix)]
mod session;
#[cfg(unix)]
mod shell;

#[cfg(unix)]
pub use config::{DEFAULT_TERM, PtyConfig};
#[cfg(unix)]
pub use event::PtyEvent;
#[cfg(unix)]
pub use session::PtySession;
#[cfg(unix)]
pub use shell::login_shell_name;
