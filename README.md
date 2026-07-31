# <img src="assets/icon.svg" width="28" alt=""> logman

[![CI](https://github.com/xcomart/logman/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/xcomart/logman/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/xcomart/logman)](https://github.com/xcomart/logman/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational)

A multi-platform GUI SSH terminal written in Rust, built on
[gpui](https://gpui.rs) — the GPU-accelerated UI framework behind the Zed editor.

![logman with two sessions in split panes and the remote files panel, in the One Dark theme](docs/screenshots/main-dark.png)

<details>
<summary>The settings dialog: language, theme, live scheme previews, installed fonts</summary>

![The settings dialog](docs/screenshots/settings.png)

</details>

- **Multiple sessions** in one window, as tabs, each with its own connection and
  scrollback — and the tab strip doubles as the window's title bar, VS Code
  style, with the system caption one setting away for those who prefer it.
- **Split panes**: split a pane into a second connection to the same host with
  one shortcut, or pull an open tab in beside another, and work in both sessions
  at once, dragging the divider to give either as much room as it needs; a pane
  closes itself when its connection ends.
- **A remote files panel**: an SFTP browser beside the terminal that follows the
  shell's working directory, with a breadcrumb header whose every piece drops
  down the directories beside it, drag-and-drop upload and download — whole
  folders included, with a progress bar while they move — and a draggable edge.
- **Password and private key authentication**, with secrets kept in the OS
  keychain rather than on disk.
- **A real terminal**, not a log view: `alacritty_terminal` drives the emulation,
  so colors, cursor addressing, alternate screen and full-screen programs behave
  the way they do in any other terminal.
- **Your language and your font**: the interface ships in eight languages,
  follows the system locale, and the terminal font is picked from the fonts
  installed on the machine.
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

`vendor/gpui` is gpui 0.2.2 with a small set of local patches, wired in through
`[patch.crates-io]`:

- **Windows IME fix.** Upstream's message pump calls `DispatchMessageW` without
  `TranslateMessage` and compensates by calling `TranslateMessage` re-entrantly
  from inside the window procedure. TSF correlates translated keys against the
  message queue, so ending a Korean composition with the Han/Yeong key leaves
  CTF regenerating `WM_IME_COMPOSITION` forever and the process pinned at 100%
  CPU.
- **`Window::set_titlebar_transparent`.** Upstream only decides at window
  creation whether the platform caption exists; this API flips it on a live
  window, which is what lets the title bar setting apply without a restart.
- Smaller fixes: window background blur on macOS Tahoe, and explicit `f32`
  suffixes in `taffy.rs` float literals.

The vendored source is otherwise identical to the published crate, so
`diff -r` against the registry copy shows exactly the patched files. Retiring
the vendor needs a released gpui that carries the IME fix and a public way to
retheme the caption at runtime; until then the patches ride along here.

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

What follows is the short version; [docs/user-guide.md](docs/user-guide.md)
covers every screen, setting and shortcut in full.

Press <kbd>Ctrl</kbd>+<kbd>T</kbd> (<kbd>Cmd</kbd>+<kbd>T</kbd> on macOS) or
click **New session** to open the connection dialog. Fill in the host and user,
pick an authentication method, and connect. The profile is saved automatically,
so the next connection is one click from the empty-state screen.

### Splitting a tab

A tab shows one session per pane, and there are two ways to get a second one.

**A second connection to the same host.**
<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>D</kbd> splits the focused pane to the
right and <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> splits it downwards,
opening a fresh session to the same host in the new half — same profile, same
credentials, no dialog. The two sessions are independent from that moment on,
each with its own shell and scrollback. Both commands are also in the
application menu and in the menu a right-click on the active tab opens, and
both work even when the pane you are splitting has failed or disconnected.

**An existing tab, moved in.** Right-click the tab you want to move and pick
**Split right of current tab** or **Split below current tab**: that tab leaves
the strip and its sessions appear next to the pane you are looking at. There is
no shortcut for *this* one, because it has to say which tab to pull in.
Right-clicking the active tab offers the reverse, moving its focused pane back
out into a tab of its own.

When a connection ends — the remote shell exits, or the server hangs up — its
pane closes on its own: siblings grow into the space, a tab closes with its
last pane, and closing the last tab returns to the start screen. A session
that *failed* to connect stays visible instead, with the error and a
Reconnect button.

### Remote files panel

The sidebar to the left of the terminal is an SFTP browser for the session in
the focused pane. It rides on the same SSH connection over a channel of its
own, so listing a directory or copying a file never holds up the shell — and
the shell never holds up a transfer.

<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>B</kbd> (<kbd>Cmd</kbd>+<kbd>B</kbd> on
macOS) shows and hides it, as does the panel button left of the tab strip. It
is shown by default.

- Double-click a directory to enter it, or `..` to go up. Directories sort
  before files, ignoring case.
- **The header path is a breadcrumb**: pressing a piece of it lists the
  directories beside that one, and choosing a row goes there. A path too long
  for the panel's current width folds its front into a `…` piece which lists
  what it hid.
- **The toolbar** runs from the commands that need no selection to the ones that
  do: **⟳** lists the directory again, the folder-plus button creates one in it,
  **↑** uploads local files and the folder button beside it a whole folder,
  **↓** saves the selection — a file, several files, or an entire directory —
  locally, and the pencil and bin at the end rename and delete it. A button
  whose command does not apply to the current selection is dimmed.
- **Several rows can be selected at once**: <kbd>Ctrl</kbd>-click
  (<kbd>Cmd</kbd>-click on macOS) adds or removes one, <kbd>Shift</kbd>-click
  takes everything between it and the last row clicked.
- **Right-click a row** to download, rename or delete the selection; right-click
  empty space to create a folder or upload into the directory. Deleting asks
  first, and a symbolic link is removed as a link rather than followed to
  whatever it points at.
- **Dropping files or folders onto the panel uploads them** into the directory
  on screen. Folders are copied recursively; a symlinked *directory* is left
  out, because a tree that links back into itself would otherwise be walked
  forever. A symlinked file is sent as its target.
- **A progress bar along the bottom** names the file in flight and shows how far
  the whole batch has got. One transfer runs per session at a time; a second
  request while one is running is refused rather than queued.
- Each session keeps its own directory, selection and scroll position, so
  switching tabs or panes picks up where you left off.

**The panel follows the remote shell's `cd`, but only if the shell says so.**
Directory tracking is driven by the `OSC 7` escape sequence, which fish emits
out of the box. bash and zsh need one line — this, in `~/.bashrc`:

```bash
PROMPT_COMMAND='printf "\033]7;file://%s%s\033\\" "$HOSTNAME" "$PWD"'
```

or in `~/.zshrc`:

```zsh
precmd() { printf '\033]7;file://%s%s\033\\' "$HOST" "$PWD" }
```

Without it the panel simply starts in the login directory and stays wherever
you navigate it. Browsing by hand always wins until the shell announces a new
directory, at which point the panel follows again.

### Shortcuts

| Key | Action |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>T</kbd> | New session |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close the active pane, and the tab with its last one |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>9</kbd> | Switch to tab *n* |
| <kbd>Alt</kbd>+<kbd>]</kbd> / <kbd>Alt</kbd>+<kbd>[</kbd> | Next / previous pane of the tab |
| <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>D</kbd> | Split right, with a new connection to the same host |
| <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> | Split below, with a new connection to the same host |
| <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>B</kbd> | Move the active pane into its own tab |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>B</kbd> | Show or hide the remote files panel |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy the selection |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> | Paste |
| <kbd>Esc</kbd> | Dismiss the connection dialog |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

On macOS every <kbd>Ctrl</kbd> and <kbd>Alt</kbd> above is <kbd>Cmd</kbd>,
copy/paste are plain <kbd>Cmd</kbd>+<kbd>C</kbd>/<kbd>V</kbd>, and the files
panel is plain <kbd>Cmd</kbd>+<kbd>B</kbd>. The pane shortcuts use
<kbd>Alt</kbd> elsewhere because <kbd>Ctrl</kbd>+<kbd>[</kbd> is ESC to a remote
shell; the files panel takes the shifted chord for the same reason, since
<kbd>Ctrl</kbd>+<kbd>B</kbd> is tmux's prefix key. The split shortcuts are
shifted because bare <kbd>Alt</kbd>+<kbd>D</kbd> is readline's *kill-word*,
and a terminal cannot tell the shifted chord apart from it anyway — so taking
it costs the remote shell nothing.

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

### Settings

<kbd>Ctrl</kbd>+<kbd>,</kbd> (<kbd>Cmd</kbd>+<kbd>,</kbd> on macOS) opens the
settings dialog: interface language (eight are built in; the default follows
the system locale), UI theme, title bar style — logman's own tab-strip title
bar or the system caption, swapped on the open window as soon as the change is
saved — terminal color scheme (One Dark, One Light,
Solarized Dark/Light, Gruvbox Dark, Dracula — each shown with a live
preview), font family — picked from the fonts installed on the machine — and
size, scrollback depth, `TERM`, copy-on-select, window background opacity and
blur, and the defaults applied to new connections. Everything lands in `settings.json` next to the profiles, is
safe to edit by hand, and out-of-range values are clamped on load rather
than breaking the app.

A profile can override the scheme, font size, scrollback and `TERM` for just
that session — the "Session overrides" section of the connection dialog;
empty fields inherit the global value. Theme and scheme changes apply to open
sessions immediately; a changed `TERM` takes effect on the next reconnect,
since it has already been sent to the server; a changed scrollback applies to
sessions opened afterwards, since resizing a live terminal's scrollback would
rebuild the grid and clear the screen.

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
| `logman-ssh` | russh client: authentication, pty, shell, resize, and the SFTP channel behind the files panel. Owns its own thread and Tokio runtime. |
| `logman-term` | `alacritty_terminal` wrapper: byte stream in, styled snapshot out; key encoding. No GUI. |
| `logman-app` | The gpui binary: widgets, terminal rendering, session management. |

Two boundaries are worth knowing about.

**SSH never blocks the UI.** Each session owns a dedicated thread running its own
Tokio runtime, and talks to the GUI only over channels. A hung network read
cannot stall a repaint.

**The terminal model knows nothing about gpui, and the GUI knows nothing about
russh.** `logman-term` turns bytes into a `TerminalSnapshot` of styled runs, and
that is all the renderer sees. Both lower crates are testable without a window:
of the 247 tests in the workspace, 210 need neither a GUI nor a network, and the
rest need only loopback.

### Third-party libraries

The heavy lifting is done by these projects:

| Library | Role |
| --- | --- |
| [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) | GPU-accelerated UI framework, from the Zed editor (vendored 0.2.2, [patched](#gpui-is-vendored-and-patched)) |
| [russh](https://github.com/warp-tech/russh) | Pure-Rust SSH client: transport, authentication, pty and shell channels |
| [russh-sftp](https://github.com/AspectUnk/russh-sftp) | SFTP client for the remote files panel, on a channel of the same connection |
| [alacritty_terminal](https://github.com/alacritty/alacritty) | Terminal emulation: grid, VTE parsing, scrollback |
| [tokio](https://github.com/tokio-rs/tokio) | Async runtime for the SSH transport thread |
| [keyring](https://github.com/open-source-cooperative/keyring-rs) | OS credential store: Windows Credential Manager, macOS Keychain, Secret Service |
| [directories](https://github.com/soc/directories-rs) | Per-platform configuration paths, and the home directory the save dialog opens in |

Supporting crates:
[serde](https://github.com/serde-rs/serde) /
[serde_json](https://github.com/serde-rs/json) (profiles and settings),
[uuid](https://github.com/uuid-rs/uuid) (profile identity),
[anyhow](https://github.com/dtolnay/anyhow) /
[thiserror](https://github.com/dtolnay/thiserror) (errors),
[log](https://github.com/rust-lang/log) /
[env_logger](https://github.com/rust-cli/env_logger) (logging),
[futures](https://github.com/rust-lang/futures-rs) /
[async-trait](https://github.com/dtolnay/async-trait) (async glue),
[parking_lot](https://github.com/Amanieu/parking_lot),
[smallvec](https://github.com/servo/rust-smallvec),
[bitflags](https://github.com/bitflags/bitflags),
[unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation)
(grapheme-safe text editing).

Windows only: [windows-rs](https://github.com/microsoft/windows-rs) (DWM
caption colors), [raw-window-handle](https://github.com/rust-windowing/raw-window-handle)
(HWND access), [winresource](https://github.com/BenjaminRi/winresource)
(icon embedding). Tests additionally use
[tempfile](https://github.com/Stebalien/tempfile) and
[rand](https://github.com/rust-random/rand).

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
- <kbd>Ctrl</kbd>+<kbd>T</kbd>, <kbd>Ctrl</kbd>+<kbd>W</kbd> and the
  <kbd>Alt</kbd> pane shortcuts are taken by the application, so the remote
  shell never sees them.
- **The remote files panel has no way to change permissions or ownership.**
  Transfers and deletes run one at a time per session and cannot be cancelled
  once started. The panel's edge can be dragged, but the width is session state
  and reverts to the default on the next start.
- **Panes cannot be rearranged by dragging.** A divider drag changes the
  proportions of an existing split and nothing else — there is no way to move a
  pane to another position, and a split layout is not remembered across
  restarts. Every split starts out even.
- **Runtime palette changes are ignored.** A program that redefines colors with
  `OSC 4` / `OSC 10-11` will render with the static theme.
- A selection is anchored to the viewport and is not re-anchored when the
  scrollback moves under it.
- There is no timeout on the pty and shell requests. A server that accepts the
  connection and then never answers leaves the session in *Connecting*; closing
  the tab cancels it.

## License

Apache-2.0 — see [LICENSE](LICENSE). The vendored gpui keeps its own
Apache-2.0 notice under `vendor/gpui/`.
