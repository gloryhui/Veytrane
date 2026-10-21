//! Small GPUI building blocks shared by the single-page desktop workbench.

use gpui::{CursorStyle, Div, FontWeight, SharedString, Stateful, div, prelude::*, px, rgb, white};

use super::theme;

#[derive(Clone, Copy)]
pub(in crate::desktop) enum StatusTone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

pub(in crate::desktop) fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p(px(16.))
        .bg(rgb(theme::SURFACE))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .rounded_xl()
        .flex_shrink_0()
}

pub(in crate::desktop) fn section_header(
    icon: &'static str,
    title: &'static str,
    subtitle: &'static str,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(36.))
                .h(px(36.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .bg(rgb(theme::PRIMARY_SOFT))
                .text_color(rgb(theme::PRIMARY))
                .text_size(px(20.))
                .font_weight(FontWeight::BOLD)
                .child(icon),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(theme::TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(subtitle),
                ),
        )
}

pub(in crate::desktop) fn primary_button(
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    let label = label.into();
    let button = div()
        .id(label.clone())
        .h(px(40.))
        .px(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .text_sm()
        .font_weight(FontWeight::BOLD)
        .child(label);
    if enabled {
        button
            .bg(rgb(theme::PRIMARY))
            .border_color(rgb(theme::PRIMARY))
            .text_color(white())
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(rgb(theme::PRIMARY_HOVER)))
            .active(|style| style.bg(rgb(theme::PRIMARY_PRESSED)))
    } else {
        button
            .bg(rgb(theme::DISABLED_BG))
            .border_color(rgb(theme::DISABLED_BG))
            .text_color(rgb(theme::TEXT_MUTED))
            .cursor(CursorStyle::Arrow)
    }
}

pub(in crate::desktop) fn secondary_button(label: impl Into<SharedString>, enabled: bool) -> Div {
    let button = div()
        .h(px(36.))
        .px(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .text_sm()
        .child(label.into());
    if enabled {
        button
            .bg(white())
            .text_color(rgb(theme::TEXT))
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(rgb(theme::SURFACE_SUBTLE)))
    } else {
        button
            .bg(rgb(theme::DISABLED_BG))
            .text_color(rgb(theme::TEXT_MUTED))
            .cursor(CursorStyle::Arrow)
    }
}

pub(in crate::desktop) fn compact_secondary_button(
    label: impl Into<SharedString>,
    enabled: bool,
) -> Div {
    let button = div()
        .h(px(25.))
        .px(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .text_size(px(9.8))
        .child(label.into());
    if enabled {
        button
            .bg(white())
            .text_color(rgb(theme::TEXT))
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(rgb(theme::SURFACE_SUBTLE)))
    } else {
        button
            .bg(rgb(theme::DISABLED_BG))
            .text_color(rgb(theme::TEXT_MUTED))
            .cursor(CursorStyle::Arrow)
    }
}

pub(in crate::desktop) fn status_badge(label: impl Into<SharedString>, tone: StatusTone) -> Div {
    let (dot, background, foreground) = match tone {
        StatusTone::Neutral => (
            theme::TEXT_MUTED,
            theme::SURFACE_SUBTLE,
            theme::TEXT_SECONDARY,
        ),
        StatusTone::Info => (theme::PRIMARY, theme::PRIMARY_SOFT, theme::PRIMARY),
        StatusTone::Success => (theme::SUCCESS, theme::SUCCESS_SOFT, theme::SUCCESS),
        StatusTone::Warning => (theme::WARNING, theme::WARNING_SOFT, theme::WARNING),
        StatusTone::Danger => (theme::DANGER, theme::DANGER_SOFT, theme::DANGER),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(12.))
        .py(px(8.))
        .rounded_md()
        .bg(rgb(background))
        .text_sm()
        .text_color(rgb(foreground))
        .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(rgb(dot)))
        .child(label.into())
}
