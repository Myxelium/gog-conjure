use std::collections::HashSet;

use egui::{Image, ScrollArea, TextureHandle, Ui, Vec2};

use crate::gog::{file_label, format_bytes, DownloadFile, FileKind, GameDetails, LibraryGame};
use crate::images::{cover_url_candidates, CoverSize, ImageCache};

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
        ui.heading("Game");
        if loading {
            ui.spinner();
            ui.label("Loading details…");
            return;
        }

        let Some(details) = details else {
            ui.label("Select a game from the library.");
            return;
        };

        ui.horizontal(|ui| {
            let cover_src = library_game
                .and_then(|g| g.image.as_deref())
                .or(details.image.as_deref());
            let candidates = cover_src
                .map(|img| cover_url_candidates(img, CoverSize::Large))
                .unwrap_or_default();

            for url in &candidates {
                request_image(url.clone());
            }

            if let Some(tex) = candidates.iter().find_map(|u| images.texture(u)) {
                let size = fit_cover(tex, 220.0);
                ui.add(Image::new(tex).fit_to_exact_size(size));
                ui.add_space(12.0);
            } else if candidates.iter().any(|u| images.is_pending(u)) {
                ui.add_sized([140.0, 220.0], egui::Spinner::new());
                ui.add_space(12.0);
            }

            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&details.title).size(22.0).strong());
                if let Some(game) = library_game {
                    if let Some(slug) = &game.slug {
                        ui.weak(slug);
                    }
                }
                ui.add_space(8.0);
                ui.label(format!(
                    "{} installer file(s) · {} extra(s)",
                    details.installers.len(),
                    details.extras.len()
                ));
            });
        });

        ui.add_space(8.0);

        ScrollArea::vertical()
            .id_salt("detail_scroll")
            .max_height(ui.available_height() - 80.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Installers / versions").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("All installers").clicked() {
                            for file in &details.installers {
                                selected.insert(file.id.clone());
                            }
                        }
                        if ui.small_button("None").clicked() {
                            for file in &details.installers {
                                selected.remove(&file.id);
                            }
                        }
                    });
                });
                ui.small("Check the OS / language builds you want.");
                ui.add_space(4.0);
                for file in &details.installers {
                    draw_file_checkbox(ui, file, selected);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Extras (optional)").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("All extras").clicked() {
                            for file in &details.extras {
                                selected.insert(file.id.clone());
                            }
                        }
                    });
                });
                ui.small("Manuals, soundtracks, and other bonuses.");
                ui.add_space(4.0);
                if details.extras.is_empty() {
                    ui.weak("No extras listed for this title.");
                } else {
                    for file in &details.extras {
                        draw_file_checkbox(ui, file, selected);
                    }
                }
            });

        ui.separator();
        let selected_files: Vec<DownloadFile> = details
            .installers
            .iter()
            .chain(details.extras.iter())
            .filter(|f| selected.contains(&f.id))
            .cloned()
            .collect();
        let total: u64 = selected_files.iter().map(|f| f.size).sum();
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} file(s) · {}",
                selected_files.len(),
                format_bytes(total)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !selected_files.is_empty();
                if ui
                    .add_enabled(enabled, egui::Button::new("Add to queue"))
                    .clicked()
                {
                    on_queue(selected_files);
                }
            });
        });
    }
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
