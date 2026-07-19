//! Standalone GOG login window (child process only).
//!
//! GOG rejects localhost redirect URIs (`redirect_uri_mismatch`), so we cannot
//! use the SourceGit/Gitea browser+callback pattern. Instead we load the official
//! Galaxy redirect in a WebView and capture `code=` from the navigation URL.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebViewBuilder};

use super::{auth_url, extract_code, is_login_success_url};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub fn run_login_window(output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(output_path);

    let mut event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("GOG Login — gog-conjure")
        .with_inner_size(tao::dpi::LogicalSize::new(980.0, 720.0))
        .build(&event_loop)
        .context("create login window")?;

    let (code_tx, code_rx) = mpsc::channel::<String>();
    let auth = auth_url();

    let tx_nav = code_tx.clone();
    let tx_load = code_tx;

    let builder = WebViewBuilder::new()
        .with_url(&auth)
        .with_navigation_handler(move |nav_url| {
            if try_capture(&tx_nav, &nav_url) {
                // Cancel further navigation once we have the code.
                return false;
            }
            true
        })
        .with_on_page_load_handler(move |event, url| {
            // Some WebKit builds only expose the final URL on page load.
            if matches!(event, PageLoadEvent::Finished) {
                let _ = try_capture(&tx_load, &url);
            }
        });

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let _webview = builder.build(&window).context("create webview")?;

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| anyhow!("no gtk vbox for login webview"))?;
        builder.build_gtk(vbox).context("create gtk webview")?
    };

    let out = output_path.to_path_buf();
    let started = Instant::now();

    event_loop.run_return(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50));

        if started.elapsed() > LOGIN_TIMEOUT {
            *control_flow = ControlFlow::Exit;
            return;
        }

        if let Ok(code) = code_rx.try_recv() {
            let _ = std::fs::write(&out, code);
            *control_flow = ControlFlow::Exit;
            return;
        }

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });

    if output_path.is_file() {
        Ok(())
    } else {
        Err(anyhow!("login cancelled"))
    }
}

fn try_capture(tx: &mpsc::Sender<String>, url: &str) -> bool {
    if !is_login_success_url(url) {
        return false;
    }
    match extract_code(url) {
        Ok(code) => {
            let _ = tx.send(code);
            true
        }
        Err(_) => false,
    }
}
