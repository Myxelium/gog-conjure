use std::collections::HashSet;

use egui::{Image, Label, ScrollArea, Sense, TextEdit, TextureHandle, Ui, Vec2};

use crate::gog::LibraryGame;
use crate::images::{cover_url_candidates, CoverSize, ImageCache};
use crate::theme;

pub struct LibraryPanel;

pub struct LibraryActions {
    pub select_all_filtered: bool,
    pub clear_checks: bool,
    pub download_selected: bool,
    pub plan_selected: bool,
    /// Games inside the scroll viewport, plus a few rows of overscan.
    pub visible_ids: Vec<u64>,
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
            download_selected: false,
            plan_selected: false,
            visible_ids: Vec::new(),
        };

        ui.heading(theme::header("Library"));
        ui.add(
            TextEdit::singleline(filter)
                .hint_text("Search owned games…")
                .desired_width(f32::INFINITY),
        );

        let filter_lower = filter.to_lowercase();
        let filtered: Vec<&LibraryGame> = games
            .iter()
            .filter(|g| filter_lower.is_empty() || g.title.to_lowercase().contains(&filter_lower))
            .collect();

        ui.horizontal(|ui| {
            ui.label(format!("{} games", filtered.len()));
            if !checked.is_empty() {
                ui.label(theme::meta(format!("· {} selected", checked.len())));
            }
        });

        ui.horizontal_wrapped(|ui| {
            if ui.button("Select visible").clicked() {
                actions.select_all_filtered = true;
            }
            if ui.button("Clear checks").clicked() {
                actions.clear_checks = true;
            }
            let can_act = !checked.is_empty();
            let download_label = if checked.len() > 1 {
                "Download selected"
            } else {
                "Download"
            };
            let download_id = theme::hover_id(ui, "lib_download");
            let download_hover = can_act && theme::was_hovered(ui, download_id);
            let download = ui
                .add_enabled(
                    can_act,
                    theme::primary_button(download_label, download_hover),
                )
                .on_hover_text("Download all files for every checked game");
            theme::store_hover_rect(ui, download_id, download.rect);
            if download.clicked() {
                actions.download_selected = true;
            }
            if ui
                .add_enabled(can_act, egui::Button::new("Plan"))
                .on_hover_text(
                    "Estimate how many discs are needed from GOG file sizes, then add to Burn",
                )
                .clicked()
            {
                actions.plan_selected = true;
            }
        });

        // Keep rows inside the panel width so long titles wrap instead of stretching it.
        // Only visible rows are built. Building every owned game requested every cover
        // at startup, and decoding those images on the UI thread froze the window.
        let panel_width = ui.available_width();
        let row_height = theme::game_row_outer_height(ROW_CONTENT_H);

        ScrollArea::vertical()
            .id_salt("library_scroll")
            .max_width(panel_width)
            .show_rows(ui, row_height, filtered.len(), |ui, range| {
                ui.set_max_width(panel_width);
                let start = range.start.saturating_sub(PRELOAD_OVERSCAN);
                let end = (range.end + PRELOAD_OVERSCAN).min(filtered.len());
                for idx in start..end {
                    let Some(game) = filtered.get(idx) else {
                        continue;
                    };
                    actions.visible_ids.push(game.id);
                    if !range.contains(&idx) {
                        enqueue_thumb(images, game, request_image);
                    }
                }
                for idx in range {
                    let Some(game) = filtered.get(idx) else {
                        continue;
                    };
                    let width = ui.available_width();
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, row_height), Sense::empty());
                    let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    // `show_rows` starts one stride above the viewport. Keep the scroll clip
                    // so that row does not draw over the controls above the list.
                    row_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                    if show_game_row(&mut row_ui, game, checked, images, request_image) {
                        if checked.contains(&game.id) {
                            checked.remove(&game.id);
                        } else {
                            checked.insert(game.id);
                        }
                        *selected_id = Some(game.id);
                    }
                }
            });

        actions
    }
}

