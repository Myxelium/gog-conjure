use egui::{ComboBox, RichText, ScrollArea, Ui};

use crate::disc::{BurnPlan, DiscMedia};
use crate::gog::{format_bytes, DownloadFile, GameDetails};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrasFilter {
    All,
    None,
}

impl ExtrasFilter {
    pub fn label(self) -> &'static str {
        match self {
            ExtrasFilter::All => "All",
            ExtrasFilter::None => "None",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadWhen {
    Now,
    Later,
}

impl DownloadWhen {
    pub fn label(self) -> &'static str {
        match self {
            DownloadWhen::Now => "Download now",
            DownloadWhen::Later => "Do it later",
        }
    }
}

/// "All" sentinel for OS / language combos.
pub const FILTER_ALL: &str = "All";

#[derive(Debug, Clone)]
pub struct LibraryPlanState {
    pub open: bool,
    pub media: DiscMedia,
    pub os: String,
    pub language: String,
    pub extras: ExtrasFilter,
    pub download_when: DownloadWhen,
    pub loading: bool,
    pub error: Option<String>,
    /// Raw details fetched for checked games (before filter).
    pub details: Vec<GameDetails>,
    pub available_os: Vec<String>,
    pub available_languages: Vec<String>,
    pub preview: Option<BurnPlan>,
    pub preview_total_bytes: u64,
    pub game_count: usize,
}

impl Default for LibraryPlanState {
    fn default() -> Self {
        Self {
            open: false,
            media: DiscMedia::default_for_new(),
            os: FILTER_ALL.into(),
            language: FILTER_ALL.into(),
            extras: ExtrasFilter::All,
            download_when: DownloadWhen::Now,
            loading: false,
            error: None,
            details: Vec::new(),
            available_os: Vec::new(),
            available_languages: Vec::new(),
            preview: None,
            preview_total_bytes: 0,
            game_count: 0,
        }
    }
}

impl LibraryPlanState {
    pub fn reset_for_open(&mut self, media: DiscMedia, prefer_os: &str, game_count: usize) {
        self.open = true;
        self.media = media;
        self.os = if prefer_os.is_empty() {
            FILTER_ALL.into()
        } else {
            prefer_os.into()
        };
        self.language = FILTER_ALL.into();
        self.extras = ExtrasFilter::All;
        self.download_when = DownloadWhen::Now;
        self.loading = true;
        self.error = None;
        self.details.clear();
        self.available_os.clear();
        self.available_languages.clear();
        self.preview = None;
        self.preview_total_bytes = 0;
        self.game_count = game_count;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.loading = false;
        self.details.clear();
        self.preview = None;
        self.error = None;
    }

    /// Refresh OS/language dropdown options from loaded details (languages honor OS filter).
    pub fn refresh_filter_options(&mut self) {
        let (os, langs) = collect_filter_options(&self.details, Some(&self.os));
        self.available_os = os;
        self.available_languages = langs;
        if self.language != FILTER_ALL
            && !self
                .available_languages
                .iter()
                .any(|l| l == &self.language)
        {
            self.language = FILTER_ALL.into();
        }
    }
}

pub struct PlanModalActions {
    pub cancel: bool,
    pub add_to_burn: bool,
    pub filters_changed: bool,
}

pub struct PlanModal;

impl PlanModal {
    pub fn show(ctx: &egui::Context, state: &mut LibraryPlanState) -> PlanModalActions {
        let mut actions = PlanModalActions {
            cancel: false,
            add_to_burn: false,
            filters_changed: false,
        };

        if !state.open {
            return actions;
        }

        let mut open = state.open;
        let center = ctx.screen_rect().center();
        egui::Window::new("Plan discs")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .movable(true)
            .default_pos(center)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Plan {} selected game(s) onto identical discs using GOG file sizes.",
                    state.game_count.max(state.details.len())
                ));
                ui.add_space(8.0);

                // Avoid Grid+ComboBox: popup height can be clipped to one row.
                let combo_w = 220.0;

