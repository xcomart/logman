//! Color palette used by every widget in [`crate::ui`].
//!
//! The theme is stored as a gpui [`Global`], so any widget that has access to an
//! [`App`] reference can read it without threading it through its constructor.

use gpui::{App, Global, Hsla, hsla};

/// A flat set of semantic colors.
///
/// Widgets never hardcode colors; they always resolve them through a `Theme` so
/// that swapping [`Theme::dark`] for [`Theme::light`] restyles the whole app.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Window / app background.
    pub background: Hsla,
    /// Background of raised chrome such as panels, toolbars and the tab bar.
    pub surface: Hsla,
    /// Surface color while the pointer hovers an interactive element.
    pub surface_hover: Hsla,
    /// Surface color while an interactive element is pressed or selected.
    pub surface_active: Hsla,
    /// Hairline separators and control outlines.
    pub border: Hsla,
    /// Primary foreground color.
    pub text: Hsla,
    /// Secondary foreground color for hints, placeholders and inactive labels.
    pub text_muted: Hsla,
    /// Brand color used for the active tab, focus rings and primary buttons.
    pub accent: Hsla,
    /// Destructive actions and error states.
    pub danger: Hsla,
    /// Successful / connected states.
    pub success: Hsla,
    /// Translucent backdrop painted behind modal dialogs (includes alpha).
    pub overlay: Hsla,
}

impl Theme {
    /// The default dark theme, in the spirit of One Dark.
    pub fn dark() -> Self {
        Self {
            background: hsla(220. / 360., 0.13, 0.18, 1.0),
            surface: hsla(220. / 360., 0.13, 0.14, 1.0),
            surface_hover: hsla(220. / 360., 0.13, 0.23, 1.0),
            surface_active: hsla(220. / 360., 0.13, 0.28, 1.0),
            border: hsla(220. / 360., 0.13, 0.31, 1.0),
            text: hsla(219. / 360., 0.14, 0.78, 1.0),
            text_muted: hsla(220. / 360., 0.09, 0.55, 1.0),
            accent: hsla(207. / 360., 0.82, 0.66, 1.0),
            danger: hsla(355. / 360., 0.65, 0.65, 1.0),
            success: hsla(95. / 360., 0.38, 0.62, 1.0),
            overlay: hsla(220. / 360., 0.13, 0.06, 0.62),
        }
    }

    /// A light counterpart to [`Theme::dark`].
    pub fn light() -> Self {
        Self {
            background: hsla(0., 0.0, 1.0, 1.0),
            surface: hsla(220. / 360., 0.16, 0.96, 1.0),
            surface_hover: hsla(220. / 360., 0.16, 0.91, 1.0),
            surface_active: hsla(220. / 360., 0.16, 0.86, 1.0),
            border: hsla(220. / 360., 0.13, 0.80, 1.0),
            text: hsla(220. / 360., 0.16, 0.20, 1.0),
            text_muted: hsla(220. / 360., 0.10, 0.45, 1.0),
            accent: hsla(212. / 360., 0.76, 0.46, 1.0),
            danger: hsla(355. / 360., 0.66, 0.46, 1.0),
            success: hsla(120. / 360., 0.45, 0.33, 1.0),
            overlay: hsla(220. / 360., 0.13, 0.35, 0.40),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Global for Theme {}

/// Returns the active theme, falling back to [`Theme::dark`] when the app has
/// not installed one yet.
///
/// A clone is returned rather than a borrow so that callers can keep using the
/// [`App`] mutably while styling their elements.
pub fn theme(cx: &App) -> Theme {
    cx.try_global::<Theme>().cloned().unwrap_or_default()
}

/// Installs `theme` as the active [`Theme`] global.
pub fn set_theme(theme: Theme, cx: &mut App) {
    cx.set_global(theme);
}

/// Returns `color` with its lightness shifted by `delta`, clamped to `[0, 1]`.
///
/// Used by widgets to derive hover / pressed shades from a base color without
/// having to store one entry per state in [`Theme`].
pub fn shift_lightness(color: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (color.l + delta).clamp(0.0, 1.0),
        ..color
    }
}
