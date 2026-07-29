//! The application settings dialog.
//!
//! Edits [`AppSettings`] and nothing else: it reads the current snapshot from
//! [`crate::app_settings`] when it opens, writes the edited copy to disk when
//! the user saves, and replaces the global so the rest of the app picks the
//! change up. Range checking is deliberately *not* duplicated here — the form
//! collects whatever the user typed and [`AppSettings::sanitize`] clamps it once
//! on the way out, which keeps one definition of "valid" in `logman-core`.

use std::sync::Once;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement, KeyBinding,
    KeyDownEvent, Render, ScrollHandle, SharedString, Window, actions, div, prelude::*, px, rgb,
};
use logman_core::{AppSettings, UiTheme};
use logman_term::TerminalTheme;

use crate::app_settings;
use crate::ui::{
    Button, ButtonVariant, Checkbox, SchemePicker, SchemePreview, SchemeSwatch, Segmented,
    TextInput, Theme, form_row, modal, theme,
};

/// Width of the dialog panel.
const DIALOG_WIDTH: f32 = 760.;

/// Height at which the form body starts scrolling.
const BODY_MAX_HEIGHT: f32 = 520.;

/// Cards per row in the color scheme picker.
const SCHEME_COLUMNS: usize = 3;

/// ANSI slots previewed on each scheme card: red, green, yellow, blue, magenta,
/// cyan. Black and white are skipped because they vanish into the background.
const PREVIEW_ANSI_SLOTS: [usize; 6] = [1, 2, 3, 4, 5, 6];

/// Segments of the UI theme picker, in [`UiTheme`] order.
const UI_THEME_OPTIONS: [(&str, &str); 2] = [("dark", "Dark"), ("light", "Light")];

/// Shown under the font family field when it is left empty.
const FONT_FAMILY_PLACEHOLDER: &str = "System default";

/// Key context the dialog's own shortcuts are scoped to.
///
/// `Tab` stays scoped here for the same reason it does in the connection
/// dialog: a global binding would stop the terminal from sending `\t`.
const KEY_CONTEXT: &str = "SettingsDialog";

/// Guards the one-time registration of the dialog's key bindings.
static BIND_KEYS: Once = Once::new();

actions!(
    logman_settings,
    [
        /// Move focus to the next control in the dialog.
        FocusNext,
        /// Move focus to the previous control in the dialog.
        FocusPrev,
    ]
);

/// Tab order of the form, in visual order, spaced so controls can be inserted
/// later without renumbering.
mod tab {
    /// UI theme picker.
    pub const UI_THEME: isize = 10;
    /// Background opacity, in percent.
    pub const OPACITY: isize = 20;
    /// Background blur toggle.
    pub const BLUR: isize = 30;
    /// Terminal color scheme grid.
    pub const SCHEME: isize = 40;
    /// Terminal font family.
    pub const FONT_FAMILY: isize = 50;
    /// Terminal font size.
    pub const FONT_SIZE: isize = 60;
    /// Scrollback depth.
    pub const SCROLLBACK: isize = 70;
    /// `TERM` advertised to the remote host.
    pub const TERM: isize = 80;
    /// Copy-on-select toggle.
    pub const COPY_ON_SELECT: isize = 90;
    /// Default SSH port for new connections.
    pub const DEFAULT_PORT: isize = 100;
    /// Default login name for new connections.
    pub const DEFAULT_USERNAME: isize = 110;
    /// Keepalive interval.
    pub const KEEPALIVE: isize = 120;
    /// Connect timeout.
    pub const TIMEOUT: isize = 130;
    /// Cancel.
    pub const CANCEL: isize = 140;
    /// Save.
    pub const SAVE: isize = 150;
}

/// Emitted by [`SettingsDialog`] when the user acts on it.
pub enum SettingsDialogEvent {
    /// The user saved: the settings global has been replaced and persisted.
    /// The shell should re-apply settings to the window and open sessions.
    Applied,
    /// The dialog was dismissed without saving.
    Dismissed,
}

/// Builds the preview colors for the scheme with the given id.
///
/// Returns `None` for an id `logman-term` does not know, which is how a
/// hand-edited `settings.json` naming a removed scheme degrades to a plain card
/// instead of a panic.
fn preview_for(id: &str) -> Option<SchemePreview> {
    let scheme = TerminalTheme::by_name(id)?;
    Some(SchemePreview {
        background: rgb(scheme.background.to_u32()).into(),
        foreground: rgb(scheme.foreground.to_u32()).into(),
        ansi: PREVIEW_ANSI_SLOTS
            .iter()
            .map(|slot| rgb(scheme.ansi[*slot].to_u32()).into())
            .collect(),
    })
}

