use egui::{ProgressBar, ScrollArea, Ui};

use crate::download::{JobStatus, QueueItem};
use crate::gog::format_bytes;

pub struct QueuePanel;

impl QueuePanel {
    pub fn show(
        ui: &mut Ui,
        items: &[QueueItem],
        on_cancel: &mut dyn FnMut(u64),
        on_clear: &mut dyn FnMut(),
    ) {
        ui.horizontal(|ui| {
            ui.heading("Queue");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear finished").clicked() {
                    on_clear();
                }
            });
        });
        ui.separator();

        if items.is_empty() {
            ui.weak("Queue is empty — select files on a game and add them.");
            return;
        }

        ScrollArea::vertical().id_salt("queue_scroll").show(ui, |ui| {
            for item in items {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(&item.game_title).strong());
                    ui.label(&item.file.name);
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
                        JobStatus::Failed => format!(
                            "failed: {}",
                            item.error.as_deref().unwrap_or("unknown")
                        ),
                        JobStatus::Cancelled => "cancelled".to_string(),
                    };
                    ui.add(ProgressBar::new(fraction).text(text));
                    if matches!(item.status, JobStatus::Queued | JobStatus::Running)
                        && ui.button("Cancel").clicked()
                    {
                        on_cancel(item.id);
                    }
                });
                ui.add_space(6.0);
            }
        });
    }
}
