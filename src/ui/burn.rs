use egui::{ScrollArea, Ui};

use crate::disc::{DiscMedia, DiscPack};
use crate::gog::format_bytes;

pub struct BurnPanel;

impl BurnPanel {
    pub fn show(
        ui: &mut Ui,
        media: &mut DiscMedia,
        pack: &Option<DiscPack>,
        on_suggest: &mut dyn FnMut(),
    ) {
        ui.heading("Disc burn");
        ui.label("Plan packs for DVD / Blu-ray. Automatic burning comes later.");
        ui.add_space(8.0);

        egui::ComboBox::from_label("Media")
            .selected_text(media.label())
            .show_ui(ui, |ui| {
                for m in DiscMedia::all() {
                    ui.selectable_value(media, *m, m.label());
                }
            });

        ui.add_space(6.0);
        if ui.button("Suggest games that fit").clicked() {
            on_suggest();
        }

        ui.separator();

        match pack {
            None => {
                ui.weak(
                    "Suggestions use folders already under your download root \
                     (each game in its own folder).",
                );
            }
            Some(pack) => {
                ui.label(egui::RichText::new(pack.used_label()).strong());
                ui.small(format!(
                    "{} game(s) selected for {}",
                    pack.selected.len(),
                    pack.media.label()
                ));
                ui.add_space(6.0);
                ScrollArea::vertical()
                    .id_salt("burn_scroll")
                    .show(ui, |ui| {
                        for game in &pack.selected {
                            ui.horizontal(|ui| {
                                ui.label(&game.title);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(format_bytes(game.size_bytes));
                                    },
                                );
                            });
                        }
                    });
                ui.add_space(8.0);
                ui.add_enabled(false, egui::Button::new("Burn disc (coming soon)"));
            }
        }
    }
}