/// The built-in schemes as picker entries, each with a live preview.
pub(crate) fn scheme_swatches() -> Vec<SchemeSwatch> {
    TerminalTheme::builtin()
        .iter()
        .map(|info| {
            let swatch = SchemeSwatch::new(info.id, info.name);
            match preview_for(info.id) {
                Some(preview) => swatch.preview(preview),
                None => swatch,
            }
        })
        .collect()
}

/// Severity of the message strip at the bottom of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLevel {
    /// Something went wrong and the settings were not written.
    Error,
}

impl StatusLevel {
    /// Color of the message text under the active theme.
    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Error => theme.danger,
        }
    }
}

/// Modal dialog editing [`logman_core::AppSettings`].
///
/// Create it once with [`SettingsDialog::new`], keep the handle, subscribe to
/// [`SettingsDialogEvent`], and render it as the last child of a `relative()`
/// root. It renders nothing while [`SettingsDialog::is_open`] is `false`, so it
/// is safe to render unconditionally.
pub struct SettingsDialog {
    /// Whether the dialog is currently visible.
    open: bool,
    /// The snapshot the form was populated from. Saving starts from this value
    /// so fields the dialog does not edit — the schema version, for one —
    /// survive a round trip.
    base: AppSettings,
    /// UI chrome theme currently selected in the form.
    ui_theme: UiTheme,
    /// Whether the window should be blurred behind.
    background_blur: bool,
    /// Terminal color scheme id currently selected in the form.
    scheme: SharedString,
    /// Whether the selection is copied to the clipboard on mouse release.
    copy_on_select: bool,
    /// Message strip shown above the buttons.
    status: Option<SharedString>,
    /// Focus of the dialog root; also the anchor for the `Escape` handler.
    focus_handle: FocusHandle,
    /// Whether focus should move into the form on the next render.
    pending_focus: bool,
    /// Scroll position of the form body, so `Tab` can reveal the section it
    /// just moved into.
    body_scroll: ScrollHandle,
    /// Index of the section currently scrolled into view. Kept so that tabbing
    /// between two controls of the same section does not re-scroll it.
    visible_section: usize,
    /// Window background opacity, in whole percent.
    opacity_input: Entity<TextInput>,
    /// Terminal font family; empty means the per-OS default.
    font_family_input: Entity<TextInput>,
    /// Terminal font size.
    font_size_input: Entity<TextInput>,
    /// Scrollback depth in lines.
    scrollback_input: Entity<TextInput>,
    /// `TERM` advertised to the remote host.
    term_input: Entity<TextInput>,
    /// Default SSH port for new connections.
    port_input: Entity<TextInput>,
    /// Default login name for new connections.
    username_input: Entity<TextInput>,
    /// Seconds between keepalive probes.
    keepalive_input: Entity<TextInput>,
    /// Seconds to wait for the TCP connection.
    timeout_input: Entity<TextInput>,
}

impl SettingsDialog {
    /// Build the dialog.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();

        BIND_KEYS.call_once(|| {
            cx.bind_keys([
                KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
                KeyBinding::new("shift-tab", FocusPrev, Some(KEY_CONTEXT)),
            ]);
        });

