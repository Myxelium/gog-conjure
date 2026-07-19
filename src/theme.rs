//! Distinctive "conjure desk" look — ink, ember, and parchment accents.
//! Intentionally not stock egui gray.

use egui::{FontFamily, FontId, Stroke, Style, TextStyle, Visuals};

pub fn apply(ctx: &egui::Context) {
    let mut style = Style::default();

    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.window_margin = egui::Margin::same(16);
    style.visuals = visuals();

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(28.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    ctx.set_style(style);
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    // Deep ink background with ember accents — not purple glow, not cream/terracotta.
    let ink = egui::Color32::from_rgb(14, 18, 28);
    let panel = egui::Color32::from_rgb(24, 30, 44);
    let ember = egui::Color32::from_rgb(232, 140, 64);
    let mist = egui::Color32::from_rgb(196, 208, 222);
    let steel = egui::Color32::from_rgb(72, 88, 112);

    v.override_text_color = Some(mist);
    v.window_fill = ink;
    v.panel_fill = panel;
    v.faint_bg_color = egui::Color32::from_rgb(20, 26, 38);
    v.extreme_bg_color = egui::Color32::from_rgb(10, 12, 20);
    v.widgets.noninteractive.bg_fill = panel;
    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(34, 42, 60);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, mist);
    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(48, 58, 82);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, ember);
    v.widgets.active.bg_fill = egui::Color32::from_rgb(58, 70, 96);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, ember);
    v.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(232, 140, 64, 60);
    v.selection.stroke = Stroke::new(1.0_f32, ember);
    v.hyperlink_color = ember;
    v.warn_fg_color = ember;
    v.error_fg_color = egui::Color32::from_rgb(220, 90, 90);
    v.window_stroke = Stroke::new(1.0_f32, steel);
    v
}

pub const BRAND: &str = "gog-conjure";
pub const TAGLINE: &str = "Summon your GOG library to disk.";
