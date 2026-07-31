//! The remote file panel: an SFTP browser for the session in the active pane.
//!
//! The panel is a single entity owned by the workspace, not one per session.
//! What *is* per session is the browsing state — the directory being listed,
//! its entries, the selection, the scroll position — so switching tabs or panes
//! restores what that session was showing instead of asking the server again.
//! [`FilePanel::set_session`] is the only way in: the workspace calls it while
//! rendering, and a call naming the session already on screen is a no-op.
//!
//! Two things drive the panel, and they are deliberately allowed to disagree:
//!
//! * the **shell**, through [`Session::cwd`] — a prompt configured to emit
//!   `OSC 7` reports every `cd`, and the panel follows it;
//! * the **user**, through clicks in the list.
//!
//! Manual navigation wins until the shell moves again, at which point the panel
//! follows once more. That is the whole tracking rule; there is no "locked"
//! mode, because the next `cd` re-synchronises the two anyway.
//!
//! Every request is a [`cx.spawn`](gpui::Context::spawn) away, and the SFTP
//! futures are runtime-agnostic, so a transfer runs on gpui's own executor
//! without blocking a repaint. Replies are matched against a per-session
//! generation counter: clicking through three directories quickly leaves two
//! listings in flight whose answers must not overwrite the third.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, ClickEvent, Context, DragMoveEvent, ElementId, Entity, EntityId,
    ExternalPaths, PathPromptOptions, ScrollHandle, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use logman_ssh::{RemoteEntry, SftpClient, SftpError};

use crate::app_settings;
use crate::i18n::ts;
use crate::icons;
use crate::session::Session;
use crate::ui::{Theme, theme};

/// Width the panel opens at, in pixels.
///
/// Wide enough for a typical file name plus its size column; dragging the right
/// edge takes it from there.
const DEFAULT_PANEL_WIDTH: f32 = 260.;

/// Narrowest the panel may be dragged, in pixels.
///
/// Below this the header's path and the toolbar buttons start colliding, and a
/// panel too narrow to read is indistinguishable from one the user meant to
/// close — which the toggle already does, and reversibly.
const MIN_PANEL_WIDTH: f32 = 180.;

/// Widest the panel may be dragged, in pixels.
///
/// The panel is a sidebar next to the terminals, not a half of the window; the
/// cap is what stops a slipped drag from squeezing the panes down to nothing on
/// a small display.
const MAX_PANEL_WIDTH: f32 = 560.;

/// Width of the grab area along the panel's right edge, in pixels.
///
/// The edge itself is the panel's hairline border, far too thin to hit. The
/// handle is laid over it absolutely so that widening the grab area costs the
/// listing no room.
const PANEL_HANDLE: f32 = 6.;

/// Longest remote path the header shows in full, in characters.
///
/// Beyond this the *front* is dropped, not the tail: the leaf directory is what
/// identifies where you are, and it is the part a plain `truncate` would eat.
const PATH_CHARS: usize = 38;

/// Size of the icon leading a listing row, in pixels.
const ROW_ICON: f32 = 14.;

/// Size of the badge marking a symbolic link, in pixels.
const BADGE_ICON: f32 = 11.;

/// Size of a toolbar button's icon, in pixels.
const TOOLBAR_ICON: f32 = 15.;

/// Style group of one toolbar button, so hovering the button recolours the
/// icon inside it: an SVG takes its tint from its own `text_color`, which —
/// unlike a text glyph's — does not inherit from the button around it.
const BUTTON_GROUP: &str = "file-panel-button";

/// The row standing for the parent directory. Punctuation, never translated.
const PARENT_NAME: &str = "..";

/// The panel's right edge, while a drag is holding it.
///
/// Carries nothing: there is one panel and one edge, so the type alone says
/// what is being dragged. Being its own type is the point — it is what keeps
/// an edge drag from looking like the [`ExternalPaths`] drop the panel accepts,
/// since gpui routes both through the same drag machinery and tells them apart
/// by the payload's type.
struct DraggedPanelEdge;

