use egui::{RichText, ScrollArea, TextEdit, Ui};

use crate::disc::{
    sanitize_volid, AvailableDownload, BurnListEntry, BurnPlan, DiscBurnStatus, DiscLayout,
    DiscMedia, DownloadReadiness, OpticalDrive, SplitPolicy, VOLID_MAX_LEN,
};
use crate::gog::format_bytes;
use crate::theme::{PRIMARY_BTN, SIDE_BTN};

pub struct BurnPanel;

pub struct BurnPanelActions {
    pub plan: bool,
    pub clear_list: bool,
    pub refresh_drives: bool,
    pub refresh_available: bool,
    pub add_disc: bool,
    pub remove_disc: Option<usize>,
    pub burn_disc: Option<usize>,
    pub download_disc: Option<usize>,
    pub remove_from_list: Option<u64>,
    pub add_available: Option<usize>,
    /// Indices into `available` for bulk add (respects current filter).
    pub add_all_available: Vec<usize>,
    pub install_xorriso: bool,
    pub new_disc_media: DiscMedia,
}

impl BurnPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        ui: &mut Ui,
        available: &[AvailableDownload],
        available_filter: &mut String,
        new_disc_media: &mut DiscMedia,
        global_split: &mut SplitPolicy,
        burn_list: &mut [BurnListEntry],
        plan: &mut BurnPlan,
        drives: &[OpticalDrive],
        backend_name: &str,
        backend_ok: bool,
        unavailable: Option<&str>,
        burning: bool,
        burn_active_disc: Option<usize>,
        burn_log: &str,
        burn_progress: Option<f32>,
        burn_progress_text: &str,
        installing_xorriso: bool,
        can_install_xorriso: bool,
        install_hint: Option<&str>,
    ) -> BurnPanelActions {
        let mut actions = BurnPanelActions {
            plan: false,
            clear_list: false,
            refresh_drives: false,
            refresh_available: false,
            add_disc: false,
            remove_disc: None,
            burn_disc: None,
            download_disc: None,
            remove_from_list: None,
            add_available: None,
            add_all_available: Vec::new(),
            install_xorriso: false,
            new_disc_media: *new_disc_media,
        };

        ui.horizontal(|ui| {
            ui.heading("Burn");
            ui.weak("·");
            ui.weak(backend_name);
        });
        if let Some(reason) = unavailable {
            ui.colored_label(ui.visuals().warn_fg_color, reason);
            ui.horizontal(|ui| {
                if installing_xorriso {
                    ui.spinner();
                    ui.label("Installing xorriso (approve the system password prompt if shown)…");
                } else if can_install_xorriso {
                    if ui
                        .button("Install xorriso")
                        .on_hover_text(
                            install_hint
                                .unwrap_or("Install the xorriso package with administrator rights"),
                        )
                        .clicked()
                    {
                        actions.install_xorriso = true;
                    }
                    if let Some(hint) = install_hint {
                        ui.weak(hint);
                    }
                }
            });
        }

        ui.add_space(4.0);

        let full = ui.available_width();
        let left_w = (full * 0.40).clamp(280.0, 440.0);
        let body_h = ui.available_height();

        ui.horizontal_top(|ui| {
            // ── Left: library of downloads + burn queue ──
            ui.allocate_ui_with_layout(
                egui::vec2(left_w, body_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_height(body_h);
                    show_available(
                        ui,
                        available,
                        available_filter,
                        &mut actions,
                        burning,
                    );
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    show_burn_queue(ui, burn_list, global_split, &mut actions, burning);
                },
            );

            ui.separator();

            // ── Right: discs ──
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    show_discs_header(
                        ui,
                        new_disc_media,
                        global_split,
                        plan,
                        burn_list,
                        &mut actions,
                        burning,
                    );

                    if !plan.blockers.is_empty() {
                        for b in &plan.blockers {
                            ui.colored_label(ui.visuals().error_fg_color, format!("• {b}"));
                        }
                        ui.add_space(4.0);
                    }
                    if !plan.warnings.is_empty() {
                        for w in &plan.warnings {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("• {w}"));
                        }
                        ui.add_space(4.0);
                    }

                    if plan.discs.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.weak("No discs yet.");
                            ui.label("Add a disc with the size you have, put games in the burn list, then Plan.");
                        });
                    } else {
                        let mut burn_clicked = None;
                        let mut download_disc = None;
                        let mut remove_disc = None;
                        ScrollArea::vertical()
                            .id_salt("burn_discs_scroll")
                            .show(ui, |ui| {
                                for disc in &mut plan.discs {
                                    let idx = disc.index;
                                    egui::Frame::group(ui.style())
                                        .inner_margin(egui::Margin::same(10))
                                        .show(ui, |ui| {
                                            let is_active = burning
                                                && burn_active_disc == Some(disc.index);
                                            let downloads_ready = disc.units.iter().all(|u| {
                                                burn_list
                                                    .iter()
                                                    .find(|e| {
                                                        e.game_id == u.game_id
                                                            || e.title == u.game_title
                                                    })
                                                    .map(|e| {
                                                        e.readiness == DownloadReadiness::Ready
                                                    })
                                                    .unwrap_or(true)
                                            });
                                            let need_download_count = disc_games_needing_download(
                                                disc, burn_list,
                                            )
                                            .len();
                                            show_disc_card(
                                                ui,
                                                disc,
                                                drives,
                                                backend_ok,
                                                unavailable,
                                                burning,
                                                is_active,
                                                burn_progress,
                                                burn_progress_text,
                                                plan.blockers.is_empty(),
                                                downloads_ready,
                                                need_download_count,
                                                &mut burn_clicked,
                                                &mut download_disc,
                                                &mut remove_disc,
                                                &mut actions.refresh_drives,
                                            );
                                        });
                                    ui.add_space(8.0);
                                    let _ = idx;
                                }
                            });
                        actions.burn_disc = burn_clicked;
                        actions.download_disc = download_disc;
                        actions.remove_disc = remove_disc;
                    }

                    if burning || !burn_log.is_empty() || burn_progress.is_some() {
                        ui.separator();
                        ui.label(RichText::new("Burn progress").strong());
                        if let Some(p) = burn_progress {
                            let text = if burn_progress_text.is_empty() {
                                format!("{:.0}%", p.clamp(0.0, 1.0) * 100.0)
                            } else {
                                format!(
                                    "{:.0}% — {}",
                                    p.clamp(0.0, 1.0) * 100.0,
                                    burn_progress_text
                                )
                            };
                            ui.add(
                                egui::ProgressBar::new(p.clamp(0.0, 1.0))
                                    .desired_width(f32::INFINITY)
                                    .text(text),
                            );
                        } else if burning {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Starting…");
                            });
                        }
                        if !burn_log.is_empty() {
                            ScrollArea::vertical()
                                .id_salt("burn_log_scroll")
                                .max_height(140.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    ui.monospace(burn_log);
                                });
                        }
                    }
                },
            );
        });

        actions.new_disc_media = *new_disc_media;
        actions
    }
}

