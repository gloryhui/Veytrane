//! Reusable GPUI components and text input primitives.

pub mod components;
pub mod text_actions;
pub mod text_field;
pub mod theme;

pub fn format_bytes(value: f64) -> String {
    let value = if value.is_finite() { value.max(0.) } else { 0. };
    if value >= 1024. * 1024. * 1024. {
        format!("{:.1} GiB", value / (1024. * 1024. * 1024.))
    } else if value >= 1024. * 1024. {
        format!("{:.1} MiB", value / (1024. * 1024.))
    } else if value >= 1024. {
        format!("{:.1} KiB", value / 1024.)
    } else {
        format!("{value:.0} B")
    }
}
