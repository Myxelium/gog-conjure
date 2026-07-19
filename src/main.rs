mod app;
mod auth;
mod config;
mod disc;
mod download;
mod gog;
mod images;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    if let Some(flag) = args.next() {
        if flag == "--gog-login" {
            let Some(out) = args.next().map(PathBuf::from) else {
                eprintln!("usage: gog-conjure --gog-login <output-file>");
                return ExitCode::from(2);
            };
            return match auth::run_login_window(&out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("login helper failed: {err:#}");
                    ExitCode::from(1)
                }
            };
        }
    }

    if let Err(err) = run_app() {
        eprintln!("gog-conjure failed: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run_app() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("app icon PNG must be valid");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("gog-conjure")
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "gog-conjure",
        native_options,
        Box::new(|cc| Ok(Box::new(app::ConjureApp::new(cc)))),
    )
}