/// Where one navigation gets its directory from.
enum Target {
    /// The login directory. Asked for once, when a session first appears in the
    /// panel and its shell has not reported a directory of its own.
    Home,
    /// A path to canonicalise before listing. This is how `..` is resolved:
    /// the server flattens `<current>/..` for us, so the panel never has to
    /// guess how the remote host spells a parent directory.
    Resolve(String),
    /// A path to list exactly as given.
    Exact(String),
}

/// The line along the bottom of the panel.
///
/// Cleared by the next successful listing, so a failure stays readable until
/// something works rather than until the next repaint.
enum Notice {
    /// Progress, in the panel's muted text color.
    Info(SharedString),
    /// A failure, in the danger color.
    Error(SharedString),
}

impl Notice {
    /// Wraps an SFTP failure in the localised sentence that frames it.
    ///
    /// The detail itself stays in English: it comes from the server or from the
    /// local filesystem, and translating half a sentence would only make it
    /// harder to search for.
    fn from_error(error: &SftpError) -> Self {
        Self::Error(ts!("files.failed", error = error.to_string()))
    }
}

/// What one session is looking at, kept while the session lives.
struct SessionState {
    /// Directory currently listed. `None` until the first listing lands.
    path: Option<String>,
    /// Entries of [`SessionState::path`], directories first and then by name.
    entries: Vec<RemoteEntry>,
    /// Name of the selected entry, if the selection still exists.
    selected: Option<String>,
    /// The shell directory the panel last followed.
    ///
    /// Compared against [`Session::cwd`] on every session notification; a
    /// difference is a `cd` the panel has not caught up with yet.
    followed: Option<String>,
    /// Whether the first listing has been attempted.
    ///
    /// Without this a failed initial listing would be retried on every chunk of
    /// terminal output, because the session notifies on each one and the panel's
    /// "no path yet" condition would still hold.
    attempted: bool,
    /// Bumped by every navigation. A reply carrying an older value belongs to a
    /// directory the user has already left, and is dropped.
    generation: u64,
    /// Whether a listing is in flight.
    busy: bool,
    /// The bottom status line.
    notice: Option<Notice>,
    /// Vertical scroll of the list, kept per session so returning to a tab
    /// returns to the same place in its directory.
    scroll: ScrollHandle,
}