fn show_available(
    ui: &mut Ui,
    available: &[AvailableDownload],
    filter: &mut String,
    actions: &mut BurnPanelActions,
    burning: bool,
) {
    let filter_l = filter.to_lowercase();
    let filtered: Vec<(usize, &AvailableDownload)> = available
        .iter()
        .enumerate()
        .filter(|(_, a)| filter_l.is_empty() || a.title.to_lowercase().contains(&filter_l))
        .collect();
    let addable: Vec<usize> = filtered
        .iter()
        .filter(|(_, a)| !a.on_burn_list)
        .map(|(idx, _)| *idx)
        .collect();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Downloaded").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("Refresh").min_size(SIDE_BTN))
                .on_hover_text("Rescan download folder")
                .clicked()
            {
                actions.refresh_available = true;
            }
            if ui
                .add_enabled(
                    !burning && !addable.is_empty(),
                    egui::Button::new("Add all").min_size(SIDE_BTN),
                )
                .on_hover_text("Add all visible downloads that are not already on the burn list")
                .clicked()
            {
                actions.add_all_available = addable.clone();
            }
        });
    });
    ui.add(
        TextEdit::singleline(filter)
            .hint_text("Filter downloads…")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(4.0);

    if filtered.is_empty() {
        ui.weak("No downloaded games found yet.");
    } else {
        ScrollArea::vertical()
            .id_salt("available_dl_scroll")
            .max_height(220.0)
            .show(ui, |ui| {
                for (idx, game) in filtered {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.vertical(|ui| {
                                ui.set_max_width(ui.available_width() - SIDE_BTN.x - 8.0);
                                ui.label(RichText::new(&game.title).strong());
                                ui.horizontal(|ui| {
                                    ui.weak(format_bytes(game.size_bytes));
                                    if game.burned {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(80, 160, 100),
                                            "Burned",
                                        );
                                    } else {
                                        ui.weak("Downloaded");
                                    }
                                    if game.on_burn_list {
                                        ui.weak("· in list");
                                    }
                                });
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = if game.on_burn_list { "Re-add" } else { "Add" };
                            if ui
                                .add_enabled(
                                    !burning,
                                    egui::Button::new(label).min_size(SIDE_BTN),
                                )
                                .on_hover_text("Add this game to the burn list")
                                .clicked()
                            {
                                actions.add_available = Some(idx);
                            }
                        });
                    });
                    ui.separator();
                }
            });
    }
}

