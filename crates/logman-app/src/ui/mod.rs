//! Reusable gpui widgets shared by every logman view.
//!
//! The module is deliberately free of SSH or terminal concepts: it only knows
//! about colors ([`theme`]), text entry ([`text_input`]), buttons ([`button`]),
//! session tabs ([`tab_bar`]) and dialogs ([`modal`]).
//!
//! Call [`init`] once during application start-up so the widgets that need key
//! bindings get them.

pub mod button;
pub mod checkbox;
pub mod modal;
pub mod segmented;
pub mod tab_bar;
pub mod text_input;
pub mod theme;

pub use button::{Button, ButtonVariant};
pub use checkbox::Checkbox;
pub use modal::{form_row, modal};
pub use segmented::Segmented;
pub use tab_bar::{TabBar, TabItem, TabStatus};
pub use text_input::TextInput;
pub use theme::{Theme, set_theme, theme};

use gpui::App;

/// Registers everything the widget layer needs before the first window opens.
pub fn init(cx: &mut App) {
    set_theme(Theme::dark(), cx);
    TextInput::init(cx);
}
