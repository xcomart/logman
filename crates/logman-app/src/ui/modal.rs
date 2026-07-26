//! Centered dialog rendered on top of a translucent backdrop.

use gpui::{AnyElement, App, ElementId, Pixels, SharedString, Window, div, prelude::*, px};

use super::theme::theme;

/// Callback fired when the backdrop is clicked.
type DismissHandler = Box<dyn Fn(&mut Window, &mut App)>;

/// Builds a modal dialog.
///
/// The returned element positions itself absolutely, so it must be rendered
/// inside a `relative()` ancestor that spans the window — typically the root
/// element of the view — and it should be the last child so that it paints on
/// top of everything else.
///
/// Clicks on the panel itself are swallowed; only clicks on the backdrop invoke
/// `on_dismiss`.
///
/// ```ignore
/// modal("connect", "New connection", px(420.), body, cx.listener(..))
/// ```
pub fn modal<E: IntoElement>(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    width: Pixels,
    body: E,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    Modal {
        id: id.into(),
        title: title.into(),
        width,
        body: body.into_any_element(),
        on_dismiss: Box::new(on_dismiss),
    }
}

/// Backing element of [`modal`].
#[derive(IntoElement)]
struct Modal {
    id: ElementId,
    title: SharedString,
    width: Pixels,
    body: AnyElement,
    on_dismiss: DismissHandler,
}

impl RenderOnce for Modal {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);
        let on_dismiss = self.on_dismiss;

        div()
            .id(self.id.clone())
            .absolute()
            .inset_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .on_click(move |_, window, cx| on_dismiss(window, cx))
            .child(
                div()
                    .id(ElementId::from((self.id, "panel")))
                    .occlude()
                    .flex()
                    .flex_col()
                    .w(self.width)
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .shadow_lg()
                    .text_color(theme.text)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .px(px(16.))
                            .h(px(44.))
                            .border_b_1()
                            .border_color(theme.border)
                            .text_size(px(14.))
                            .child(self.title),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .p(px(16.))
                            .child(self.body),
                    ),
            )
    }
}

/// Builds a labelled form row: a fixed-width label followed by `control`.
///
/// Intended for the body of a [`modal`], but usable anywhere a label/control
/// pair is needed.
pub fn form_row<E: IntoElement>(label: impl Into<SharedString>, control: E) -> impl IntoElement {
    FormRow {
        label: label.into(),
        control: control.into_any_element(),
    }
}

/// Backing element of [`form_row`].
#[derive(IntoElement)]
struct FormRow {
    label: SharedString,
    control: AnyElement,
}

impl RenderOnce for FormRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = theme(cx);

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(self.label),
            )
            .child(div().flex_grow().min_w_0().child(self.control))
    }
}
