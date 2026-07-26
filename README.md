# logman

A multi-platform GUI SSH terminal written in Rust, built on
[gpui](https://gpui.rs) — the GPU-accelerated UI framework behind the Zed editor.

- **Multiple sessions** in one window, as tabs, each with its own connection and
  scrollback.
- **Password and private key authentication**, with secrets kept in the OS
  keychain rather than on disk.
- **A real terminal**, not a log view: `alacritty_terminal` drives the emulation,
  so colors, cursor addressing, alternate screen and full-screen programs behave
  the way they do in any other terminal.
- **Trust on first use** host key checking, backed by a `known_hosts` file.

## Building

Requires a Rust toolchain (edition 2024, so 1.85 or newer) and a platform
compiler toolchain — MSVC on Windows, Xcode command line tools on macOS, a C
compiler and the usual X11/Wayland development packages on Linux.

```bash
cargo run --release -p logman-app
```

The SSH layer deliberately uses russh's `ring` backend instead of the default
`aws-lc-rs`. `aws-lc-rs` needs NASM to build on Windows, which would make a
clean checkout fail to compile there; `ring` builds everywhere with no extra
tooling. Do not re-enable russh's default features.

### gpui is vendored and patched

`vendor/gpui` is gpui 0.2.2 with a three-line fix, wired in through
`[patch.crates-io]`. Upstream's Windows message pump calls
`DispatchMessageW` without `TranslateMessage` and compensates by calling
`TranslateMessage` re-entrantly from inside the window procedure. TSF correlates
translated keys against the message queue, so ending a Korean composition with
the Han/Yeong key leaves CTF regenerating `WM_IME_COMPOSITION` forever and the
process pinned at 100% CPU.

The vendored source is otherwise identical to the published crate, so
`diff -r` against the registry copy shows exactly the patched files. Drop
`vendor/gpui`, the `[patch.crates-io]` entry and the `exclude` line once a
released gpui carries the fix — nothing under `crates/` depends on the patch.

### Release builds on Windows need `fxc.exe`

gpui precompiles its HLSL shaders only in release builds — debug builds compile
them at runtime — so `cargo build --release` needs `fxc.exe` from the Windows
SDK. gpui looks on `PATH` and then at one hardcoded location under
`C:\Program Files (x86)`, so an SDK installed anywhere else fails the build with
`Failed to find fxc.exe`. Point it at the right file:

```powershell
$env:GPUI_FXC_PATH = "D:\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe"
cargo build --release
```

Debug builds do not need it.

## Using it

Press <kbd>Ctrl</kbd>+<kbd>T</kbd> (<kbd>Cmd</kbd>+<kbd>T</kbd> on macOS) or
click **New session** to open the connection dialog. Fill in the host and user,
pick an authentication method, and connect. The profile is saved automatically,
so the next connection is one click from the empty-state screen.

### Shortcuts

| Key | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | New session |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close the active tab |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>9</kbd> | Switch to tab *n* |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy the selection |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> | Paste |
| <kbd>Esc</kbd> | Dismiss the connection dialog |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

On macOS every <kbd>Ctrl</kbd> above is <kbd>Cmd</kbd>, and copy/paste are plain
<kbd>Cmd</kbd>+<kbd>C</kbd>/<kbd>V</kbd>.

Select text by dragging across the grid; scroll back with the mouse wheel.

### Where things are stored

| | |
| --- | --- |
| Windows | `%APPDATA%\logman\logman\config\` |
| macOS | `~/Library/Application Support/dev.logman.logman/` |
| Linux | `~/.config/logman/` |

`profiles.json` holds saved connections and `known_hosts` the trusted host key
fingerprints. Both are plain text and safe to edit by hand. **Passwords and key
passphrases are never written to either file** — they go to the Windows
Credential Manager, the macOS Keychain, or the freedesktop Secret Service, and
only when "Remember … in the system keychain" is ticked. Without a usable
keychain the application still runs; it just asks for the secret every time.

### Host key policy

The first time a host is seen its key is recorded and trusted. On later
connections a **changed** fingerprint aborts the connection rather than
prompting, and logs both the stored and the presented fingerprint. If a server
was legitimately rebuilt, remove its line from `known_hosts` to trust the new
key.

Keys are recorded per host, port *and* algorithm, matching OpenSSH: a server may
legitimately offer both an Ed25519 and an RSA host key.

## How it is put together

| Crate | Responsibility |
| --- | --- |
| `logman-core` | Profiles, OS keychain, `known_hosts`, config paths. No SSH, no GUI. |
| `logman-ssh` | russh client: authentication, pty, shell, resize. Owns its own thread and Tokio runtime. |
| `logman-term` | `alacritty_terminal` wrapper: byte stream in, styled snapshot out; key encoding. No GUI. |
| `logman-app` | The gpui binary: widgets, terminal rendering, session management. |

Two boundaries are worth knowing about.

**SSH never blocks the UI.** Each session owns a dedicated thread running its own
Tokio runtime, and talks to the GUI only over channels. A hung network read
cannot stall a repaint.

**The terminal model knows nothing about gpui, and the GUI knows nothing about
russh.** `logman-term` turns bytes into a `TerminalSnapshot` of styled runs, and
that is all the renderer sees. Both lower crates are testable without a window:
of the 105 tests in the workspace, 68 need neither a GUI nor a network, and the
rest need only loopback.

### Testing

```bash
cargo test --workspace
```

`logman-ssh` is tested against a real SSH server: the integration suite starts an
in-process russh server on an ephemeral port with a freshly generated host key
and drives the actual client against it — password and public key
authentication (including an encrypted key), pty parameters, data round-trip,
`window-change`, host key rejection, and teardown. No fixture keys are committed
and no external server is needed.

## Limitations

- **No SSH agent support.** The dialog offers the option but disables Connect
  and says so; it is not silently ignored.
- **No keyboard-interactive authentication**, so MFA-protected servers cannot be
  reached yet.
- **IME support depends on the vendored gpui patch** described above. Building
  against an unpatched gpui 0.2.2 on Windows will hang the process the first
  time a Korean composition is ended with the Han/Yeong key.
- **IME composition is only verified on Windows.** Text input goes through
  gpui's `EntityInputHandler`, so composing Korean or Japanese in a session
  works — the preedit is drawn at the cursor and nothing reaches the remote
  until it is committed — but only the Microsoft Korean IME has actually been
  exercised. Under it, <kbd>Esc</kbd> during composition *commits* the syllable
  and then leaves insert mode, which is the IME's own behavior rather than
  something logman chooses.
- <kbd>Ctrl</kbd>+<kbd>T</kbd> and <kbd>Ctrl</kbd>+<kbd>W</kbd> are taken by the
  application, so the remote shell never sees them.
- **Runtime palette changes are ignored.** A program that redefines colors with
  `OSC 4` / `OSC 10-11` will render with the static theme.
- A selection is anchored to the viewport and is not re-anchored when the
  scrollback moves under it.
- There is no timeout on the pty and shell requests. A server that accepts the
  connection and then never answers leaves the session in *Connecting*; closing
  the tab cancels it.

## License

Apache-2.0.