const ROW_CONTENT_H: f32 = 88.0;
const PRELOAD_OVERSCAN: usize = 3;

fn enqueue_thumb(images: &ImageCache, game: &LibraryGame, request_image: &mut dyn FnMut(String)) {
    let Some(image) = game.image.as_deref() else {
        return;
    };
    let candidates = cover_url_candidates(image, CoverSize::Thumb);
    if let Some(url) = images.next_to_fetch(&candidates) {
        request_image(url.to_string());
    }
}

fn show_game_row(
    ui: &mut Ui,
    game: &LibraryGame,
    checked: &HashSet<u64>,
    images: &ImageCache,
    request_image: &mut dyn FnMut(String),
) -> bool {
    let is_checked = checked.contains(&game.id);

    let candidates = game
        .image
        .as_deref()
        .map(|img| cover_url_candidates(img, CoverSize::Thumb))
        .unwrap_or_default();
    let fetch = images.next_to_fetch(&candidates).map(str::to_string);
    let tex = images.texture_any(&candidates).cloned();
    let waiting = tex.is_none() && (fetch.is_some() || images.any_pending(&candidates));
    if let Some(url) = fetch {
        request_image(url);
    }
    let wash = tex.as_ref().map(|_| theme::reserve_art_wash(ui));

    let row = theme::game_row_frame().show(ui, |ui| {
        theme::expand_x(ui);
        ui.set_height(ROW_CONTENT_H);
        ui.horizontal(|ui| {
            ui.set_max_width(ui.available_width());

            if let Some(tex) = &tex {
                let size = fit_thumb(tex, 84.0);
                ui.add(Image::new(tex).fit_to_exact_size(size).corner_radius(0));
            } else if waiting {
                ui.add_sized([84.0, 84.0], egui::Spinner::new());
            } else {
                ui.add_sized([84.0, 84.0], Label::new(theme::meta("·")).selectable(false));
            }

            let text_w = ui.available_width().max(72.0);
            ui.vertical(|ui| {
                ui.set_max_width(text_w);
                ui.add(Label::new(theme::header(&game.title)).wrap());
            });
        });
    });

    if let (Some(slot), Some(tex)) = (wash, tex.as_ref()) {
        theme::commit_art_wash(ui, slot, row.response.rect, tex, 0);
    }

    let strip = theme::check_strip_rect(row.response.rect);
    let response = ui.interact(
        strip,
        theme::hover_id(ui, game.id).with("check"),
        Sense::click(),
    );
    let hover_t = theme::ease_hover(
        ui,
        theme::hover_id(ui, game.id).with("check_ease"),
        response.hovered(),
    );
    theme::paint_check_strip(ui.painter(), strip, hover_t, is_checked);
    response.clicked()
}

fn fit_thumb(tex: &TextureHandle, height: f32) -> Vec2 {
    let size = tex.size_vec2();
    if size.y <= 0.0 {
        return Vec2::splat(height);
    }
    let scale = height / size.y;
    Vec2::new((size.x * scale).clamp(64.0, 128.0), height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gog::LibraryGame;

    #[test]
    fn only_visible_rows_request_covers() {
        let games: Vec<LibraryGame> = (0..300)
            .map(|id| LibraryGame {
                id,
                title: format!("Game {id}"),
                image: Some(format!("img{id}")),
                slug: None,
            })
            .collect();
        let mut requested = Vec::new();
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(420.0, 760.0),
        ));
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut filter = String::new();
                let mut selected = None;
                let mut checked = HashSet::new();
                let mut images = ImageCache::default();
                LibraryPanel::show(
                    ui,
                    &games,
                    &mut filter,
                    &mut selected,
                    &mut checked,
                    &mut images,
                    &mut |url| requested.push(url),
                );
            });
        });
        assert!(
            !requested.is_empty(),
            "visible rows should start loading covers"
        );
        assert!(
            requested.len() < 30,
            "requested {} covers for 300 games",
            requested.len()
        );
    }
}