                ui.horizontal(|ui| {
                    ui.label("Disc media");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let prev = state.media;
                        ComboBox::from_id_salt("plan_media")
                            .width(combo_w)
                            .selected_text(state.media.label())
                            .show_ui(ui, |ui| {
                                for m in DiscMedia::all() {
                                    ui.selectable_value(&mut state.media, *m, m.label());
                                }
                            });
                        if state.media != prev {
                            actions.filters_changed = true;
                        }
                    });
                });

                ui.horizontal(|ui| {
                    ui.label("Operating system");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let prev = state.os.clone();
                        let os_options = state.available_os.clone();
                        ComboBox::from_id_salt("plan_os")
                            .width(combo_w)
                            .selected_text(&state.os)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut state.os, FILTER_ALL.into(), FILTER_ALL);
                                for os in os_options {
                                    ui.selectable_value(&mut state.os, os.clone(), os);
                                }
                            });
                        if state.os != prev {
                            state.refresh_filter_options();
                            actions.filters_changed = true;
                        }
                    });
                });

                ui.horizontal(|ui| {
                    ui.label("Language");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let prev = state.language.clone();
                        let lang_options = state.available_languages.clone();
                        let lang_hint = if lang_options.is_empty() {
                            "No language tags on installers"
                        } else {
                            ""
                        };
                        ComboBox::from_id_salt("plan_lang")
                            .width(combo_w)
                            .selected_text(&state.language)
                            .show_ui(ui, |ui| {
                                ui.set_min_width(combo_w);
                                ui.selectable_value(
                                    &mut state.language,
                                    FILTER_ALL.into(),
                                    FILTER_ALL,
                                );
                                for lang in lang_options {
                                    ui.selectable_value(&mut state.language, lang.clone(), lang);
                                }
                            })
                            .response
                            .on_hover_text(lang_hint);
                        if state.language != prev {
                            actions.filters_changed = true;
                        }
                    });
                });
                if !state.available_languages.is_empty() {
                    ui.weak(format!(
                        "{} language(s) available for current OS filter",
                        state.available_languages.len()
                    ));
                }

                ui.horizontal(|ui| {
                    ui.label("Extras / DLCs");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let prev = state.extras;
                        ComboBox::from_id_salt("plan_extras")
                            .width(combo_w)
                            .selected_text(state.extras.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut state.extras,
                                    ExtrasFilter::All,
                                    ExtrasFilter::All.label(),
                                );
                                ui.selectable_value(
                                    &mut state.extras,
                                    ExtrasFilter::None,
                                    ExtrasFilter::None.label(),
                                );
                            });
                        if state.extras != prev {
                            actions.filters_changed = true;
                        }
                    });
                });

                ui.horizontal(|ui| {
                    ui.label("Download");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ComboBox::from_id_salt("plan_download")
                            .width(combo_w)
                            .selected_text(state.download_when.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut state.download_when,
                                    DownloadWhen::Now,
                                    DownloadWhen::Now.label(),
                                );
                                ui.selectable_value(
                                    &mut state.download_when,
                                    DownloadWhen::Later,
                                    DownloadWhen::Later.label(),
                                );
                            });
                    });
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                if state.loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Fetching game sizes from GOG…");
                    });
                } else if let Some(err) = &state.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                } else if let Some(plan) = &state.preview {
                    show_preview(ui, plan, state.preview_total_bytes);
                } else {
                    ui.weak("No preview yet.");
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        actions.cancel = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_add = !state.loading
                            && state.error.is_none()
                            && state
                                .preview
                                .as_ref()
                                .is_some_and(|p| !p.discs.is_empty() && p.blockers.is_empty());
                        if ui
                            .add_enabled(can_add, egui::Button::new("Add to burn"))
                            .on_hover_text("Create these discs on the Burn tab")
                            .clicked()
                        {
                            actions.add_to_burn = true;
                        }
                    });
                });
            });

        if !open {
            actions.cancel = true;
        }

        actions
    }
}

