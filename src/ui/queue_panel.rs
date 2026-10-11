use egui::{ScrollArea, Ui};

use crate::download::{JobStatus, QueueItem};
use crate::gog::format_bytes;
use crate::theme;

pub struct QueuePanel;

impl QueuePanel {
    pub fn show(
        ui: &mut Ui,
        items: &[QueueItem],
        on_cancel: &mut dyn FnMut(u64),
        on_clear: &mut dyn FnMut(),
    ) {
        ui.horizontal(|ui| {
            ui.heading(theme::header("Queue"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear finished").clicked() {
                    on_clear();
                }
            });
        });

        if items.is_empty() {
            ui.label(theme::meta(
                "Queue is empty — select files on a game and add them.",
            ));
            return;
        }

        ScrollArea::vertical()
            .id_salt("queue_scroll")
            .show(ui, |ui| {
                for item in items {
                    let id = theme::hover_id(ui, item.id);
                    let hovered = theme::was_hovered(ui, id);
                    let active = item.status == JobStatus::Running;
                    let can_cancel = matches!(item.status, JobStatus::Queued | JobStatus::Running);
                    let row = theme::glass_row(hovered, active).show(ui, |ui| {
                        theme::expand_x(ui);
                        ui.label(theme::header(&item.game_title));
                        ui.label(theme::meta(&item.file.name));
                        let fraction = if item.total > 0 {
                            (item.downloaded as f32 / item.total as f32).clamp(0.0, 1.0)
                        } else if item.status == JobStatus::Completed {
                            1.0
                        } else {
                            0.0
                        };
                        let text = match item.status {
                            JobStatus::Queued => "queued".to_string(),
                            JobStatus::Running => format!(
                                "{} / {}",
                                format_bytes(item.downloaded),
                                format_bytes(item.total)
                            ),
                            JobStatus::Completed => "done".to_string(),
                            JobStatus::Failed => {
                                format!("failed: {}", item.error.as_deref().unwrap_or("unknown"))
                            }
                            JobStatus::Cancelled => "cancelled".to_string(),
                        };
                        let mut bar = theme::progress(fraction).text(text);
                        if item.status == JobStatus::Failed {
                            bar = bar.fill(ui.visuals().error_fg_color);
                        }
                        ui.add(bar);
                        if can_cancel && ui.button("Cancel").clicked() {
                            on_cancel(item.id);
                        }
                    });
                    theme::store_hover_rect(ui, id, row.response.rect);
                }
            });
    }
}