fn show_burn_queue(
    ui: &mut Ui,
    burn_list: &mut [BurnListEntry],
    global_split: &mut SplitPolicy,
    actions: &mut BurnPanelActions,
    burning: bool,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Burn list").strong());
        ui.weak(format!("({})", burn_list.len()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !burn_list.is_empty() && !burning,
                    egui::Button::new("Clear").min_size(SIDE_BTN),
                )
                .clicked()
            {
                actions.clear_list = true;
            }
        });
    });
    ui.weak("Games here are packed onto discs when you Plan.");
    ui.add_space(4.0);

    if burn_list.is_empty() {
        ui.weak(
            "Add downloads from the list above, or use Plan / Download in the Library.",
        );
        return;
    }

    // Use remaining left-column height so the list isn't a tiny cramped box.
    let list_height = (ui.available_height() - 8.0).max(180.0);
    ScrollArea::vertical()
        .id_salt("burn_queue_scroll")
        .max_height(list_height)
        .show(ui, |ui| {
            let mut remove = None;
            for entry in burn_list.iter_mut() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut entry.included, "");
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - SIDE_BTN.x - 8.0);
                        ui.label(RichText::new(&entry.title).strong());
                        ui.horizontal(|ui| {
                            let color = match entry.readiness {
                                DownloadReadiness::Ready => ui.visuals().text_color(),
                                DownloadReadiness::Failed => ui.visuals().error_fg_color,
                                _ => ui.visuals().weak_text_color(),
                            };
                            ui.colored_label(color, entry.readiness.label());
                            ui.weak(format_bytes(entry.size_bytes));
                        });
                        ui.horizontal(|ui| {
                            ui.weak("Split");
                            let mut policy = entry.split_override.unwrap_or(*global_split);
                            let prev = policy;
                            egui::ComboBox::from_id_salt(format!("qsplit_{}", entry.game_id))
                                .selected_text(policy.label())
                                .width(170.0)
                                .show_ui(ui, |ui| {
                                    for p in SplitPolicy::all() {
                                        ui.selectable_value(&mut policy, *p, p.label());
                                    }
                                });
                            if policy != prev {
                                entry.split_override = Some(policy);
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        if ui
                            .add_enabled(
                                !burning,
                                egui::Button::new("Remove").min_size(SIDE_BTN),
                            )
                            .on_hover_text("Remove from burn list")
                            .clicked()
                        {
                            remove = Some(entry.game_id);
                        }
                    });
                });
                ui.separator();
            }
            actions.remove_from_list = remove;
        });
}

