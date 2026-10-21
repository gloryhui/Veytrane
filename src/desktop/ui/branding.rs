//! Shared application identity for both desktop entry points.
use super::theme as ui_theme;
use gpui::{div, prelude::*, px, rgb};

pub(in crate::desktop) fn brand_identity(title: &'static str, subtitle: &'static str) -> gpui::Div {
    static ICON: std::sync::OnceLock<std::sync::Arc<gpui::Image>> = std::sync::OnceLock::new();
    let icon = ICON
        .get_or_init(|| {
            std::sync::Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                include_bytes!("../../../assets/icons/app-icon-ui.png").to_vec(),
            ))
        })
        .clone();
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(gpui::img(icon).w(px(40.)).h(px(40.)).flex_shrink_0())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(24.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(ui_theme::TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(ui_theme::TEXT_SECONDARY))
                        .child(subtitle),
                ),
        )
}