impl SessionState {
    /// A state that has not listed anything yet.
    fn new() -> Self {
        Self {
            path: None,
            entries: Vec::new(),
            selected: None,
            followed: None,
            attempted: false,
            generation: 0,
            busy: false,
            notice: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// The selected entry, if one is selected and still listed.
    fn selection(&self) -> Option<&RemoteEntry> {
        let name = self.selected.as_deref()?;
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// The remote file panel.
pub struct FilePanel {
    /// Session whose directory is on screen. `None` while no tab is open.
    session: Option<Entity<Session>>,
    /// Browsing state per session, keyed by the session entity.
    ///
    /// Entries are dropped by [`FilePanel::forget_session`] when a pane closes;
    /// nothing else removes them, so a session keeps its place for as long as
    /// it is open.
    states: HashMap<EntityId, SessionState>,
    /// How wide the panel is drawn, in pixels.
    ///
    /// Session state only, like the workspace's `panel_open` flag: persisting it
    /// would mean a settings key, and re-dragging an edge is cheap enough that
    /// the key would earn its keep only once there is more to remember about the
    /// panel than a flag and a number.
    width: f32,
    /// Watches the active session for directory and status changes.
    _observer: Option<Subscription>,
}

impl FilePanel {
    /// An empty panel, attached to no session.
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            session: None,
            states: HashMap::new(),
            width: DEFAULT_PANEL_WIDTH,
            _observer: None,
        }
    }

    /// Widens or narrows the panel to follow a drag of its right edge.
    ///
    /// The width is read off the pointer rather than accumulated as a delta:
    /// the panel's left edge never moves, so the distance from it *is* the
    /// width, and a gesture that wandered outside the window comes back to the
    /// right place instead of to wherever the deltas summed to.
    fn drag_edge(&mut self, event: &DragMoveEvent<DraggedPanelEdge>, cx: &mut Context<Self>) {
        let width = f32::from(event.event.position.x - event.bounds.left());
        let width = width.clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH);
        if width == self.width || !width.is_finite() {
            return;
        }
        self.width = width;
        cx.notify();
    }

    /// Points the panel at `session`, keeping whatever it was showing before.
    ///
    /// Called from the workspace's render, so it must be cheap and idempotent:
    /// naming the session already on screen returns immediately, and only a real
    /// change re-subscribes and repaints.
    pub fn set_session(&mut self, session: Option<Entity<Session>>, cx: &mut Context<Self>) {
        let current = self.session.as_ref().map(Entity::entity_id);
        let next = session.as_ref().map(Entity::entity_id);
        if current == next {
            return;
        }

        // Only the active session is observed. A background session that
        // changes directory is caught the moment it becomes active again,
        // because `sync` compares against the directory last followed rather
        // than against the last one seen.
        self._observer = session
            .as_ref()
            .map(|session| cx.observe(session, |panel, _session, cx| panel.sync(cx)));
        self.session = session;
        self.sync(cx);
        cx.notify();
    }

    /// Drops the state of a session whose pane has closed.
    pub fn forget_session(&mut self, session: EntityId, cx: &mut Context<Self>) {
        if self.states.remove(&session).is_none() {
            return;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.entity_id() == session)
        {
            self.session = None;
            self._observer = None;
        }
        cx.notify();
    }

    /// Brings the panel in step with the active session.
    ///
    /// Runs on every notification from that session — which means on every chunk
    /// of terminal output — so the common path is two string comparisons and no
    /// allocation beyond the directory itself.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let id = session.entity_id();
        let Some(sftp) = session.read(cx).sftp() else {
            // Not connected (yet). The status change that connects the session
            // is itself a notification, so this is retried at the right moment.
            return;
        };
        let cwd = session.read(cx).cwd().map(str::to_owned);

        let target = {
            let state = self.states.entry(id).or_insert_with(SessionState::new);
            if !state.attempted {
                state.attempted = true;
                state.followed = cwd.clone();
                Some(cwd.clone().map_or(Target::Home, Target::Exact))
            } else if let Some(cwd) =
                cwd.filter(|cwd| state.followed.as_deref() != Some(cwd.as_str()))
            {
                state.followed = Some(cwd.clone());
                Some(Target::Exact(cwd))
            } else {
                None
            }
        };

        if let Some(target) = target {
            self.go(id, sftp, target, cx);
        }
    }