        // `Enter` saves from any field, matching the connection dialog. The
        // deferred call is load-bearing: `on_submit` runs while gpui has the
        // TextInput leased, and saving reads every field back.
        let field = {
            let weak = weak.clone();
            move |cx: &mut Context<Self>, placeholder: &'static str, tab_index: isize| {
                let weak = weak.clone();
                cx.new(move |cx| {
                    TextInput::new(cx)
                        .placeholder(placeholder)
                        .tab_index(tab_index)
                        .on_submit(move |_, _window, cx| {
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                weak.update(cx, |this, cx| this.save(cx)).ok();
                            });
                        })
                })
            }
        };

        let opacity_input = field(cx, "100", tab::OPACITY);
        let font_family_input = field(cx, FONT_FAMILY_PLACEHOLDER, tab::FONT_FAMILY);
        let font_size_input = field(cx, "14", tab::FONT_SIZE);
        let scrollback_input = field(cx, "5000", tab::SCROLLBACK);
        let term_input = field(cx, "xterm-256color", tab::TERM);
        let port_input = field(cx, "22", tab::DEFAULT_PORT);
        let username_input = field(cx, "none", tab::DEFAULT_USERNAME);
        let keepalive_input = field(cx, "30", tab::KEEPALIVE);
        let timeout_input = field(cx, "15", tab::TIMEOUT);

        // Numeric fields have no input filter of their own, so each one is
        // sanitised after the fact by an observer.
        restrict_to_number(cx, &opacity_input, false, 3);
        restrict_to_number(cx, &font_size_input, true, 5);
        restrict_to_number(cx, &scrollback_input, false, 6);
        restrict_to_number(cx, &port_input, false, 5);
        restrict_to_number(cx, &keepalive_input, false, 5);
        restrict_to_number(cx, &timeout_input, false, 5);

        let base = AppSettings::default();
        Self {
            open: false,
            ui_theme: base.ui_theme,
            background_blur: base.window.background_blur,
            scheme: base.terminal.scheme.clone().into(),
            copy_on_select: base.terminal.copy_on_select,
            base,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            body_scroll: ScrollHandle::new(),
            visible_section: 0,
            opacity_input,
            font_family_input,
            font_size_input,
            scrollback_input,
            term_input,
            port_input,
            username_input,
            keepalive_input,
            timeout_input,
        }
    }

    /// Show the dialog, re-reading the current settings into the form.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        let settings = app_settings::current(cx);
        self.fill_form(&settings, cx);
        self.base = settings;
        self.status = None;
        self.open = true;
        self.pending_focus = true;
        self.visible_section = 0;
        self.body_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Whether the dialog is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Hide the dialog without saving.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.pending_focus = false;
        self.status = None;
        cx.notify();
    }

    /// Copy `settings` into every control.
    fn fill_form(&mut self, settings: &AppSettings, cx: &mut Context<Self>) {
        self.ui_theme = settings.ui_theme;
        self.background_blur = settings.window.background_blur;
        self.scheme = settings.terminal.scheme.clone().into();
        self.copy_on_select = settings.terminal.copy_on_select;

        let percent = (settings.window.background_opacity * 100.0).round() as i32;
        set_text(&self.opacity_input, percent.to_string(), cx);
        set_text(
            &self.font_family_input,
            settings.terminal.font_family.clone().unwrap_or_default(),
            cx,
        );
        set_text(
            &self.font_size_input,
            format_number(settings.terminal.font_size),
            cx,
        );
        set_text(
            &self.scrollback_input,
            settings.terminal.scrollback_lines.to_string(),
            cx,
        );
        set_text(&self.term_input, settings.terminal.term.clone(), cx);
        set_text(
            &self.port_input,
            settings.connection.default_port.to_string(),
            cx,
        );
        set_text(
            &self.username_input,
            settings
                .connection
                .default_username
                .clone()
                .unwrap_or_default(),
            cx,
        );
        set_text(
            &self.keepalive_input,
            settings.connection.keepalive_secs.to_string(),
            cx,
        );
        set_text(
            &self.timeout_input,
            settings.connection.connect_timeout_secs.to_string(),
            cx,
        );
    }

    /// Assemble the form into settings, starting from the snapshot the dialog
    /// opened with so untouched fields survive.
    ///
    /// A field the user emptied or made unparseable keeps the value it had when
    /// the dialog opened; nothing here clamps, because
    /// [`AppSettings::sanitize`] does that once for the whole struct.
    fn collect(&self, cx: &App) -> AppSettings {
        let mut settings = self.base.clone();

        settings.ui_theme = self.ui_theme;
        settings.window.background_blur = self.background_blur;
        if let Some(percent) = parse_number::<f32>(&self.opacity_input, cx) {
            settings.window.background_opacity = percent / 100.0;
        }

        settings.terminal.scheme = self.scheme.to_string();
        settings.terminal.font_family = optional_text(&self.font_family_input, cx);
        settings.terminal.copy_on_select = self.copy_on_select;
        if let Some(size) = parse_number::<f32>(&self.font_size_input, cx) {
            settings.terminal.font_size = size;
        }
        if let Some(lines) = parse_number::<usize>(&self.scrollback_input, cx) {
            settings.terminal.scrollback_lines = lines;
        }
        let term = text(&self.term_input, cx);
        if !term.is_empty() {
            settings.terminal.term = term;
        }

        if let Some(port) = parse_number::<u16>(&self.port_input, cx) {
            settings.connection.default_port = port;
        }
        settings.connection.default_username = optional_text(&self.username_input, cx);
        if let Some(secs) = parse_number::<u64>(&self.keepalive_input, cx) {
            settings.connection.keepalive_secs = secs;
        }
        if let Some(secs) = parse_number::<u64>(&self.timeout_input, cx) {
            settings.connection.connect_timeout_secs = secs;
        }

        settings
    }

    /// Persist the form and apply it, or report why it could not be written.
    ///
    /// A failed write leaves the dialog open with the message showing, so the
    /// user never believes a setting took effect when it did not.
    fn save(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.collect(cx);
        settings.sanitize();

        if let Err(err) = settings.save() {
            log::error!("could not write settings.json: {err:#}");
            self.status = Some(format!("Could not save the settings: {err:#}").into());
            // Show the clamped values so the user sees what would be stored.
            self.fill_form(&settings, cx);
            cx.notify();
            return;
        }

        app_settings::replace(settings, cx);
        cx.emit(SettingsDialogEvent::Applied);
        self.close(cx);
    }

    /// Close the dialog and report that nothing was saved.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsDialogEvent::Dismissed);
        self.close(cx);
    }

    /// `Tab`: move focus to the next control. gpui's tab ring wraps on its own.
    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next();
        self.reveal_focused(window, cx);
    }

    /// `Shift+Tab`: move focus to the previous control, wrapping to the last.
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev();
        self.reveal_focused(window, cx);
    }

    /// Scroll the section holding the focused control into view.
    ///
    /// Without this a focus ring below the fold would be invisible, which is
    /// the same as having no focus indicator at all. The section is derived
    /// from the focused handle's tab index, so no per-control bookkeeping is
    /// needed for the controls whose focus handles gpui creates itself.
    fn reveal_focused(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(handle) = window.focused(cx) else {
            return;
        };
        let section = match handle.tab_index {
            index if index <= tab::BLUR => 0,
            index if index <= tab::COPY_ON_SELECT => 1,
            _ => 2,
        };
        if section != self.visible_section {
            self.visible_section = section;
            self.body_scroll.scroll_to_item(section);
            cx.notify();
        }
    }

    /// `Escape` dismisses the dialog from anywhere inside it.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.open && event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.dismiss(cx);
        }
    }

    /// Move focus into the first control when the dialog opens.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_focus {
            return;
        }
        self.pending_focus = false;
        let handle = self.opacity_input.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    /// The "Appearance" section.
    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();
        let selected = match self.ui_theme {
            UiTheme::Dark => 0,
            UiTheme::Light => 1,
        };

        let theme_picker = Segmented::new("settings-ui-theme")
            .options(UI_THEME_OPTIONS)
            .selected(selected)
            .tab_index(tab::UI_THEME)
            .on_select({
                let this = this.clone();
                move |index, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.ui_theme = if index == 1 {
                            UiTheme::Light
                        } else {
                            UiTheme::Dark
                        };
                        cx.notify();
                    });
                }
            });

        let blur = Checkbox::new("settings-blur", "Blur the desktop behind the window")
            .checked(self.background_blur)
            .tab_index(tab::BLUR)
            .on_toggle({
                let this = this.clone();
                move |checked, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.background_blur = checked;
                        cx.notify();
                    });
                }
            });

        section(
            "Appearance",
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row("UI theme", theme_picker))
                .child(form_row(
                    "Opacity",
                    suffixed(self.opacity_input.clone(), "% (50\u{2013}100)", cx),
                ))
                .child(form_row("", blur)),
        )
    }

    /// The "Terminal" section.
    fn render_terminal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.entity();

        let picker = SchemePicker::new("settings-scheme")
            .options(scheme_swatches())
            .selected(Some(self.scheme.clone()))
            .columns(SCHEME_COLUMNS)
            .tab_index(tab::SCHEME)
            .on_select({
                let this = this.clone();
                move |id, _window, cx| {
                    let id = SharedString::from(id.to_owned());
                    this.update(cx, |dialog, cx| {
                        dialog.scheme = id;
                        cx.notify();
                    });
                }
            });

        let copy_on_select = Checkbox::new(
            "settings-copy-on-select",
            "Copy the selection on mouse release",
        )
        .checked(self.copy_on_select)
        .tab_index(tab::COPY_ON_SELECT)
        .on_toggle({
            let this = this.clone();
            move |checked, _window, cx| {
                this.update(cx, |dialog, cx| {
                    dialog.copy_on_select = checked;
                    cx.notify();
                });
            }
        });

        section(
            "Terminal",
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row("Color scheme", picker))
                .child(form_row("Font", self.font_family_input.clone()))
                .child(form_row(
                    "Font size",
                    suffixed(self.font_size_input.clone(), "pt (6\u{2013}32)", cx),
                ))
                .child(form_row(
                    "Scrollback",
                    suffixed(self.scrollback_input.clone(), "lines (max 100000)", cx),
                ))
                .child(form_row("TERM", self.term_input.clone()))
                .child(form_row("", copy_on_select)),
        )
    }

    /// The "New connections" section.
    fn render_connection(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        section(
            "New connections",
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row("Port", self.port_input.clone()))
                .child(form_row("Username", self.username_input.clone()))
                .child(form_row(
                    "Keepalive",
                    suffixed(self.keepalive_input.clone(), "seconds (0 disables)", cx),
                ))
                .child(form_row(
                    "Connect timeout",
                    suffixed(self.timeout_input.clone(), "seconds", cx),
                )),
        )
    }

    /// The message strip and the action buttons.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme(cx);
        let this = cx.entity();

        let status = self.status.clone().map(|message| {
            div()
                .text_size(px(12.))
                .text_color(StatusLevel::Error.color(&theme))
                .child(message)
        });

        div()
            .flex()
            .flex_col()
            .flex_none()
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
                        Button::new("settings-cancel", "Cancel")
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
                        Button::new("settings-save", "Save")
                            .variant(ButtonVariant::Primary)
                            .tab_index(tab::SAVE)
                            .on_click({
                                let this = this.clone();
                                move |_, _window, cx| {
                                    this.update(cx, |dialog, cx| dialog.save(cx));
                                }
                            }),
                    ),
            )
    }
}