fn show_discs_header(
    ui: &mut Ui,
    new_disc_media: &mut DiscMedia,
    global_split: &mut SplitPolicy,
    plan: &BurnPlan,
    burn_list: &[BurnListEntry],
    actions: &mut BurnPanelActions,
    burning: bool,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Discs").strong());
        ui.weak(format!("({})", plan.discs.len()));

        egui::ComboBox::from_id_salt("new_disc_media")
            .selected_text(new_disc_media.short_label())
            .width(90.0)
            .show_ui(ui, |ui| {
                for m in DiscMedia::all() {
                    ui.selectable_value(new_disc_media, *m, m.label());
                }
            });

        if ui
            .add_enabled(!burning, egui::Button::new("Add disc"))
            .on_hover_text("Add an empty disc with the selected media size")
            .clicked()
        {
            actions.add_disc = true;
        }

        let can_plan = !plan.discs.is_empty() && burn_list.iter().any(|e| e.included);
        if ui
            .add_enabled(can_plan && !burning, egui::Button::new("Plan"))
            .on_hover_text(
                "Organize the burn list onto your discs using GOG/file sizes (downloads need not be finished yet)",
            )
            .clicked()
        {
            actions.plan = true;
        }
    });

    ui.horizontal(|ui| {
        ui.weak("Default split");
        egui::ComboBox::from_id_salt("global_split")
            .selected_text(global_split.label())
            .show_ui(ui, |ui| {
                for p in SplitPolicy::all() {
                    ui.selectable_value(global_split, *p, p.label());
                }
            });
    });
}

#[allow(clippy::too_many_arguments)]
/// Games on a disc that are neither Ready nor currently Downloading.
fn disc_games_needing_download<'a>(
    disc: &DiscLayout,
    burn_list: &'a [BurnListEntry],
) -> Vec<&'a BurnListEntry> {
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_titles = std::collections::HashSet::new();
    let mut out = Vec::new();
    for unit in &disc.units {
        let entry = burn_list.iter().find(|e| {
            (unit.game_id != 0 && e.game_id == unit.game_id) || e.title == unit.game_title
        });
        let Some(entry) = entry else {
            continue;
        };
        if !needs_download(entry) {
            continue;
        }
        let dup = if entry.game_id != 0 {
            !seen_ids.insert(entry.game_id)
        } else {
            !seen_titles.insert(entry.title.as_str())
        };
        if !dup {
            out.push(entry);
        }
    }
    out
}

fn needs_download(entry: &BurnListEntry) -> bool {
    !matches!(
        entry.readiness,
        DownloadReadiness::Ready | DownloadReadiness::Downloading
    )
}