    /// Lists `target` for `session` and shows the result.
    ///
    /// Takes the session explicitly rather than reading the active one, so that
    /// a listing triggered by a finished transfer lands on the session that
    /// transferred, even if the user has since switched tabs.
    fn go(&mut self, session: EntityId, sftp: SftpClient, target: Target, cx: &mut Context<Self>) {
        let generation = {
            let state = self.states.entry(session).or_insert_with(SessionState::new);
            state.generation = state.generation.wrapping_add(1);
            state.busy = true;
            state.generation
        };

        cx.spawn(async move |panel, cx| {
            let result = list(&sftp, target).await;
            panel
                .update(cx, |panel, cx| {
                    panel.listing_arrived(session, generation, result, cx);
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Applies a listing, unless the user has moved on since it was asked for.
    fn listing_arrived(
        &mut self,
        session: EntityId,
        generation: u64,
        result: Result<(String, Vec<RemoteEntry>), SftpError>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.states.get_mut(&session) else {
            return;
        };
        // The stale-reply guard: a newer navigation has already bumped the
        // counter, so this answer describes a directory nobody is looking at.
        if state.generation != generation {
            return;
        }
        state.busy = false;

        match result {
            Ok((path, mut entries)) => {
                sort_entries(&mut entries);
                // A directory change invalidates the selection; staying on the
                // old name would let the download button act on a file from a
                // directory that is no longer on screen.
                if state.path.as_deref() != Some(path.as_str()) {
                    state.selected = None;
                    state.scroll.set_offset(Default::default());
                }
                state.path = Some(path);
                state.entries = entries;
                state.notice = None;
            }
            Err(error) => state.notice = Some(Notice::from_error(&error)),
        }
        cx.notify();
    }

    /// The active session's id, SFTP client and current directory.
    ///
    /// `None` whenever an action has nothing to act on: no session, a session
    /// that is not connected, or one whose first listing has not landed.
    fn acting_on(&self, cx: &App) -> Option<(EntityId, SftpClient, String)> {
        let session = self.session.as_ref()?;
        let id = session.entity_id();
        let sftp = session.read(cx).sftp()?;
        let path = self.states.get(&id)?.path.clone()?;
        Some((id, sftp, path))
    }

    /// Puts `notice` on `session`'s status line.
    fn set_notice(&mut self, session: EntityId, notice: Notice, cx: &mut Context<Self>) {
        let Some(state) = self.states.get_mut(&session) else {
            return;
        };
        state.notice = Some(notice);
        cx.notify();
    }

    /// Lists the current directory again.
    ///
    /// Also the way out of a failed first listing, which is not retried on its
    /// own.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let id = session.entity_id();
        let Some(sftp) = session.read(cx).sftp() else {
            return;
        };
        let target = match self.states.get(&id).and_then(|state| state.path.clone()) {
            Some(path) => Target::Exact(path),
            None => Target::Home,
        };
        self.go(id, sftp, target, cx);
    }

    /// Selects the entry named `name`.
    fn select(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref().map(Entity::entity_id) else {
            return;
        };
        let Some(state) = self.states.get_mut(&session) else {
            return;
        };
        if state.selected.as_deref() == Some(name) {
            return;
        }
        state.selected = Some(name.to_owned());
        cx.notify();
    }

    /// Opens the entry named `name`, if it is a directory.
    fn activate(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some((session, sftp, path)) = self.acting_on(cx) else {
            return;
        };
        let is_dir = self
            .states
            .get(&session)
            .and_then(|state| state.entries.iter().find(|entry| entry.name == name))
            .is_some_and(|entry| entry.is_dir);
        if !is_dir {
            return;
        }
        self.go(session, sftp, Target::Exact(join(&path, name)), cx);
    }

    /// Moves to the parent of the current directory.
    fn open_parent(&mut self, cx: &mut Context<Self>) {
        let Some((session, sftp, path)) = self.acting_on(cx) else {
            return;
        };
        self.go(session, sftp, Target::Resolve(join(&path, PARENT_NAME)), cx);
    }

    /// Asks the platform for local files and uploads them here.
    fn pick_upload(&mut self, cx: &mut Context<Self>) {
        if self.acting_on(cx).is_none() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(ts!("files.select_upload")),
        });

        cx.spawn(async move |panel, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the file picker could not be opened: {error:#}");
                    return;
                }
            };
            panel.update(cx, |panel, cx| panel.upload(chosen, cx)).ok();
        })
        .detach();
    }

    /// Uploads `paths` into the current directory, one after another.
    ///
    /// Directories are skipped rather than recursed into: a dropped folder is
    /// much more likely to be a slip than a request to copy a tree, and half a
    /// tree on the server would be worse than none.
    fn upload(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some((session, sftp, directory)) = self.acting_on(cx) else {
            return;
        };

        let mut files = Vec::new();
        let mut skipped: Option<SharedString> = None;
        for path in paths {
            // A `metadata` call per dropped path, on the UI thread: a handful of
            // local stats, against a network round trip per file about to
            // follow. Anything unreadable is left to the transfer to report.
            if std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                skipped.get_or_insert_with(|| file_name(&path));
                continue;
            }
            files.push(path);
        }