impl EventEmitter<SettingsDialogEvent> for SettingsDialog {}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().id("settings-dialog");
        }

        self.apply_pending_focus(window, cx);

        // The `min_h_0` chain lets the scroll area shrink below its cap when
        // the modal hits the window height, keeping the footer on screen.
        let body = div()
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(12.))
            .child(
                div()
                    .id("settings-body")
                    .track_scroll(&self.body_scroll)
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .gap(px(14.))
                    .max_h(px(BODY_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .child(self.render_appearance(cx))
                    .child(self.render_terminal(cx))
                    .child(self.render_connection(cx)),
            )
            .child(self.render_footer(cx));

        let on_dismiss = {
            let this = cx.entity();
            move |_window: &mut Window, cx: &mut App| {
                this.update(cx, |dialog, cx| dialog.dismiss(cx));
            }
        };

        // Absolute and full-size for the same reason as the connection dialog:
        // an absolutely positioned child is laid out against its direct parent.
        div()
            .id("settings-dialog")
            .key_context(KEY_CONTEXT)
            .absolute()
            .inset_0()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                "settings-modal",
                "Settings",
                px(DIALOG_WIDTH),
                body,
                on_dismiss,
            ))
    }
}

/// Wraps `body` in a titled card.
fn section<E: IntoElement>(title: &'static str, cx: &App, body: E) -> impl IntoElement + use<E> {
    let theme = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(12.))
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child(title),
        )
        .child(body)
}

