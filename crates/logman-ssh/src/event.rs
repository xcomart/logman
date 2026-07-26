//! Events published by a running [`SshSession`](crate::SshSession).
//!
//! A session never blocks its caller: everything it learns about the remote
//! host arrives as an [`SshEvent`] on the receiver handed out by
//! [`SshSession::connect`](crate::SshSession::connect).

use std::fmt;

/// A single observation about the life of an SSH session.
///
/// The stream always ends with either [`SshEvent::Disconnected`] or
/// [`SshEvent::Error`]; no further events follow those.
#[derive(Clone)]
pub enum SshEvent {
    /// The transport is about to open a TCP connection to the server.
    Connecting,
    /// The server presented its host key and the verifier ruled on it.
    ///
    /// Emitted for display purposes even when the key was rejected, in which
    /// case an [`SshEvent::Error`] with
    /// [`SshErrorKind::HostKeyRejected`] follows.
    HostKey {
        /// SSH name of the key algorithm, e.g. `ssh-ed25519`.
        algorithm: String,
        /// OpenSSH-style SHA-256 fingerprint, e.g. `SHA256:...`.
        fingerprint: String,
        /// Whether the verifier accepted the key.
        accepted: bool,
    },
    /// Authentication succeeded and the remote pty and shell are live.
    ///
    /// The server has *confirmed* both the `pty-req` and the `shell` request
    /// before this is published, so a session that reaches `Ready` can be
    /// shown as connected: a later refusal of either cannot contradict it.
    Ready,
    /// Bytes read from the remote shell's standard output.
    Data(Vec<u8>),
    /// Bytes read from the remote shell's extended data stream (stderr).
    ExtendedData(Vec<u8>),
    /// The remote shell reported its exit status.
    ExitStatus(u32),
    /// The session finished. Covers both orderly and unexpected shutdowns
    /// that are not classified as errors.
    Disconnected {
        /// Human-readable explanation, suitable for display in the UI.
        reason: String,
    },
    /// The session failed and cannot continue.
    Error(SshErrorKind, String),
}

impl fmt::Debug for SshEvent {
    /// Summarises payload-carrying variants by length instead of dumping the
    /// bytes: terminal traffic is both noisy and potentially sensitive.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => f.write_str("Connecting"),
            Self::HostKey {
                algorithm,
                fingerprint,
                accepted,
            } => f
                .debug_struct("HostKey")
                .field("algorithm", algorithm)
                .field("fingerprint", fingerprint)
                .field("accepted", accepted)
                .finish(),
            Self::Ready => f.write_str("Ready"),
            Self::Data(bytes) => write!(f, "Data({} bytes)", bytes.len()),
            Self::ExtendedData(bytes) => write!(f, "ExtendedData({} bytes)", bytes.len()),
            Self::ExitStatus(code) => write!(f, "ExitStatus({code})"),
            Self::Disconnected { reason } => f
                .debug_struct("Disconnected")
                .field("reason", reason)
                .finish(),
            Self::Error(kind, message) => {
                f.debug_tuple("Error").field(kind).field(message).finish()
            }
        }
    }
}

/// Coarse classification of a fatal session failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshErrorKind {
    /// Name resolution, TCP connect, connect timeout, or protocol handshake
    /// failure — the session never reached authentication.
    Connect,
    /// The host key verifier refused the key the server presented.
    HostKeyRejected,
    /// The server rejected our credentials.
    Auth,
    /// A private key could not be read, parsed, or decrypted.
    KeyLoad,
    /// Opening the session channel, the pty, or the shell failed.
    Channel,
    /// Transport-level I/O failure, or an internal error while running the
    /// session.
    Io,
}

impl fmt::Display for SshErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Connect => "connection failed",
            Self::HostKeyRejected => "host key rejected",
            Self::Auth => "authentication failed",
            Self::KeyLoad => "private key could not be loaded",
            Self::Channel => "channel request failed",
            Self::Io => "i/o error",
        };
        f.write_str(text)
    }
}