        if files.is_empty() {
            if let Some(name) = skipped {
                self.set_notice(
                    session,
                    Notice::Error(ts!("files.skipped_directory", name = name)),
                    cx,
                );
            }
            return;
        }

        cx.spawn(async move |panel, cx| {
            let mut uploaded = 0usize;
            let mut last = SharedString::default();
            let mut failure = None;

            for file in files {
                let name = file_name(&file);
                let progress = ts!("files.uploading", name = name.clone());
                if panel
                    .update(cx, |panel, cx| {
                        panel.set_notice(session, Notice::Info(progress), cx);
                    })
                    .is_err()
                {
                    return;
                }

                match sftp.upload(file, &directory).await {
                    Ok(_) => {
                        uploaded += 1;
                        last = name;
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }

            // A skipped directory outranks the success line: the transfer that
            // did happen is visible in the listing, the one that did not is not.
            let notice = match (failure, skipped) {
                (Some(error), _) => Notice::from_error(&error),
                (None, Some(name)) => Notice::Error(ts!("files.skipped_directory", name = name)),
                (None, None) if uploaded == 1 => Notice::Info(ts!("files.uploaded", name = last)),
                (None, None) => Notice::Info(ts!("files.uploaded_many", count = uploaded)),
            };

            panel
                .update(cx, |panel, cx| {
                    panel.set_notice(session, notice, cx);
                    if uploaded > 0 {
                        panel.go(session, sftp, Target::Exact(directory), cx);
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Saves the selected file locally, asking where to put it first.
    fn download(&mut self, cx: &mut Context<Self>) {
        let Some((session, sftp, directory)) = self.acting_on(cx) else {
            return;
        };
        let Some(entry) = self
            .states
            .get(&session)
            .and_then(SessionState::selection)
            .filter(|entry| !entry.is_dir)
        else {
            return;
        };
        let name = entry.name.clone();
        let remote = join(&directory, &name);
        let prompt = cx.prompt_for_new_path(&suggested_directory(), Some(&name));

        cx.spawn(async move |panel, cx| {
            let local = match prompt.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    log::warn!("the save dialog could not be opened: {error:#}");
                    return;
                }
            };

            let progress = ts!("files.downloading", name = name);
            if panel
                .update(cx, |panel, cx| {
                    panel.set_notice(session, Notice::Info(progress), cx);
                })
                .is_err()
            {
                return;
            }

            let shown = local.display().to_string();
            let notice = match sftp.download(&remote, local).await {
                Ok(()) => Notice::Info(ts!("files.downloaded", path = shown)),
                Err(error) => Notice::from_error(&error),
            };
            panel
                .update(cx, |panel, cx| panel.set_notice(session, notice, cx))
                .ok();
        })
        .detach();
    }

    /// Renders the header: the current path and the three action buttons.
    fn render_header(&self, state: Option<&SessionState>, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let path = state
            .and_then(|state| state.path.as_deref())
            .map_or_else(|| ts!("files.title"), |path| elide_start(path).into());
        let ready = state.is_some_and(|state| state.path.is_some());
        let downloadable = state
            .and_then(SessionState::selection)
            .is_some_and(|entry| !entry.is_dir);

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                // Mirrors the status bar: `truncate` needs a row flexing the
                // text child, not a bare `w_full`, to resolve its width.
                div().flex().flex_row().w_full().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(path),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(2.))
                    .child(icon_button(
                        "file-panel-refresh",
                        icons::REFRESH,
                        self.session.is_some(),
                        &theme,
                        cx.listener(|panel, _: &ClickEvent, _window, cx| panel.refresh(cx)),
                    ))
                    .child(icon_button(
                        "file-panel-upload",
                        icons::UPLOAD,
                        ready,
                        &theme,
                        cx.listener(|panel, _: &ClickEvent, _window, cx| panel.pick_upload(cx)),
                    ))
                    .child(icon_button(
                        "file-panel-download",
                        icons::DOWNLOAD,
                        ready && downloadable,
                        &theme,
                        cx.listener(|panel, _: &ClickEvent, _window, cx| panel.download(cx)),
                    )),
            )
            .into_any_element()
    }

    /// Renders the directory listing, or the placeholder standing in for it.
    fn render_list(&self, state: Option<&SessionState>, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme(cx);
        let connected = self
            .session
            .as_ref()
            .is_some_and(|session| session.read(cx).sftp().is_some());

        let Some(state) = state.filter(|state| state.path.is_some()) else {
            let message = if self.session.is_none() {
                ts!("files.no_session")
            } else if !connected {
                ts!("files.not_connected")
            } else {
                ts!("files.loading")
            };
            return placeholder(message, &theme);
        };

        // Safe by the filter above; kept as a match so a future change cannot
        // turn this into a panic.
        let path = state.path.as_deref().unwrap_or_default();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(state.entries.len() + 1);

        if path != "/" && !path.is_empty() {
            rows.push(self.render_row(
                ElementId::from("file-row-parent"),
                PARENT_NAME,
                true,
                false,
                None,
                false,
                &theme,
                cx,
            ));
        }
        for (index, entry) in state.entries.iter().enumerate() {
            let size = (!entry.is_dir).then(|| SharedString::from(format_size(entry.size)));
            rows.push(self.render_row(
                ElementId::from(("file-row", index)),
                &entry.name,
                entry.is_dir,
                entry.is_symlink,
                size,
                state.selected.as_deref() == Some(entry.name.as_str()),
                &theme,
                cx,
            ));
        }

        if rows.is_empty() {
            return placeholder(ts!("files.empty"), &theme);
        }

        div()
            .id("file-panel-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py(px(2.))
            .overflow_y_scroll()
            .track_scroll(&state.scroll)
            .children(rows)
            .into_any_element()
    }

    /// Renders one row of the listing.
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        id: ElementId,
        name: &str,
        is_dir: bool,
        is_symlink: bool,
        size: Option<SharedString>,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = SharedString::from(name.to_owned());
        let parent = name == PARENT_NAME;
        let owned = label.clone();

        div()
            .id(id)
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .w_full()
            .px(px(8.))
            .py(px(3.))
            .text_size(px(12.))
            .text_color(theme.text)
            .cursor_pointer()
            .when(selected, |row| row.bg(theme.surface_active))
            .when(!selected, |row| {
                row.hover(|style| style.bg(theme.surface_hover))
            })
            .on_click(cx.listener(move |panel, event: &ClickEvent, _window, cx| {
                // A double click arrives as two events, so the first one has
                // already selected the row by the time this opens it.
                if event.click_count() >= 2 {
                    if parent {
                        panel.open_parent(cx);
                    } else {
                        panel.activate(&owned, cx);
                    }
                } else if !parent {
                    panel.select(&owned, cx);
                }
            }))
            // The accent on directories is what makes a listing scannable at a
            // glance: it separates the folders from the files ahead of the
            // sort order, in both themes.
            .child(if is_dir {
                icons::icon(icons::FOLDER, px(ROW_ICON), theme.accent)
            } else {
                icons::icon(icons::FILE, px(ROW_ICON), theme.text_muted)
            })
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(is_symlink, |row| {
                row.child(icons::icon(
                    icons::SYMLINK,
                    px(BADGE_ICON),
                    theme.text_muted,
                ))
            })
            .children(size.map(|size| {
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(size)
            }))
            .into_any_element()
    }

    /// Renders the status line, when there is anything to say.
    fn render_notice(
        &self,
        state: Option<&SessionState>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = theme(cx);
        let state = state?;
        let (text, color) = match (&state.notice, state.busy) {
            (Some(Notice::Error(text)), _) => (text.clone(), theme.danger),
            (Some(Notice::Info(text)), _) => (text.clone(), theme.text_muted),
            (None, true) => (ts!("files.loading"), theme.text_muted),
            (None, false) => return None,
        };

        Some(
            div()
                .flex_none()
                .w_full()
                .min_w_0()
                .px(px(8.))
                .py(px(4.))
                .border_t_1()
                .border_color(theme.border)
                .text_size(px(11.))
                .text_color(color)
                .child(text)
                .into_any_element(),
        )
    }
}

impl Render for FilePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx);
        let state = self
            .session
            .as_ref()
            .and_then(|session| self.states.get(&session.entity_id()));

        let header = self.render_header(state, cx);
        let list = self.render_list(state, cx);
        let notice = self.render_notice(state, cx);
        let accent = theme.accent;

        // Kept wholly inside the panel and added last, so it wins the hit test
        // against the rows it covers. Straddling the border would put half the
        // grab area over the pane next door, which is drawn after the panel and
        // would take those pixels back.
        let handle = div()
            .id("file-panel-edge")
            .absolute()
            // A plain hitbox does not stop events reaching what is under it,
            // and under this one are listing rows that would take the press as
            // a selection.
            .occlude()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(PANEL_HANDLE))
            .cursor_ew_resize()
            // An empty preview: the edge follows the pointer directly, so a
            // ghost trailing it would only be a second thing to watch.
            .on_drag(DraggedPanelEdge, |_, _, _, cx| cx.new(|_| gpui::Empty));

