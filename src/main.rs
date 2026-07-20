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
    install_panic_hook();

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

/// Write panics to `%APPDATA%\gog-conjure\gog-conjure\crash.log` (and stderr) so a
/// console-flash exit on Windows still leaves a breadcrumb.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".into());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Box<dyn Any>".into()
        };
        let body = format!("gog-conjure panic at {location}\n{payload}\n");
        eprintln!("{body}");
        if let Ok(dir) = config::config_dir() {
            let path = dir.join("crash.log");
            let _ = std::fs::write(&path, &body);
            eprintln!("panic details written to {}", path.display());
        }
        default_hook(info);
    }));
}
