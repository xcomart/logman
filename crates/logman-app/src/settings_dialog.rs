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
use logman_core::{AppSettings, TitlebarStyle, UiTheme};
use logman_term::TerminalTheme;

use crate::app_settings;
use crate::i18n::{self, ts};
use crate::ui::{
    Button, ButtonVariant, Checkbox, SchemePicker, SchemePreview, SchemeSwatch, Segmented, Select,
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
///
/// The first half of each pair is an element id and is never translated; only
/// the label is. Built per call rather than declared as a `const` because the
/// labels come out of the active locale.
fn ui_theme_options() -> [(&'static str, SharedString); 2] {
    [
        ("dark", ts!("settings.theme_dark")),
        ("light", ts!("settings.theme_light")),
    ]
}

/// Segments of the title bar style picker, in [`TitlebarStyle`] order.
///
/// Built per call for the same reason [`ui_theme_options`] is.
fn titlebar_options() -> [(&'static str, SharedString); 2] {
    [
        ("custom", ts!("settings.titlebar_custom")),
        ("system", ts!("settings.titlebar_system")),
    ]
}

/// Label of the entry that hands the choice back to the operating system.
///
/// Heads both dropdowns in the dialog, and doubles as their placeholder so a
/// trigger reads the same whether or not its list is open.
fn system_default() -> SharedString {
    ts!("settings.system_default")
}

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
    /// Title bar style picker.
    pub const TITLEBAR: isize = 15;
    /// Interface language picker.
    pub const LANGUAGE: isize = 20;
    /// Background opacity, in percent.
    pub const OPACITY: isize = 30;
    /// Background blur toggle.
    pub const BLUR: isize = 40;
    /// Terminal color scheme grid.
    pub const SCHEME: isize = 50;
    /// Terminal font family.
    pub const FONT_FAMILY: isize = 60;
    /// Terminal font size.
    pub const FONT_SIZE: isize = 70;
    /// Scrollback depth.
    pub const SCROLLBACK: isize = 80;
    /// `TERM` advertised to the remote host.
    pub const TERM: isize = 90;
    /// Copy-on-select toggle.
    pub const COPY_ON_SELECT: isize = 100;
    /// Default SSH port for new connections.
    pub const DEFAULT_PORT: isize = 110;
    /// Default login name for new connections.
    pub const DEFAULT_USERNAME: isize = 120;
    /// Keepalive interval.
    pub const KEEPALIVE: isize = 130;
    /// Connect timeout.
    pub const TIMEOUT: isize = 140;
    /// Cancel.
    pub const CANCEL: isize = 150;
    /// Save.
    pub const SAVE: isize = 160;
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
///
/// A scheme whose colors cannot be resolved falls back to the muted placeholder
/// card, so it is given the translated label that card draws.
pub(crate) fn scheme_swatches() -> Vec<SchemeSwatch> {
    TerminalTheme::builtin()
        .iter()
        .map(|info| {
            let swatch = SchemeSwatch::new(info.id, info.name);
            match preview_for(info.id) {
                Some(preview) => swatch.preview(preview),
                None => swatch.placeholder_label(ts!("common.inherits")),
            }
        })
        .collect()
}

/// Which of the dialog's dropdown lists is currently showing.
///
/// A single field rather than one flag per dropdown, so that the two cannot be
/// open at once — their lists are drawn deferred and would overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenList {
    /// The interface language picker.
    Language,
    /// The terminal font picker.
    Font,
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
    /// Title bar style currently selected in the form.
    titlebar: TitlebarStyle,
    /// BCP 47 tag of the interface language; `None` follows the system locale.
    /// Holds the tag rather than the label, because the label is what the
    /// dropdown shows and the tag is what gets persisted.
    language: Option<String>,
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
    /// Terminal font family; `None` means the per-OS default.
    font_family: Option<SharedString>,
    /// Which dropdown, if any, is showing its list.
    open_list: Option<OpenList>,
    /// Font families installed on the machine, read once per opening of the
    /// dialog rather than on every render.
    fonts: Vec<SharedString>,
    /// Scroll position of the font list, so opening it reveals the current
    /// font instead of the top of the alphabet.
    font_scroll: ScrollHandle,
    /// Scroll position of the language list, kept for the same reason.
    language_scroll: ScrollHandle,
    /// Window background opacity, in whole percent.
    opacity_input: Entity<TextInput>,
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
            move |cx: &mut Context<Self>, placeholder: SharedString, tab_index: isize| {
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

        // Every placeholder but one is a sample *value* — a number, or the
        // default `TERM` — and reads the same in every language. The username
        // hint is a word, so it is translated; it is also the only placeholder
        // `refresh_placeholders` has to revisit after a language switch.
        let opacity_input = field(cx, "100".into(), tab::OPACITY);
        let font_size_input = field(cx, "14".into(), tab::FONT_SIZE);
        let scrollback_input = field(cx, "5000".into(), tab::SCROLLBACK);
        let term_input = field(cx, "xterm-256color".into(), tab::TERM);
        let port_input = field(cx, "22".into(), tab::DEFAULT_PORT);
        let username_input = field(
            cx,
            ts!("settings.username_placeholder"),
            tab::DEFAULT_USERNAME,
        );
        let keepalive_input = field(cx, "30".into(), tab::KEEPALIVE);
        let timeout_input = field(cx, "15".into(), tab::TIMEOUT);

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
            titlebar: base.window.titlebar,
            language: base.language.clone(),
            background_blur: base.window.background_blur,
            scheme: base.terminal.scheme.clone().into(),
            copy_on_select: base.terminal.copy_on_select,
            font_family: base.terminal.font_family.clone().map(SharedString::from),
            base,
            status: None,
            focus_handle: cx.focus_handle(),
            pending_focus: false,
            body_scroll: ScrollHandle::new(),
            visible_section: 0,
            open_list: None,
            fonts: Vec::new(),
            font_scroll: ScrollHandle::new(),
            language_scroll: ScrollHandle::new(),
            opacity_input,
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
        self.fonts = installed_fonts(cx);
        self.refresh_placeholders(cx);
        self.fill_form(&settings, cx);
        self.base = settings;
        self.status = None;
        self.open = true;
        self.open_list = None;
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
        self.open_list = None;
        self.pending_focus = false;
        self.status = None;
        cx.notify();
    }

    /// Re-translate the placeholders of the fields that have a worded one.
    ///
    /// The text fields are built once, when the dialog is created, so their
    /// hints would otherwise still be in whatever language was active at
    /// start-up after the user switches — including right after switching it
    /// here.
    fn refresh_placeholders(&self, cx: &mut Context<Self>) {
        self.username_input.update(cx, |input, cx| {
            input.set_placeholder(ts!("settings.username_placeholder"), cx);
        });
    }

    /// Copy `settings` into every control.
    fn fill_form(&mut self, settings: &AppSettings, cx: &mut Context<Self>) {
        self.ui_theme = settings.ui_theme;
        self.titlebar = settings.window.titlebar;
        self.language = settings.language.clone();
        self.background_blur = settings.window.background_blur;
        self.scheme = settings.terminal.scheme.clone().into();
        self.copy_on_select = settings.terminal.copy_on_select;
        self.font_family = settings
            .terminal
            .font_family
            .clone()
            .map(SharedString::from);

        let percent = (settings.window.background_opacity * 100.0).round() as i32;
        set_text(&self.opacity_input, percent.to_string(), cx);
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
        settings.language = self.language.clone();
        settings.window.titlebar = self.titlebar;
        settings.window.background_blur = self.background_blur;
        if let Some(percent) = parse_number::<f32>(&self.opacity_input, cx) {
            settings.window.background_opacity = percent / 100.0;
        }

        settings.terminal.scheme = self.scheme.to_string();
        settings.terminal.font_family = self.font_family.as_ref().map(ToString::to_string);
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
            self.status = Some(ts!("settings.save_failed", error = format!("{err:#}")));
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
        self.close_lists(cx);
        window.focus_next();
        self.reveal_focused(window, cx);
    }

    /// `Shift+Tab`: move focus to the previous control, wrapping to the last.
    fn focus_prev(&mut self, _: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.close_lists(cx);
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
    ///
    /// An open dropdown takes the key first and only closes itself, so that
    /// backing out of a list does not also throw away the whole form.
    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || event.keystroke.key != "escape" {
            return;
        }
        cx.stop_propagation();
        if self.open_list.is_some() {
            self.close_lists(cx);
            return;
        }
        self.dismiss(cx);
    }

    /// Hide whichever dropdown list is showing.
    ///
    /// Called whenever focus leaves a dropdown, so that a list nobody is
    /// driving any more does not stay painted over the rest of the form.
    fn close_lists(&mut self, cx: &mut Context<Self>) {
        if self.open_list.take().is_some() {
            cx.notify();
        }
    }

    /// The entries of the font dropdown: the "leave it to the OS" row first,
    /// then every installed family.
    ///
    /// A saved font that is not installed — a hand-edited `settings.json`, or a
    /// family that has since been removed — is spliced in after the first row,
    /// so the trigger keeps showing it instead of silently falling back.
    fn font_options(&self) -> Vec<SharedString> {
        let mut options = Vec::with_capacity(self.fonts.len() + 2);
        options.push(system_default());
        options.extend(
            self.font_family
                .clone()
                .filter(|family| !self.fonts.contains(family)),
        );
        options.extend(self.fonts.iter().cloned());
        options
    }

    /// The entries of the language dropdown: "follow the system" first, then
    /// every shipped translation named in its own language.
    fn language_options() -> Vec<SharedString> {
        let supported = i18n::supported();
        let mut options = Vec::with_capacity(supported.len() + 1);
        options.push(system_default());
        options.extend(supported.iter().map(|(_, name)| name.clone()));
        options
    }

    /// Show or hide `list`, revealing the current entry as it opens.
    ///
    /// Opening one list closes the other, since both are drawn deferred and
    /// two open at once would paint over each other.
    fn set_list_open(&mut self, list: OpenList, open: bool, cx: &mut Context<Self>) {
        self.open_list = open.then_some(list);
        if open {
            let (scroll, current) = match list {
                OpenList::Font => {
                    let options = self.font_options();
                    let current = self
                        .font_family
                        .as_ref()
                        .and_then(|family| options.iter().position(|option| option == family));
                    (&self.font_scroll, current)
                }
                OpenList::Language => (&self.language_scroll, self.language_index()),
            };
            scroll.scroll_to_item(current.unwrap_or(0));
        }
        cx.notify();
    }

    /// Position of the selected language in [`Self::language_options`], or
    /// `None` while the language follows the system — or names a tag logman
    /// has no translation for, which the app treats the same way.
    fn language_index(&self) -> Option<usize> {
        let tag = self.language.as_deref()?;
        let index = i18n::supported()
            .iter()
            .position(|(code, _)| *code == tag)?;
        Some(index + 1)
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
            .options(ui_theme_options())
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

        let titlebar_picker = Segmented::new("settings-titlebar")
            .options(titlebar_options())
            .selected(match self.titlebar {
                TitlebarStyle::Custom => 0,
                TitlebarStyle::System => 1,
            })
            .tab_index(tab::TITLEBAR)
            .on_select({
                let this = this.clone();
                move |index, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.titlebar = if index == 1 {
                            TitlebarStyle::System
                        } else {
                            TitlebarStyle::Custom
                        };
                        cx.notify();
                    });
                }
            });

        let language = Select::new("settings-language")
            .options(Self::language_options())
            .selected(self.language.as_deref().and_then(i18n::display_name))
            .placeholder(system_default())
            .open(self.open_list == Some(OpenList::Language))
            .tab_index(tab::LANGUAGE)
            .scroll_handle(self.language_scroll.clone())
            .on_select({
                let this = this.clone();
                // By index, not by label: row 0 is "follow the system" and the
                // rest line up with `i18n::supported`, whereas the labels are
                // endonyms that say nothing about their position.
                move |index, _label, _window, cx| {
                    let tag = index
                        .checked_sub(1)
                        .and_then(|index| i18n::supported().get(index))
                        .map(|(code, _)| (*code).to_owned());
                    this.update(cx, |dialog, cx| {
                        dialog.language = tag;
                        cx.notify();
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_list_open(OpenList::Language, open, cx);
                    });
                }
            });

        let blur = Checkbox::new("settings-blur", ts!("settings.blur"))
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
            ts!("settings.section.appearance"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(ts!("settings.ui_theme"), theme_picker))
                .child(form_row(ts!("settings.titlebar"), titlebar_picker))
                .child(form_row(ts!("settings.language"), language))
                .child(form_row(
                    ts!("settings.opacity"),
                    suffixed(self.opacity_input.clone(), ts!("settings.opacity_hint"), cx),
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

        let font = Select::new("settings-font")
            .options(self.font_options())
            .selected(self.font_family.clone())
            .placeholder(system_default())
            .open(self.open_list == Some(OpenList::Font))
            .tab_index(tab::FONT_FAMILY)
            .scroll_handle(self.font_scroll.clone())
            .on_select({
                let this = this.clone();
                // Row 0 is the "leave it to the OS" entry; comparing its label
                // against the picked text would only work in one language.
                move |index, family, _window, cx| {
                    let family = (index > 0).then(|| SharedString::from(family.to_owned()));
                    this.update(cx, |dialog, cx| {
                        dialog.font_family = family;
                        cx.notify();
                    });
                }
            })
            .on_open_change({
                let this = this.clone();
                move |open, _window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.set_list_open(OpenList::Font, open, cx);
                    });
                }
            });

        let copy_on_select =
            Checkbox::new("settings-copy-on-select", ts!("settings.copy_on_select"))
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
            ts!("settings.section.terminal"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(ts!("settings.scheme"), picker))
                .child(form_row(ts!("settings.font"), font))
                .child(form_row(
                    ts!("settings.font_size"),
                    suffixed(
                        self.font_size_input.clone(),
                        ts!("settings.font_size_hint"),
                        cx,
                    ),
                ))
                .child(form_row(
                    ts!("settings.scrollback"),
                    suffixed(
                        self.scrollback_input.clone(),
                        ts!("settings.scrollback_hint"),
                        cx,
                    ),
                ))
                .child(form_row(ts!("settings.term"), self.term_input.clone()))
                .child(form_row("", copy_on_select)),
        )
    }

    /// The "New connections" section.
    fn render_connection(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        section(
            ts!("settings.section.connection"),
            cx,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(form_row(ts!("settings.port"), self.port_input.clone()))
                .child(form_row(
                    ts!("settings.username"),
                    self.username_input.clone(),
                ))
                .child(form_row(
                    ts!("settings.keepalive"),
                    suffixed(
                        self.keepalive_input.clone(),
                        ts!("settings.keepalive_hint"),
                        cx,
                    ),
                ))
                .child(form_row(
                    ts!("settings.timeout"),
                    suffixed(self.timeout_input.clone(), ts!("settings.timeout_hint"), cx),
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
                        Button::new("settings-cancel", ts!("common.cancel"))
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
                        Button::new("settings-save", ts!("common.save"))
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
                ts!("settings.title"),
                px(DIALOG_WIDTH),
                body,
                on_dismiss,
            ))
    }
}

/// Wraps `body` in a titled card.
fn section<E: IntoElement>(title: SharedString, cx: &App, body: E) -> impl IntoElement + use<E> {
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
fn suffixed<E: IntoElement>(control: E, hint: SharedString, cx: &App) -> impl IntoElement + use<E> {
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

/// The font families the platform offers, in the order gpui reports them —
/// sorted and deduplicated already.
///
/// Names starting with a dot are dropped: those are the platform's private
/// aliases, such as `.SystemUIFont` on macOS, which are not meant to be chosen
/// by name.
fn installed_fonts(cx: &App) -> Vec<SharedString> {
    cx.text_system()
        .all_font_names()
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .map(SharedString::from)
        .collect()
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
