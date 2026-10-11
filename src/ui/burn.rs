use egui::{ScrollArea, TextEdit, Ui};

use crate::disc::{
    disc_needs_udf, is_local_game_id, sanitize_volid, xorriso_udf_error, AvailableDownload,
    BurnListEntry, BurnPlan, BurnedDisc, DiscBurnStatus, DiscFilesystem, DiscLayout, DiscMedia,
    DownloadReadiness, OpticalDrive, SplitPolicy, VOLID_MAX_LEN,
};
use crate::gog::format_bytes;
use crate::theme::{self, PRIMARY_BTN, SIDE_BTN};

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
    pub reburn: Option<usize>,
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
        burned_discs: &[BurnedDisc],
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
            reburn: None,
        };

        ui.horizontal(|ui| {
            ui.heading(theme::header("Burn"));
            ui.label(theme::meta("·"));
            ui.label(theme::meta(backend_name));
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
                        ui.label(theme::meta(hint));
                    }
                }
            });
        }

        ui.add_space(4.0);

        let full = ui.available_width().max(1.0);
        let left_w = (full * 0.40).clamp(280.0, 440.0).min(full);
        let body_h = ui.available_height().max(1.0);

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
            let right_w = ui.available_width().max(1.0);
            let right_h = ui.available_height().max(1.0);
            ui.allocate_ui_with_layout(
                egui::vec2(right_w, right_h),
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

                    // Progress footer must be reserved *before* the discs ScrollArea.
                    // Otherwise the scroll area eats all remaining height and the footer
                    // gets zero-sized (NaN) rects, which panics egui hit-testing.
                    let show_burn_progress =
                        burning || !burn_log.is_empty() || burn_progress.is_some();
                    let progress_footer_h = if show_burn_progress {
                        let log_h = if burn_log.is_empty() { 0.0 } else { 156.0 };
                        // Glass card padding + label + bar + optional log.
                        128.0 + log_h
                    } else {
                        0.0
                    };

                    if plan.discs.is_empty() {
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            ui.label(theme::meta("No discs yet."));
                            ui.label("Add a disc with the size you have, put games in the burn list, then Plan.");
                        });
                        show_burned_discs(ui, burned_discs, burning, &mut actions.reburn);
                    } else {
                        let mut burn_clicked = None;
                        let mut download_disc = None;
                        let mut remove_disc = None;
                        let scroll_h = (ui.available_height() - progress_footer_h).max(80.0);
                        ScrollArea::vertical()
                            .id_salt("burn_discs_scroll")
                            .max_height(scroll_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (idx, disc) in plan.discs.iter_mut().enumerate() {
                                    // Keep index aligned with Vec position (burn_active_disc uses this).
                                    disc.index = idx;
                                    let is_active = burning && burn_active_disc == Some(idx);
                                    let downloads_ready = disc.units.iter().all(|u| {
                                        burn_list
                                            .iter()
                                            .find(|e| {
                                                e.game_id == u.game_id || e.title == u.game_title
                                            })
                                            .map(|e| e.readiness == DownloadReadiness::Ready)
                                            .unwrap_or(true)
                                    });
                                    let need_download_count =
                                        disc_games_needing_download(disc, burn_list).len();
                                    let id = theme::hover_id(ui, ("disc", idx));
                                    let hovered = theme::was_hovered(ui, id);
                                    let shown =
                                        theme::glass_card(hovered, is_active).show(ui, |ui| {
                                            theme::expand_x(ui);
                                            show_disc_card(
                                                ui,
                                                disc,
                                                drives,
                                                backend_ok,
                                                unavailable,
                                                burning,
                                                is_active,
                                                plan.blockers.is_empty(),
                                                downloads_ready,
                                                need_download_count,
                                                &mut burn_clicked,
                                                &mut download_disc,
                                                &mut remove_disc,
                                                &mut actions.refresh_drives,
                                            );
                                        });
                                    theme::store_hover_rect(ui, id, shown.response.rect);
                                }
                                show_burned_discs(ui, burned_discs, burning, &mut actions.reburn);
                            });
                        actions.burn_disc = burn_clicked;
                        actions.download_disc = download_disc;
                        actions.remove_disc = remove_disc;
                    }

                    if show_burn_progress {
                        let disc_label = burn_active_disc
                            .map(|i| format!("Disc {} burn progress", i + 1))
                            .unwrap_or_else(|| "Burn progress".into());
                        let id = theme::hover_id(ui, "burn_progress");
                        let hovered = theme::was_hovered(ui, id);
                        let shown = theme::glass_card(hovered, burning).show(ui, |ui| {
                            theme::expand_x(ui);
                            ui.label(theme::header(disc_label));
                            if let Some(p) = burn_progress {
                                let p = finite_unit_interval(p);
                                let text = if burn_progress_text.is_empty() {
                                    format!("{:.0}%", p * 100.0)
                                } else {
                                    format!("{:.0}% — {burn_progress_text}", p * 100.0)
                                };
                                let bar_w = ui.available_width().max(1.0);
                                ui.add(theme::progress(p).desired_width(bar_w).text(text));
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
                        });
                        theme::store_hover_rect(ui, id, shown.response.rect);
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
        ui.label(theme::header("Downloaded"));
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
        ui.label(theme::meta("No downloaded games found yet."));
    } else {
        ScrollArea::vertical()
            .id_salt("available_dl_scroll")
            .max_height(240.0)
            .show(ui, |ui| {
                for (idx, game) in filtered {
                    let id = theme::hover_id(ui, ("available", idx));
                    let hovered = theme::was_hovered(ui, id);
                    let row = theme::glass_row(hovered, false).show(ui, |ui| {
                        theme::expand_x(ui);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.vertical(|ui| {
                                        ui.set_max_width(ui.available_width() - SIDE_BTN.x - 8.0);
                                        ui.label(theme::header(&game.title));
                                        ui.horizontal(|ui| {
                                            ui.label(theme::meta(format_bytes(game.size_bytes)));
                                            if game.burned {
                                                ui.colored_label(theme::CYAN, "Burned");
                                            } else {
                                                ui.label(theme::meta("Downloaded"));
                                            }
                                            if game.on_burn_list {
                                                ui.label(theme::meta("· in list"));
                                            }
                                        });
                                    });
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
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
                                },
                            );
                        });
                    });
                    theme::store_hover_rect(ui, id, row.response.rect);
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
        ui.label(theme::header("Burn list"));
        ui.label(theme::meta(format!("({})", burn_list.len())));
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
    ui.label(theme::meta(
        "Games here are packed onto discs when you Plan.",
    ));

    if burn_list.is_empty() {
        ui.label(theme::meta(
            "Add downloads from the list above, or use Plan / Download in the Library.",
        ));
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
                let id = theme::hover_id(ui, ("burn_list", entry.game_id));
                let hovered = theme::was_hovered(ui, id);
                let row = theme::glass_row(hovered, false).show(ui, |ui| {
                    theme::expand_x(ui);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut entry.included, "");
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width() - SIDE_BTN.x - 8.0);
                            ui.label(theme::header(&entry.title));
                            ui.horizontal(|ui| {
                                let color = match entry.readiness {
                                    DownloadReadiness::Ready => theme::CYAN,
                                    DownloadReadiness::Downloading => theme::TEXT_HEADER,
                                    DownloadReadiness::Failed => ui.visuals().error_fg_color,
                                    DownloadReadiness::Pending => theme::text_meta(),
                                };
                                ui.colored_label(color, entry.readiness.label());
                                ui.label(theme::meta(format_bytes(entry.size_bytes)));
                            });
                            ui.horizontal(|ui| {
                                ui.label(theme::meta("Split"));
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
                });
                theme::store_hover_rect(ui, id, row.response.rect);
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
        ui.label(theme::header("Discs"));
        ui.label(theme::meta(format!("({})", plan.discs.len())));

        egui::ComboBox::from_id_salt("new_disc_media")
            .selected_text(new_disc_media.short_label())
            .width(120.0)
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
        ui.label(theme::meta("Default split"));
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
/// Local (non-GOG) folders are excluded — they cannot be fetched via the API.
fn disc_games_needing_download<'a>(
    disc: &DiscLayout,
    burn_list: &'a [BurnListEntry],
) -> Vec<&'a BurnListEntry> {
    let mut seen_ids = std::collections::HashSet::new();
    let mut out = Vec::new();
    for unit in &disc.units {
        let entry = burn_list.iter().find(|e| e.game_id == unit.game_id);
        let Some(entry) = entry else {
            continue;
        };
        if is_local_game_id(entry.game_id) || entry.game_id == 0 {
            continue;
        }
        if !needs_download(entry) {
            continue;
        }
        if seen_ids.insert(entry.game_id) {
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

/// egui hit-testing panics if progress/layout values become NaN.
fn finite_unit_interval(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn show_burned_discs(
    ui: &mut Ui,
    records: &[BurnedDisc],
    burning: bool,
    reburn: &mut Option<usize>,
) {
    if records.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.separator();
    ui.label(theme::header("Burned discs"));
    ui.label(theme::meta(
        "Reburn blanks a rewritable disc and writes a full new image from the current files.",
    ));
    for (idx, record) in records.iter().enumerate() {
        let id = theme::hover_id(ui, ("burned", idx));
        let hovered = theme::was_hovered(ui, id);
        let row = theme::glass_row(hovered, false).show(ui, |ui| {
            theme::expand_x(ui);
            ui.horizontal_wrapped(|ui| {
                let volid = if record.volid.is_empty() {
                    "(no label)"
                } else {
                    record.volid.as_str()
                };
                ui.label(format!(
                    "{volid} · {} · {}",
                    record.media.short_label(),
                    record.filesystem.label()
                ));
                if !record.titles.is_empty() {
                    ui.label(theme::meta(record.titles.join(", ")));
                }
                if record.media.allows_reburn() {
                    if ui
                        .add_enabled(!burning, egui::Button::new("Reburn").min_size(SIDE_BTN))
                        .on_hover_text(
                            "Reload these games, keep this media type and filesystem, force Blank RW, and burn a new image.",
                        )
                        .clicked()
                    {
                        *reburn = Some(idx);
                    }
                } else {
                    ui.label(theme::meta("Write-once — cannot reburn"));
                }
            });
        });
        theme::store_hover_rect(ui, id, row.response.rect);
    }
}

fn show_disc_card(
    ui: &mut Ui,
    disc: &mut DiscLayout,
    drives: &[OpticalDrive],
    backend_ok: bool,
    unavailable: Option<&str>,
    burning: bool,
    is_active_burn: bool,
    plan_ok: bool,
    downloads_ready: bool,
    need_download_count: usize,
    burn_clicked: &mut Option<usize>,
    download_clicked: &mut Option<usize>,
    remove_disc: &mut Option<usize>,
    refresh_drives: &mut bool,
) {
    // Isolate widget IDs per disc (combos / buttons).
    ui.push_id(disc.index, |ui| {
        show_disc_card_inner(
            ui,
            disc,
            drives,
            backend_ok,
            unavailable,
            burning,
            is_active_burn,
            plan_ok,
            downloads_ready,
            need_download_count,
            burn_clicked,
            download_clicked,
            remove_disc,
            refresh_drives,
        );
    });
}

fn show_disc_card_inner(
    ui: &mut Ui,
    disc: &mut DiscLayout,
    drives: &[OpticalDrive],
    backend_ok: bool,
    unavailable: Option<&str>,
    burning: bool,
    is_active_burn: bool,
    plan_ok: bool,
    downloads_ready: bool,
    need_download_count: usize,
    burn_clicked: &mut Option<usize>,
    download_clicked: &mut Option<usize>,
    remove_disc: &mut Option<usize>,
    refresh_drives: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(theme::header(format!("Disc {}", disc.index + 1)).size(16.0));
        egui::ComboBox::from_id_salt(format!("media_{}", disc.index))
            .selected_text(disc.media.short_label())
            .width(120.0)
            .show_ui(ui, |ui| {
                for m in DiscMedia::all() {
                    if ui.selectable_label(disc.media == *m, m.label()).clicked() {
                        if *m != disc.media && m.is_mdisc() {
                            disc.options.blank = false;
                        }
                        disc.media = *m;
                        disc.recompute_usage();
                    }
                }
            });
        let status_text = if is_active_burn {
            "Burning…"
        } else if burning {
            "Waiting…"
        } else {
            disc.status.label()
        };
        if is_active_burn {
            ui.colored_label(theme::CYAN, status_text);
        } else {
            ui.label(theme::meta(status_text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!burning, egui::Button::new("Remove").min_size(SIDE_BTN))
                .clicked()
            {
                *remove_disc = Some(disc.index);
            }
        });
    });

    // Capacity as text only — ProgressBar widgets were mistaken for burn progress
    // (and can visually "update" on every burn log repaint).
    ui.label(theme::meta(disc.used_label()));
    let fill = finite_unit_interval(disc.fill_fraction());
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), 8.0),
        egui::Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        theme::paint_meter(ui.painter(), rect, fill);
    }

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
            disc.volid = sanitize_volid(&edit).chars().take(VOLID_MAX_LEN).collect();
            disc.volid_manual = true;
        }
        ui.label(theme::meta(format!(
            "{}/{}",
            disc.volid.len(),
            VOLID_MAX_LEN
        )));
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

    // IMAPI cannot store a file ≥ 2 GiB in Joliet. xorriso can: ISO 9660 level 3
    // raises the file size limit, and xorriso has no UDF writer at all.
    let xorriso_no_udf = cfg!(target_os = "linux");
    let needs_udf = disc_needs_udf(disc) && !xorriso_no_udf;
    if needs_udf {
        disc.options.filesystem = DiscFilesystem::Udf;
    }
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut disc.options.verify, "Verify")
            .on_hover_text("Read back the disc after writing (MD5 check)");
        ui.checkbox(&mut disc.options.simulate, "Simulate")
            .on_hover_text(
                "Dry-run: build a temporary ISO only (does not write or eject the optical drive)",
            );
        let fs_response = egui::ComboBox::from_id_salt(format!("fs_{}", disc.index))
            .selected_text(disc.options.filesystem.label())
            .width(80.0)
            .show_ui(ui, |ui| {
                ui.add_enabled_ui(!needs_udf, |ui| {
                    ui.selectable_value(
                        &mut disc.options.filesystem,
                        DiscFilesystem::Joliet,
                        "Joliet",
                    );
                });
                ui.add_enabled_ui(!xorriso_no_udf, |ui| {
                    ui.selectable_value(&mut disc.options.filesystem, DiscFilesystem::Udf, "UDF");
                });
            });
        if needs_udf {
            fs_response.response.on_hover_text(
                "A file on this disc is ≥ 2 GiB. Joliet cannot store it, so UDF is required.",
            );
        } else if xorriso_no_udf {
            fs_response.response.on_hover_text(
                "xorriso does not write UDF. Joliet is the Linux filesystem, including for files ≥ 2 GiB.",
            );
        }
        let blank_tip = if disc.media.is_mdisc() {
            "M-DISC DVD is write-once and cannot be erased for reuse."
        } else {
            "Blank rewriteable media before writing (ignored in Simulate)"
        };
        ui.checkbox(&mut disc.options.blank, "Blank RW")
            .on_hover_text(blank_tip);
        ui.checkbox(&mut disc.options.eject, "Eject")
            .on_hover_text("Eject after a real burn (ignored in Simulate)");
        if disc.media.is_bluray() {
            let prev = disc.options.defect_management;
            ui.checkbox(&mut disc.options.defect_management, "Defect spare")
                .on_hover_text(
                    "Format BD with Defect Management spare area. More resilient, but ~1–3 GB less capacity and about half write speed. Re-Plan after changing.",
                );
            if disc.options.defect_management != prev {
                disc.recompute_usage();
            }
        }
    });
    if needs_udf {
        ui.label(theme::meta(
            "Joliet is off because a file on this disc is ≥ 2 GiB. UDF is required.",
        ));
    } else if xorriso_no_udf && disc.options.filesystem == DiscFilesystem::Udf {
        ui.label(theme::meta(xorriso_udf_error()));
    }

    if is_active_burn {
        ui.label(theme::header("Burn in progress — see Burn progress below.").color(theme::CYAN));
    }

    if disc.units.is_empty() {
        ui.label(theme::meta(
            "Empty — Plan will fill this disc from the burn list.",
        ));
    } else {
        for unit in &disc.units {
            ui.horizontal(|ui| {
                ui.label(unit.summary_label());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(theme::meta(format_bytes(unit.size_bytes)));
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
        let burn_id = theme::hover_id(ui, ("burn_cta", disc.index));
        let burn_hover = can_burn && theme::was_hovered(ui, burn_id);
        let burn_response = ui
            .add_enabled(
                can_burn,
                theme::primary_button(label, burn_hover).min_size(PRIMARY_BTN),
            )
            .on_hover_text(tip);
        theme::store_hover_rect(ui, burn_id, burn_response.rect);
        if burn_response.clicked() {
            *burn_clicked = Some(disc.index);
        }
    });
}
