use std::collections::HashSet;

use egui::{
    Color32, CornerRadius, Frame, Image, Label, Margin, RichText, ScrollArea, Sense, TextEdit,
    TextureHandle, Ui, Vec2,
};

use crate::gog::LibraryGame;
use crate::images::{cover_url_candidates, CoverSize, ImageCache};

pub struct LibraryPanel;

pub struct LibraryActions {
    pub select_all_filtered: bool,
    pub clear_checks: bool,
    pub queue_checked: bool,
}

impl LibraryPanel {
    pub fn show(
        ui: &mut Ui,
        games: &[LibraryGame],
        filter: &mut String,
        selected_id: &mut Option<u64>,
        checked: &mut HashSet<u64>,
        images: &mut ImageCache,
        request_image: &mut dyn FnMut(String),
    ) -> LibraryActions {
        let mut actions = LibraryActions {
            select_all_filtered: false,
            clear_checks: false,
            queue_checked: false,
        };

        ui.heading("Library");
        ui.add(
            TextEdit::singleline(filter)
                .hint_text("Search owned games…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);

        let filter_lower = filter.to_lowercase();
        let filtered: Vec<&LibraryGame> = games
            .iter()
            .filter(|g| filter_lower.is_empty() || g.title.to_lowercase().contains(&filter_lower))
            .collect();

        ui.horizontal(|ui| {
            ui.label(format!("{} games", filtered.len()));
            if !checked.is_empty() {
                ui.weak(format!("· {} selected", checked.len()));
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Select visible").clicked() {
                actions.select_all_filtered = true;
            }
            if ui.small_button("Clear checks").clicked() {
                actions.clear_checks = true;
            }
            let can_queue = !checked.is_empty();
            if ui
                .add_enabled(can_queue, egui::Button::new("Queue selected"))
                .on_hover_text("Download all files for every checked game")
                .clicked()
            {
                actions.queue_checked = true;
            }
        });
        ui.separator();

        // Keep rows inside the panel width so long titles wrap instead of stretching it.
        let panel_width = ui.available_width();

        ScrollArea::vertical()
            .id_salt("library_scroll")
            .max_width(panel_width)
            .show(ui, |ui| {
                ui.set_max_width(panel_width);
                for game in filtered {
                    let row_selected = selected_id == &Some(game.id);
                    let mut is_checked = checked.contains(&game.id);

                    ui.horizontal(|ui| {
                        ui.set_max_width(panel_width);

                        // Checkbox stays independent of row selection.
                        if ui.checkbox(&mut is_checked, "").changed() {
                            if is_checked {
                                checked.insert(game.id);
                            } else {
                                checked.remove(&game.id);
                            }
                        }

                        let fill = if row_selected {
                            ui.visuals().selection.bg_fill
                        } else {
                            Color32::TRANSPARENT
                        };

                        let row = Frame::new()
                            .fill(fill)
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(6, 4))
                            .show(ui, |ui| {
                                // Remaining width after checkbox.
                                ui.set_max_width(ui.available_width());
                                ui.horizontal_top(|ui| {
                                    ui.set_max_width(ui.available_width());

                                    let candidates = game
                                        .image
                                        .as_deref()
                                        .map(|img| cover_url_candidates(img, CoverSize::Thumb))
                                        .unwrap_or_default();

                                    for url in &candidates {
                                        request_image(url.clone());
                                    }

                                    if let Some(tex) =
                                        candidates.iter().find_map(|u| images.texture(u))
                                    {
                                        let size = fit_thumb(tex, 40.0);
                                        ui.add(Image::new(tex).fit_to_exact_size(size));
                                    } else if candidates.iter().any(|u| images.is_pending(u)) {
                                        ui.add_sized([40.0, 40.0], egui::Spinner::new());
                                    } else {
                                        ui.add_sized([40.0, 40.0], egui::Label::new("·"));
                                    }

                                    ui.add_space(6.0);

                                    let title = RichText::new(&game.title);
                                    let title = if row_selected {
                                        title.strong()
                                    } else {
                                        title
                                    };
                                    ui.add(
                                        Label::new(title)
                                            .wrap()
                                            .selectable(false),
                                    );
                                });
                            });

                        let response = row.response.interact(Sense::click());
                        if response.hovered() && !row_selected {
                            ui.painter().rect_filled(
                                response.rect,
                                CornerRadius::same(4),
                                ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.5),
                            );
                        }
                        if response.clicked() {
                            *selected_id = Some(game.id);
                        }
                    });
                }
            });

        actions
    }
}

fn fit_thumb(tex: &TextureHandle, height: f32) -> Vec2 {
    let size = tex.size_vec2();
    if size.y <= 0.0 {
        return Vec2::splat(height);
    }
    let scale = height / size.y;
    Vec2::new((size.x * scale).clamp(28.0, 48.0), height)
}