fn show_disc_card(
    ui: &mut Ui,
    disc: &mut DiscLayout,
    drives: &[OpticalDrive],
    backend_ok: bool,
    unavailable: Option<&str>,
    burning: bool,
    is_active_burn: bool,
    burn_progress: Option<f32>,
    burn_progress_text: &str,
    plan_ok: bool,
    downloads_ready: bool,
    need_download_count: usize,
    burn_clicked: &mut Option<usize>,
    download_clicked: &mut Option<usize>,
    remove_disc: &mut Option<usize>,
    refresh_drives: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("Disc {}", disc.index + 1))
                .strong()
                .size(16.0),
        );
        egui::ComboBox::from_id_salt(format!("media_{}", disc.index))
            .selected_text(disc.media.short_label())
            .width(90.0)
            .show_ui(ui, |ui| {
                for m in DiscMedia::all() {
                    if ui
                        .selectable_label(disc.media == *m, m.label())
                        .clicked()
                    {
                        disc.media = *m;
                        disc.recompute_usage();
                    }
                }
            });
        ui.weak(disc.status.label());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!burning, egui::Button::new("Remove").min_size(SIDE_BTN))
                .clicked()
            {
                *remove_disc = Some(disc.index);
            }
        });
    });

    ui.add(egui::ProgressBar::new(disc.fill_fraction()).text(disc.used_label()));

    ui.horizontal(|ui| {
        ui.label("Volume");
        let mut edit = disc.volid.clone();
        if ui
            .add(
                TextEdit::singleline(&mut edit)
                    .desired_width(180.0)
                    .hint_text("AUTO"),
            )
            .changed()
        {
            disc.volid = sanitize_volid(&edit)
                .chars()
                .take(VOLID_MAX_LEN)
                .collect();
            disc.volid_manual = true;
        }
        ui.weak(format!("{}/{}", disc.volid.len(), VOLID_MAX_LEN));
    });

    // Per-disc burn settings
    ui.horizontal_wrapped(|ui| {
        let drive_text = if disc.options.drive.is_empty() {
            "(drive)".to_string()
        } else {
            drives
                .iter()
                .find(|d| d.path == disc.options.drive)
                .map(|d| d.label())
                .unwrap_or_else(|| disc.options.drive.clone())
        };
        egui::ComboBox::from_id_salt(format!("drive_{}", disc.index))
            .selected_text(drive_text)
            .width(200.0)
            .show_ui(ui, |ui| {
                for d in drives {
                    ui.selectable_value(&mut disc.options.drive, d.path.clone(), d.label());
                }
            });
        if ui
            .add(egui::Button::new("↻").min_size(egui::vec2(SIDE_BTN.y, SIDE_BTN.y)))
            .on_hover_text("Refresh drives")
            .clicked()
        {
            *refresh_drives = true;
        }

        let speed_label = disc
            .options
            .speed
            .map(|s| format!("{s}x"))
            .unwrap_or_else(|| "Auto".into());
        egui::ComboBox::from_id_salt(format!("speed_{}", disc.index))
            .selected_text(speed_label)
            .width(70.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut disc.options.speed, None, "Auto");
                for s in [1u32, 2, 4, 6, 8, 12, 16] {
                    ui.selectable_value(&mut disc.options.speed, Some(s), format!("{s}x"));
                }
            });
    });

    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut disc.options.verify, "Verify")
            .on_hover_text("Read back the disc after writing (MD5 check)");
        ui.checkbox(&mut disc.options.simulate, "Simulate")
            .on_hover_text(
                "Dry-run: build a temporary ISO only (does not write or eject the optical drive)",
            );
        ui.checkbox(&mut disc.options.blank, "Blank RW")
            .on_hover_text("Blank rewriteable media before writing (ignored in Simulate)");
        ui.checkbox(&mut disc.options.eject, "Eject")
            .on_hover_text("Eject after a real burn (ignored in Simulate)");
    });

    if is_active_burn {
        ui.add_space(6.0);
        ui.label(RichText::new("Burning…").strong());
        let p = burn_progress.unwrap_or(0.0).clamp(0.0, 1.0);
        let text = if burn_progress_text.is_empty() {
            format!("{:.0}%", p * 100.0)
        } else {
            format!("{:.0}% — {burn_progress_text}", p * 100.0)
        };
        ui.add(
            egui::ProgressBar::new(p)
                .desired_width(f32::INFINITY)
                .animate(true)
                .text(text),
        );
    }

    if disc.units.is_empty() {
        ui.weak("Empty — Plan will fill this disc from the burn list.");
    } else {
        ui.add_space(2.0);
        for unit in &disc.units {
            ui.horizontal(|ui| {
                ui.label(unit.summary_label());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format_bytes(unit.size_bytes));
                });
            });
        }
    }

    if let Some(err) = &disc.last_error {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }

    let can_burn = backend_ok
        && plan_ok
        && downloads_ready
        && !disc.units.is_empty()
        && !disc.options.drive.is_empty()
        && !burning
        && !matches!(disc.status, DiscBurnStatus::Burning);

    let label = match disc.status {
        DiscBurnStatus::Done | DiscBurnStatus::Failed => "Reburn",
        _ => "Burn",
    };

    let tip = if !backend_ok {
        unavailable.unwrap_or("Burn backend unavailable")
    } else if !plan_ok {
        "Resolve blockers first"
    } else if disc.units.is_empty() {
        "Plan games onto this disc first"
    } else if !downloads_ready {
        "Wait for required downloads to finish before burning"
    } else if disc.options.drive.is_empty() {
        "Select a drive for this disc"
    } else if burning {
        "A burn is already in progress"
    } else {
        "Write this disc now"
    };

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if need_download_count > 0 {
            let dl_label = if need_download_count == 1 {
                "Download".to_string()
            } else {
                format!("Download ({need_download_count})")
            };
            if ui
                .add_enabled(
                    !burning,
                    egui::Button::new(dl_label).min_size(PRIMARY_BTN),
                )
                .on_hover_text(
                    "Queue downloads for games on this disc that are not downloaded or already downloading",
                )
                .clicked()
            {
                *download_clicked = Some(disc.index);
            }
        }
        if ui
            .add_enabled(
                can_burn,
                egui::Button::new(RichText::new(label).strong()).min_size(PRIMARY_BTN),
            )
            .on_hover_text(tip)
            .clicked()
        {
            *burn_clicked = Some(disc.index);
        }
    });
}