        div()
            .id("file-panel")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.width))
            .h_full()
            .min_h_0()
            // The panel is the only thing covering these pixels, so this is
            // where the window opacity lands on them. Exactly one such fill per
            // pixel — see `app_settings::window_tint`.
            .bg(app_settings::window_tint(theme.background, cx))
            // A full hairline rather than just the divider on the right, so the
            // drop highlight below can recolour a frame the user can see
            // without adding a second tinted fill over the panel.
            .border_1()
            .border_color(theme.border)
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(accent))
            .can_drop(|dragged, _window, _cx| {
                <dyn Any>::downcast_ref::<ExternalPaths>(dragged).is_some()
            })
            .on_drop(cx.listener(|panel, paths: &ExternalPaths, _window, cx| {
                panel.upload(paths.paths().to_vec(), cx);
            }))
            // Listening on the panel, not on the handle: the handle slides out
            // from under the pointer as the drag goes on, while the panel's left
            // edge — the one the new width is measured from — stays put.
            .on_drag_move::<DraggedPanelEdge>(
                cx.listener(|panel, event, _window, cx| panel.drag_edge(event, cx)),
            )
            .child(header)
            .child(list)
            .children(notice)
            .child(handle)
    }
}

/// Resolves `target` and lists what it points at.
async fn list(sftp: &SftpClient, target: Target) -> Result<(String, Vec<RemoteEntry>), SftpError> {
    let path = match target {
        Target::Home => sftp.home().await?,
        Target::Resolve(path) => sftp.realpath(&path).await?,
        Target::Exact(path) => path,
    };
    let entries = sftp.read_dir(&path).await?;
    Ok((path, entries))
}

