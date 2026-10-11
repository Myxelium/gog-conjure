use std::collections::HashSet;

use egui::{Image, ScrollArea, TextureHandle, Ui, Vec2};

use crate::gog::{file_label, format_bytes, DownloadFile, FileKind, GameDetails, LibraryGame};
use crate::images::{cover_url_candidates, CoverSize, ImageCache};
use crate::theme;

pub struct GameDetailPanel;

impl GameDetailPanel {
    pub fn show(
        ui: &mut Ui,
        details: Option<&GameDetails>,
        library_game: Option<&LibraryGame>,
        selected: &mut HashSet<String>,
        loading: bool,
        images: &mut ImageCache,
        request_image: &mut dyn FnMut(String),
        on_queue: &mut dyn FnMut(Vec<DownloadFile>),
    ) {
        ui.heading(theme::header("Game"));
        if loading {
            ui.spinner();
            ui.label("Loading details…");
            return;
        }

        let Some(details) = details else {
            ui.label(theme::meta("Select a game from the library."));
            return;
        };

        let hero_id = theme::hover_id(ui, "detail_hero");
        let hero_hover = theme::was_hovered(ui, hero_id);
        let cover_src = library_game
            .and_then(|g| g.image.as_deref())
            .or(details.image.as_deref());
        let candidates = cover_src
            .map(|img| cover_url_candidates(img, CoverSize::Large))
            .unwrap_or_default();
        let fetch = images.next_to_fetch(&candidates).map(str::to_string);
        let cover_tex = images.texture_any(&candidates).cloned();
        let waiting = cover_tex.is_none() && (fetch.is_some() || images.any_pending(&candidates));
        if let Some(url) = fetch {
            request_image(url);
        }
        let wash = cover_tex.as_ref().map(|_| theme::reserve_art_wash(ui));
        let hero = theme::glass_card(hero_hover, false).show(ui, |ui| {
            theme::expand_x(ui);
            ui.horizontal(|ui| {
                if let Some(tex) = &cover_tex {
                    let size = fit_cover(tex, 220.0);
                    ui.add(Image::new(tex).fit_to_exact_size(size).corner_radius(8));
                    ui.add_space(4.0);
                } else if waiting {
                    ui.add_sized([140.0, 220.0], egui::Spinner::new());
                    ui.add_space(4.0);
                }

                ui.vertical(|ui| {
                    ui.label(theme::header(&details.title).size(22.0));
                    if let Some(game) = library_game {
                        if let Some(slug) = &game.slug {
                            ui.label(theme::meta(slug));
                        }
                    }
                    ui.label(theme::meta(format!(
                        "{} installer file(s) · {} extra(s)",
                        details.installers.len(),
                        details.extras.len()
                    )));
                });
            });
        });
        if let (Some(slot), Some(tex)) = (wash, cover_tex.as_ref()) {
            theme::commit_art_wash(ui, slot, hero.response.rect, tex, theme::CARD_RADIUS);
        }
        theme::store_hover_rect(ui, hero_id, hero.response.rect);

        let footer_h = 96.0;
        ScrollArea::vertical()
            .id_salt("detail_scroll")
            .max_height((ui.available_height() - footer_h).max(80.0))
            .show(ui, |ui| {
                section_card(ui, "installers", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::header("Installers / versions"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("All installers").clicked() {
                                for file in &details.installers {
                                    selected.insert(file.id.clone());
                                }
                            }
                            if ui.button("None").clicked() {
                                for file in &details.installers {
                                    selected.remove(&file.id);
                                }
                            }
                        });
                    });
                    ui.label(theme::meta("Check the OS / language builds you want."));
                    for file in &details.installers {
                        draw_file_checkbox(ui, file, selected);
                    }
                });

                section_card(ui, "extras", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::header("Extras (optional)"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("All extras").clicked() {
                                for file in &details.extras {
                                    selected.insert(file.id.clone());
                                }
                            }
                        });
                    });
                    ui.label(theme::meta("Manuals, soundtracks, and other bonuses."));
                    if details.extras.is_empty() {
                        ui.label(theme::meta("No extras listed for this title."));
                    } else {
                        for file in &details.extras {
                            draw_file_checkbox(ui, file, selected);
                        }
                    }
                });
            });

        let selected_files: Vec<DownloadFile> = details
            .installers
            .iter()
            .chain(details.extras.iter())
            .filter(|f| selected.contains(&f.id))
            .cloned()
            .collect();
        let total: u64 = selected_files.iter().map(|f| f.size).sum();
        let bar_id = theme::hover_id(ui, "detail_queue_bar");
        let bar_hover = theme::was_hovered(ui, bar_id);
        let bar = theme::glass_row(bar_hover, false).show(ui, |ui| {
            theme::expand_x(ui);
            ui.horizontal(|ui| {
                ui.label(theme::meta(format!(
                    "{} file(s) · {}",
                    selected_files.len(),
                    format_bytes(total)
                )));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let enabled = !selected_files.is_empty();
                    let id = theme::hover_id(ui, "add_to_queue");
                    let hovered = enabled && theme::was_hovered(ui, id);
                    let response =
                        ui.add_enabled(enabled, theme::primary_button("Add to queue", hovered));
                    theme::store_hover_rect(ui, id, response.rect);
                    if response.clicked() {
                        on_queue(selected_files);
                    }
                });
            });
        });
        theme::store_hover_rect(ui, bar_id, bar.response.rect);
    }
}

fn section_card(ui: &mut Ui, salt: &str, add: impl FnOnce(&mut Ui)) {
    let id = theme::hover_id(ui, salt);
    let hovered = theme::was_hovered(ui, id);
    let shown = theme::glass_card(hovered, false).show(ui, |ui| {
        theme::expand_x(ui);
        add(ui);
    });
    theme::store_hover_rect(ui, id, shown.response.rect);
}

fn draw_file_checkbox(ui: &mut Ui, file: &DownloadFile, selected: &mut HashSet<String>) {
    let mut checked = selected.contains(&file.id);
    let label = match file.kind {
        FileKind::Extra => format!("[extra] {}", file_label(file)),
        _ => file_label(file),
    };
    if ui.checkbox(&mut checked, label).changed() {
        if checked {
            selected.insert(file.id.clone());
        } else {
            selected.remove(&file.id);
        }
    }
}

fn fit_cover(tex: &TextureHandle, height: f32) -> Vec2 {
    let size = tex.size_vec2();
    if size.y <= 0.0 {
        return Vec2::new(height * 0.66, height);
    }
    let scale = height / size.y;
    Vec2::new((size.x * scale).clamp(80.0, 180.0), height)
}