/// Lays a short unit hint out to the right of a narrow control.
fn suffixed<E: IntoElement>(control: E, hint: &'static str, cx: &App) -> impl IntoElement + use<E> {
    let theme = theme(cx);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .w_full()
        .child(div().flex_none().w(px(96.)).child(control))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child(hint),
        )
}

/// Trimmed content of `input`.
fn text(input: &Entity<TextInput>, cx: &App) -> String {
    input.read(cx).content().trim().to_owned()
}

/// Trimmed content of `input`, or `None` when it is blank.
fn optional_text(input: &Entity<TextInput>, cx: &App) -> Option<String> {
    let value = text(input, cx);
    (!value.is_empty()).then_some(value)
}

/// Parses `input` into `T`, or `None` when it is blank or malformed.
fn parse_number<T: std::str::FromStr>(input: &Entity<TextInput>, cx: &App) -> Option<T> {
    text(input, cx).parse::<T>().ok()
}

/// Replaces the contents of `input`.
fn set_text(input: &Entity<TextInput>, value: impl Into<SharedString>, cx: &mut App) {
    input.update(cx, |input, cx| input.set_content(value, cx));
}

/// Renders `value` without a trailing `.0`, so 14.0 shows as "14".
fn format_number(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Installs an observer that keeps `input` numeric.
///
/// The text field has no input filter, so the content is rewritten after every
/// edit. Rewriting only when the text actually changes stops the observer from
/// re-triggering itself.
fn restrict_to_number(
    cx: &mut Context<SettingsDialog>,
    input: &Entity<TextInput>,
    decimals: bool,
    max_len: usize,
) {
    cx.observe(input, move |_this, input, cx| {
        let content = input.read(cx).content().to_owned();
        let mut seen_dot = false;
        let filtered: String = content
            .chars()
            .filter(|c| {
                if c.is_ascii_digit() {
                    true
                } else if decimals && *c == '.' && !seen_dot {
                    seen_dot = true;
                    true
                } else {
                    false
                }
            })
            .take(max_len)
            .collect();
        if filtered != content {
            input.update(cx, |input, cx| input.set_content(filtered, cx));
        }
    })
    .detach();
}