/// Orders a listing the way a file manager does: directories first, then by
/// name ignoring case.
///
/// Case-insensitive order puts `Downloads` next to `documents` instead of in a
/// separate uppercase block, which is what makes a listing scannable. Ties —
/// two names differing only in case — fall back to the exact order so the
/// result is deterministic.
fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Joins a remote directory and a name with the protocol's separator.
///
/// SFTP paths are POSIX on the wire whatever the server runs on, so this never
/// goes through [`std::path`] — which would produce backslashes when logman
/// itself runs on Windows.
fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else if directory.ends_with('/') {
        format!("{directory}{name}")
    } else {
        format!("{directory}/{name}")
    }
}

/// Shortens `path` from the front, marking the cut with an ellipsis.
///
/// The tail is what the header is for: `/srv/app/releases/2026-07-30/logs` says
/// where you are, `/srv/app/releases/2026-07…` does not.
fn elide_start(path: &str) -> String {
    let count = path.chars().count();
    if count <= PATH_CHARS {
        return path.to_owned();
    }
    let tail: String = path
        .chars()
        .skip(count.saturating_sub(PATH_CHARS.saturating_sub(1)))
        .collect();
    format!("\u{2026}{tail}")
}

/// Renders a byte count the way a file manager does.
///
/// The unit symbols are not translated: like the terminal grid size in the
/// status bar they are symbols rather than words, and every locale writes them
/// the same way.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    match UNITS.get(unit) {
        Some(_) if unit == 0 => format!("{bytes} B"),
        Some(symbol) => format!("{value:.1} {symbol}"),
        None => format!("{bytes} B"),
    }
}