fn show_preview(ui: &mut Ui, plan: &BurnPlan, total_bytes: u64) {
    let filled = plan.discs.iter().filter(|d| !d.units.is_empty()).count();
    ui.label(
        RichText::new(format!(
            "{filled} disc(s) · {} total",
            format_bytes(total_bytes)
        ))
        .strong(),
    );

    if !plan.blockers.is_empty() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("{} blocker(s) — cannot add to burn", plan.blockers.len()),
        );
    }
    if !plan.warnings.is_empty() {
        ui.weak(format!("{} notice(s)", plan.warnings.len()));
    }

    ScrollArea::vertical()
        .max_height(180.0)
        .id_salt("plan_modal_preview")
        .show(ui, |ui| {
            for disc in &plan.discs {
                ui.group(|ui| {
                    ui.label(format!(
                        "Disc {} · {} · {}",
                        disc.index + 1,
                        disc.media.short_label(),
                        disc.used_label()
                    ));
                    for title in disc.game_titles() {
                        ui.weak(format!("· {title}"));
                    }
                });
            }
            for b in &plan.blockers {
                ui.colored_label(ui.visuals().error_fg_color, b);
            }
            for w in plan.warnings.iter().take(6) {
                ui.weak(w);
            }
        });
}

/// Filter installers (and optionally extras) by OS / language.
pub fn filter_details_files(
    details: &GameDetails,
    os: &str,
    language: &str,
    extras: ExtrasFilter,
) -> Vec<DownloadFile> {
    let os_all = os == FILTER_ALL;
    let lang_all = language == FILTER_ALL;

    let mut files: Vec<DownloadFile> = details
        .installers
        .iter()
        .filter(|f| {
            let os_ok = os_all || f.os.as_deref() == Some(os);
            let lang_ok = lang_all
                || f.language
                    .as_deref()
                    .is_some_and(|l| l.eq_ignore_ascii_case(language));
            os_ok && lang_ok
        })
        .cloned()
        .collect();

    if extras == ExtrasFilter::All {
        files.extend(details.extras.iter().cloned());
    }

    files
}

/// Collect OS values and languages present on installers.
/// When `os_filter` is a concrete OS (not All), only languages with installers for that OS are listed.
pub fn collect_filter_options(
    details: &[GameDetails],
    os_filter: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut os_set = std::collections::BTreeSet::new();
    let mut lang_set = std::collections::BTreeSet::new();
    let os_filter = os_filter.filter(|o| *o != FILTER_ALL);

    for d in details {
        for f in &d.installers {
            if let Some(os) = &f.os {
                if !os.is_empty() {
                    os_set.insert(os.clone());
                }
            }
            let os_ok = match (os_filter, f.os.as_deref()) {
                (None, _) => true,
                (Some(want), Some(have)) => want == have,
                (Some(_), None) => false,
            };
            if os_ok {
                if let Some(lang) = &f.language {
                    if !lang.is_empty() {
                        lang_set.insert(lang.clone());
                    }
                }
            }
        }
    }
    (os_set.into_iter().collect(), lang_set.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gog::{DownloadFile, FileKind, GameDetails};

    fn file(os: &str, lang: &str) -> DownloadFile {
        DownloadFile {
            id: format!("{os}-{lang}"),
            name: format!("setup_{lang}.exe"),
            size: 100,
            os: Some(os.into()),
            language: Some(lang.into()),
            downlink: "https://example.test".into(),
            kind: FileKind::Installer,
        }
    }

    #[test]
    fn collect_languages_lists_all_for_os() {
        let details = vec![GameDetails {
            id: 1,
            title: "Multi".into(),
            image: None,
            installers: vec![
                file("windows", "English"),
                file("windows", "Deutsch"),
                file("windows", "français"),
                file("linux", "English"),
                file("linux", "Deutsch"),
            ],
            extras: vec![],
        }];
        let (_os, langs) = collect_filter_options(&details, Some("windows"));
        assert_eq!(langs, vec!["Deutsch", "English", "français"]);
        let (_os, langs_linux) = collect_filter_options(&details, Some("linux"));
        assert_eq!(langs_linux, vec!["Deutsch", "English"]);
        let (_os, langs_all) = collect_filter_options(&details, Some(FILTER_ALL));
        assert_eq!(langs_all.len(), 3);
    }
}