/// The file name of `path`, for a status message.
fn file_name(path: &Path) -> SharedString {
    path.file_name()
        .map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
        .into()
}

/// Where the save dialog opens by default.
///
/// The platform picker remembers the last directory the user chose, so this
/// only has to be a sensible *first* answer; the home directory is one on every
/// platform, and an empty path leaves the choice to the picker.
fn suggested_directory() -> PathBuf {
    directories::UserDirs::new().map_or_else(PathBuf::new, |dirs| dirs.home_dir().to_owned())
}

/// A centred message standing in for a listing.
fn placeholder(message: SharedString, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .px(px(12.))
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(message)
        .into_any_element()
}

/// A compact icon-only toolbar button, in the style of the tab strip's own.
fn icon_button(
    id: impl Into<ElementId>,
    path: &'static str,
    enabled: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let hover = theme.surface_hover;
    let text = theme.text;
    let color = if enabled {
        theme.text_muted
    } else {
        theme.text_muted.opacity(0.4)
    };

    div()
        .id(id.into())
        .group(BUTTON_GROUP)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded_sm()
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(move |event, window, cx| on_click(event, window, cx))
        })
        .child(
            icons::icon(path, px(TOOLBAR_ICON), color).when(enabled, |icon| {
                icon.group_hover(BUTTON_GROUP, move |style| style.text_color(text))
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing entry, for the ordering test.
    fn entry(name: &str, is_dir: bool) -> RemoteEntry {
        RemoteEntry {
            name: name.to_owned(),
            is_dir,
            is_symlink: false,
            size: 0,
        }
    }

    #[test]
    fn directories_sort_before_files_and_case_is_ignored() {
        let mut entries = vec![
            entry("notes.txt", false),
            entry("Zebra", true),
            entry("apple", true),
            entry("Beta.log", false),
        ];
        sort_entries(&mut entries);

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["apple", "Zebra", "Beta.log", "notes.txt"]);
    }

    #[test]
    fn joining_adds_exactly_one_separator() {
        assert_eq!(join("/home/alice", "notes.txt"), "/home/alice/notes.txt");
        assert_eq!(join("/", ".."), "/..");
        assert_eq!(join("/srv/", "app"), "/srv/app");
    }

    #[test]
    fn a_long_path_keeps_its_tail() {
        let short = "/home/alice";
        assert_eq!(elide_start(short), short);

        let long = "/srv/application/releases/2026-07-30T12-00/logs/today";
        let elided = elide_start(long);
        assert_eq!(elided.chars().count(), PATH_CHARS);
        assert!(elided.starts_with('\u{2026}'));
        assert!(long.ends_with(elided.trim_start_matches('\u{2026}')));
    }

    #[test]
    fn sizes_read_the_way_a_file_manager_writes_them() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }
}
